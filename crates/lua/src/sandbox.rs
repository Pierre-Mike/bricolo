//! A fresh Lua state for one script: Lua 5.4 without `io`, `debug`, C
//! modules or the parts of `os` that touch the system. `require`, `dofile`
//! and `loadfile` read Lua source from `scripts/` and nowhere else.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mlua::chunk::ChunkMode;
use mlua::{Function, HookTriggers, Lua, LuaOptions, MultiValue, Nil, StdLib, Table, Value};

use crate::script::Ctx;

/// Memory one script may allocate.
const MEMORY_LIMIT: usize = 256 << 20;
/// Instructions between two budget checks.
const HOOK_EVERY: u32 = 1000;
/// Registry table of callbacks by key: `a<id>` and `e<id>` for timers,
/// `h<n>` for hotkeys.
pub const CALLBACKS: &str = "bricolo.callbacks";
/// Registry copy of `package.loaded`, so replacing `package` cannot break
/// `require`.
const LOADED: &str = "bricolo.loaded";

/// `os` functions that change the system or leave the process.
const OS_REMOVED: [&str; 6] = [
    "execute",
    "exit",
    "remove",
    "rename",
    "tmpname",
    "setlocale",
];

/// Runs once in each new state, with mlua's "pending" marker as its
/// argument.
///
/// - `load` accepts source text only: precompiled chunks can crash the VM.
/// - A call that waits yields the marker up to the node, which resumes the
///   coroutine when the answer is ready. `coroutine.resume` and
///   `coroutine.wrap` pass the marker up from coroutines the script made,
///   so waiting calls work inside those too.
const PRELUDE: &str = r##"
local pending = ...
local load, select = load, select
_G.load = function(chunk, name, _, ...)
  if select("#", ...) > 0 then
    return load(chunk, name, "t", ...)
  end
  return load(chunk, name, "t")
end

local create, resume, yield, status = coroutine.create, coroutine.resume, coroutine.yield, coroutine.status
local pack, unpack = table.pack, table.unpack
local function resume_through(co, ...)
  local r = pack(resume(co, ...))
  while r[1] and r.n == 2 and r[2] == pending and status(co) == "suspended" do
    yield(pending)
    r = pack(resume(co))
  end
  return unpack(r, 1, r.n)
end
coroutine.resume = resume_through
coroutine.wrap = function(f)
  local co = create(f)
  return function(...)
    local r = pack(resume_through(co, ...))
    if not r[1] then
      error(r[2], 0)
    end
    return unpack(r, 2, r.n)
  end
end
"##;

pub fn new_state(ctx: &Arc<Ctx>) -> mlua::Result<Lua> {
    let libs = StdLib::COROUTINE
        | StdLib::TABLE
        | StdLib::STRING
        | StdLib::UTF8
        | StdLib::MATH
        | StdLib::OS;
    let lua = Lua::new_with(libs, LuaOptions::default())?;
    lua.set_memory_limit(MEMORY_LIMIT)?;
    let clock = ctx.clock.clone();
    lua.set_global_hook(
        HookTriggers::new().every_nth_instruction(HOOK_EVERY),
        move |_, _| clock.check(),
    )?;
    let g = lua.globals();
    let os: Table = g.get("os")?;
    for name in OS_REMOVED {
        os.set(name, Nil)?;
    }
    let loaded = lua.create_table()?;
    for lib in ["coroutine", "table", "string", "utf8", "math", "os"] {
        loaded.set(lib, g.get::<Value>(lib)?)?;
    }
    loaded.set("_G", g.clone())?;
    let package = lua.create_table()?;
    package.set("loaded", loaded.clone())?;
    g.set("package", package)?;
    lua.set_named_registry_value(LOADED, loaded)?;
    g.set("require", require(&lua, ctx)?)?;
    g.set("dofile", dofile(&lua, ctx)?)?;
    g.set("loadfile", loadfile(&lua, ctx)?)?;
    lua.load(PRELUDE)
        .set_name("=prelude")
        .call::<()>(Lua::poll_pending())?;
    lua.set_named_registry_value(CALLBACKS, lua.create_table()?)?;
    Ok(lua)
}

/// Compiles a script or module from source text.
pub fn compile(lua: &Lua, source: &[u8], chunk_name: &str) -> mlua::Result<Function> {
    lua.load(source)
        .set_name(format!("@{chunk_name}"))
        .set_mode(ChunkMode::Text)
        .into_function()
}

fn require(lua: &Lua, ctx: &Arc<Ctx>) -> mlua::Result<Function> {
    let ctx = ctx.clone();
    lua.create_function(move |lua, name: String| {
        let loaded: Table = lua.named_registry_value(LOADED)?;
        let cached: Value = loaded.get(name.as_str())?;
        if !cached.is_nil() {
            return Ok(cached);
        }
        let path = module_path(&ctx.dir, &name).map_err(mlua::Error::runtime)?;
        let module = load_file(lua, &ctx, &path)?;
        let value: Value = module.call(name.as_str())?;
        let value = if value.is_nil() {
            Value::Boolean(true)
        } else {
            value
        };
        loaded.set(name, value.clone())?;
        Ok(value)
    })
}

fn dofile(lua: &Lua, ctx: &Arc<Ctx>) -> mlua::Result<Function> {
    let ctx = ctx.clone();
    lua.create_function(move |lua, path: String| {
        let path = file_path(&ctx.dir, &path).map_err(mlua::Error::runtime)?;
        load_file(lua, &ctx, &path)?.call::<MultiValue>(())
    })
}

/// Like Lua's `loadfile`: the compiled chunk, or `nil, message`.
fn loadfile(lua: &Lua, ctx: &Arc<Ctx>) -> mlua::Result<Function> {
    let ctx = ctx.clone();
    lua.create_function(move |lua, path: String| {
        let loaded = file_path(&ctx.dir, &path)
            .map_err(mlua::Error::runtime)
            .and_then(|p| load_file(lua, &ctx, &p));
        match loaded {
            Ok(f) => Ok((Value::Function(f), Nil)),
            Err(e) => Ok((Nil, Value::String(lua.create_string(e.to_string())?))),
        }
    })
}

/// Reads and compiles a file inside `scripts/`, and records it so that a
/// change to it reloads this script.
fn load_file(lua: &Lua, ctx: &Ctx, path: &Path) -> mlua::Result<Function> {
    let source = std::fs::read(path)
        .map_err(|e| mlua::Error::runtime(format!("cannot read {}: {e}", path.display())))?;
    let rel = path.strip_prefix(&ctx.dir).unwrap_or(path);
    let chunk = compile(lua, &source, &rel.to_string_lossy());
    ctx.modules
        .lock()
        .unwrap()
        .insert(path.to_path_buf(), source);
    chunk
}

/// `require("lib.util")` finds `lib/util.lua`, then `lib/util/init.lua`,
/// under `dir` (the canonical `scripts/` folder).
pub fn module_path(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    };
    if !name.split('.').all(plain) {
        return Err(format!(
            "module {name:?} not allowed: use dotted names under scripts/, like \"lib.util\""
        ));
    }
    let rel: PathBuf = name.split('.').collect();
    let candidates = [
        dir.join(&rel).with_extension("lua"),
        dir.join(&rel).join("init.lua"),
    ];
    match candidates.iter().find(|p| p.is_file()) {
        Some(found) => inside(dir, found),
        None => Err(format!("module {name:?} not found in scripts/")),
    }
}

/// A path for `dofile` or `loadfile`: relative to `scripts/`, or absolute
/// but inside it.
pub fn file_path(dir: &Path, path: &str) -> Result<PathBuf, String> {
    inside(dir, &dir.join(path))
}

/// The real path of `path`, if it lies inside `dir`. Follows symlinks, so a
/// link in `scripts/` cannot reach outside it.
fn inside(dir: &Path, path: &Path) -> Result<PathBuf, String> {
    let real = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if real.starts_with(dir) {
        Ok(real)
    } else {
        Err(format!("{} is outside scripts/", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modules_resolve_inside_scripts_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let dir = root.join("scripts");
        std::fs::create_dir_all(dir.join("lib/pkg")).unwrap();
        std::fs::write(dir.join("lib/util.lua"), "").unwrap();
        std::fs::write(dir.join("lib/pkg/init.lua"), "").unwrap();
        std::fs::write(root.join("secret.lua"), "").unwrap();
        assert_eq!(
            module_path(&dir, "lib.util").unwrap(),
            dir.join("lib/util.lua")
        );
        assert_eq!(
            module_path(&dir, "lib.pkg").unwrap(),
            dir.join("lib/pkg/init.lua")
        );
        for bad in [
            "",
            "..secret",
            "lib..util",
            "/etc/passwd",
            "lib/util",
            "a b",
        ] {
            assert!(module_path(&dir, bad).is_err(), "{bad:?}");
        }
        assert!(module_path(&dir, "lib.missing").is_err());
        assert!(file_path(&dir, "../secret.lua").is_err());
        assert!(file_path(&dir, root.join("secret.lua").to_str().unwrap()).is_err());
        assert!(file_path(&dir, "lib/util.lua").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_out_of_scripts_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let dir = root.join("scripts");
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(root.join("secret.lua"), "").unwrap();
        std::os::unix::fs::symlink(root.join("secret.lua"), dir.join("lib/link.lua")).unwrap();
        let err = module_path(&dir, "lib.link").unwrap_err();
        assert!(err.contains("outside scripts/"), "{err}");
    }
}
