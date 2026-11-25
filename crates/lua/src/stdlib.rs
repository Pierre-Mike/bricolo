//! The `bricolo` global: one table per capability, mirroring the WIT standard
//! library. Every call goes to the shared providers, which the router has
//! already wrapped, so rules and peers apply to scripts as to plugins.
//!
//! Scripts are trusted: no permission checks, `disk` may reach any path,
//! `shell` may run any program and `http` may reach any host.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bricolo_providers::CallError;
use bricolo_providers::app::AppInfo;
use bricolo_providers::audio::{PlayOptions, Sound};
use bricolo_providers::claude_code::{Session, SessionState};
use bricolo_providers::disk::expand;
use bricolo_providers::http::{Request, Response};
use bricolo_providers::log::Level;
use bricolo_providers::mic::{AudioFormat, RecordOptions};
use bricolo_providers::mouse::{MouseButton, Point, check_click_count};
use bricolo_providers::shell::Output;
use bricolo_providers::system::Power;
use bricolo_providers::tts::TtsStatus;
use bricolo_providers::ui::{Button, Panel, PanelKind, Switch, Tile, Widget, WidgetRow};
use bricolo_providers::window::{Rect, Screen, WindowInfo};
use bricolo_providers::{AppChange, AppEvent, FileChange, FileEvent, SystemEvent};
use mlua::{
    BString, FromLua, FromLuaMulti, Function, IntoLua, IntoLuaMulti, Lua, MultiValue, Nil, Table,
    Value,
};

use crate::sandbox::CALLBACKS;
use crate::script::Ctx;

/// Fills `bricolo.<name>` with its functions.
type Install = fn(&Lua, &Table, &Arc<Ctx>) -> mlua::Result<()>;

/// One entry per capability, named like its WIT interface. To add a
/// capability, write an `Install` function below that registers each call
/// with [`sync_fn`] or [`async_fn`], and list it here.
const CAPABILITIES: &[(&str, Install)] = &[
    ("log", log),
    ("timer", timer),
    ("kv", kv),
    ("disk", disk),
    ("tts", tts),
    ("speech", speech),
    ("hotkey", hotkey),
    ("clipboard", clipboard),
    ("window", window),
    ("app", app),
    ("notify", notify),
    ("shell", shell),
    ("http", http),
    ("ui", ui),
    ("keyboard", keyboard),
    ("mic", mic),
    ("audio", audio),
    ("system", system),
    ("files", files),
    ("mouse", mouse),
    ("screen", screen),
    ("selection", selection),
    ("camera", camera),
    ("secrets", secrets),
    ("claude_code", claude_code),
    ("accessibility", accessibility),
    ("process", process),
    ("device", device),
    ("scripting", scripting),
    ("virtual_screen", virtual_screen),
];

/// Sets the `bricolo` and `print` globals.
pub fn install(lua: &Lua, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    let bricolo = lua.create_table()?;
    for (name, install) in CAPABILITIES {
        let table = lua.create_table()?;
        install(lua, &table, ctx)?;
        bricolo.set(*name, table)?;
    }
    bricolo.set("script", ctx.name.as_str())?;
    bricolo.set("os", lua.create_function(|_, ()| Ok(os_name()))?)?;
    lua.globals().set("bricolo", bricolo)?;
    let c = ctx.clone();
    let print = lua.create_function(move |_, args: MultiValue| {
        let message = join(args, "\t")?;
        c.providers
            .log
            .write(&c.caller.plugin, Level::Info, &message);
        Ok(())
    })?;
    lua.globals().set("print", print)
}

/// What `bricolo.os()` answers: "macos", "linux", "windows"…
fn os_name() -> &'static str {
    std::env::consts::OS
}

/// The `err` string scripts see: the `call-error` case name.
pub fn error_name(e: &CallError) -> String {
    match e {
        CallError::Unavailable => "unavailable".into(),
        CallError::Timeout => "timeout".into(),
        CallError::Denied => "denied".into(),
        CallError::Failed(msg) => format!("failed: {msg}"),
    }
}

/// `value` on success, `nil, err` on failure.
fn answer<R: IntoLua>(lua: &Lua, result: Result<R, CallError>) -> mlua::Result<MultiValue> {
    match result {
        Ok(value) => value.into_lua_multi(lua),
        Err(e) => (Nil, error_name(&e)).into_lua_multi(lua),
    }
}

/// Registers a call that answers at once.
fn sync_fn<A, R, F>(lua: &Lua, t: &Table, name: &str, ctx: &Arc<Ctx>, f: F) -> mlua::Result<()>
where
    A: FromLuaMulti,
    R: IntoLua,
    F: Fn(&Ctx, A) -> Result<R, CallError> + Send + 'static,
{
    let ctx = ctx.clone();
    t.set(
        name,
        lua.create_function(move |lua, args: A| answer(lua, f(&ctx, args)))?,
    )
}

/// Registers a call that waits: the calling coroutine yields until the
/// provider answers, and other coroutines run meanwhile.
fn async_fn<A, R, F, Fut>(
    lua: &Lua,
    t: &Table,
    name: &str,
    ctx: &Arc<Ctx>,
    f: F,
) -> mlua::Result<()>
where
    A: FromLuaMulti,
    R: IntoLua,
    F: Fn(Arc<Ctx>, A) -> Fut + Send + 'static,
    Fut: Future<Output = Result<R, CallError>> + Send + 'static,
{
    let ctx = ctx.clone();
    t.set(
        name,
        lua.create_async_function(move |lua, args: A| {
            let call = f(ctx.clone(), args);
            async move { answer(&lua, call.await) }
        })?,
    )
}

fn callbacks(lua: &Lua) -> mlua::Result<Table> {
    lua.named_registry_value(CALLBACKS)
}

/// The arguments as text, like `print` shows them.
fn join(args: MultiValue, sep: &str) -> mlua::Result<String> {
    let parts: Vec<String> = args
        .iter()
        .map(Value::to_string)
        .collect::<Result<_, _>>()?;
    Ok(parts.join(sep))
}

fn log(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    let levels = [
        ("trace", Level::Trace),
        ("debug", Level::Debug),
        ("info", Level::Info),
        ("warn", Level::Warn),
        ("error", Level::Error),
    ];
    for (name, level) in levels {
        let c = ctx.clone();
        let f = lua.create_function(move |_, args: MultiValue| {
            let message = join(args, " ")?;
            c.providers.log.write(&c.caller.plugin, level, &message);
            Ok(())
        })?;
        t.set(name, f)?;
    }
    Ok(())
}

fn timer(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    // `after` callbacks are keyed `a<id>` and dropped once they fire;
    // `every` callbacks are keyed `e<id>`.
    for (name, prefix) in [("after", "a"), ("every", "e")] {
        let c = ctx.clone();
        let f = lua.create_function(move |lua, (ms, func): (u32, Function)| {
            let timers = &c.providers.timer;
            let id = match prefix {
                "a" => timers.after(&c.caller.plugin, ms),
                _ => timers.every(&c.caller.plugin, ms),
            };
            callbacks(lua)?.set(format!("{prefix}{id}"), func)?;
            Ok(id)
        })?;
        t.set(name, f)?;
    }
    let c = ctx.clone();
    let cancel = lua.create_function(move |lua, id: u64| {
        let cbs = callbacks(lua)?;
        cbs.set(format!("a{id}"), Nil)?;
        cbs.set(format!("e{id}"), Nil)?;
        Ok(c.providers.timer.cancel(&c.caller.plugin, id))
    })?;
    t.set("cancel", cancel)?;
    async_fn(lua, t, "sleep", ctx, |c, ms: u32| async move {
        c.providers.timer.sleep(ms).await;
        Ok(true)
    })
}

fn kv(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    sync_fn(lua, t, "get", ctx, |c, key: String| {
        Ok(c.providers.kv.get(&c.caller.plugin, &key))
    })?;
    sync_fn(lua, t, "set", ctx, |c, (key, value): (String, String)| {
        c.providers
            .kv
            .set(&c.caller.plugin, &key, &value)
            .map(|()| true)
    })?;
    sync_fn(lua, t, "delete", ctx, |c, key: String| {
        c.providers.kv.delete(&c.caller.plugin, &key).map(|()| true)
    })?;
    sync_fn(lua, t, "keys", ctx, |c, ()| {
        Ok(c.providers.kv.keys(&c.caller.plugin))
    })
}

fn disk(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(lua, t, "read", ctx, |c, path: String| async move {
        c.providers
            .disk
            .read(&c.caller, path)
            .await
            .map(BString::from)
    })?;
    async_fn(
        lua,
        t,
        "write",
        ctx,
        |c, (path, data): (String, BString)| async move {
            c.providers
                .disk
                .write(&c.caller, path, data.into())
                .await
                .map(|()| true)
        },
    )?;
    async_fn(lua, t, "list", ctx, |c, path: String| async move {
        c.providers.disk.list(&c.caller, path).await
    })
}

fn tts(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(
        lua,
        t,
        "speak",
        ctx,
        |c, (text, voice): (String, Option<String>)| async move {
            c.providers
                .tts
                .speak(&c.caller, text, voice)
                .await
                .map(|()| true)
        },
    )?;
    async_fn(lua, t, "stop", ctx, |c, ()| async move {
        c.providers.tts.stop(&c.caller).await.map(|()| true)
    })?;
    async_fn(lua, t, "status", ctx, |c, ()| async move {
        c.providers.tts.status(&c.caller).await.map(AsTable)
    })
}

fn speech(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    // The audio file is read on this node, even when `speech` runs on a
    // peer. `format` defaults to the file's extension.
    async_fn(
        lua,
        t,
        "transcribe",
        ctx,
        |c, (path, format): (String, Option<String>)| async move {
            let home = bricolo_providers::disk::home();
            let file = expand(&path, &home).unwrap_or_else(|| PathBuf::from(&path));
            let audio = tokio::fs::read(&file)
                .await
                .map_err(|e| CallError::Failed(format!("cannot read {path}: {e}")))?;
            let format = format.unwrap_or_else(|| {
                file.extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_else(|| "wav".into())
            });
            c.providers
                .speech
                .transcribe(&c.caller, audio, format)
                .await
        },
    )?;
    // The server reads the file itself, so on a peer this falls back to
    // reading it here and sending the bytes.
    async_fn(
        lua,
        t,
        "transcribe_file",
        ctx,
        |c, path: String| async move { c.providers.speech.transcribe_file(&c.caller, path).await },
    )?;
    async_fn(
        lua,
        t,
        "transcribe_audio",
        ctx,
        |c, (audio, format): (BString, String)| async move {
            c.providers
                .speech
                .transcribe(&c.caller, audio.into(), format)
                .await
        },
    )
}

/// `bind("cmd+alt+h", fn)`, or Hammerspoon's `bind({"cmd", "alt"}, "h", fn)`,
/// each with an optional function for the release after `fn`.
fn chord_args(args: MultiValue) -> mlua::Result<(String, Function, Option<Function>)> {
    let mut args = args.into_iter();
    let release = |v: Option<Value>| match v {
        Some(Value::Function(f)) => Ok(Some(f)),
        None | Some(Value::Nil) => Ok(None),
        Some(_) => Err(mlua::Error::runtime(
            "bricolo.hotkey.bind: the release handler must be a function",
        )),
    };
    match (args.next(), args.next(), args.next()) {
        (Some(Value::String(chord)), Some(Value::Function(f)), up) => {
            Ok((chord.to_str()?.to_string(), f, release(up)?))
        }
        (Some(Value::Table(mods)), Some(Value::String(key)), Some(Value::Function(f))) => {
            let mut parts: Vec<String> = mods.sequence_values().collect::<mlua::Result<_>>()?;
            parts.push(key.to_str()?.to_string());
            Ok((parts.join("+"), f, release(args.next())?))
        }
        _ => Err(mlua::Error::runtime(
            "bricolo.hotkey.bind takes (chord, fn[, on_release]) or (mods, key, fn[, on_release])",
        )),
    }
}

/// The callbacks key of a hotkey's release handler.
pub(crate) fn release_key(id: &str) -> String {
    format!("{id}:up")
}

fn hotkey(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    let c = ctx.clone();
    let bind = lua.create_async_function(move |lua, args: MultiValue| {
        let c = c.clone();
        async move {
            let (chord, func, up) = chord_args(args)?;
            let id = format!("h{}", c.next_hotkey.fetch_add(1, Ordering::Relaxed));
            callbacks(&lua)?.set(id.as_str(), func)?;
            callbacks(&lua)?.set(release_key(&id), up)?;
            let result = c.providers.hotkey.bind(&c.caller, id.clone(), chord).await;
            if result.is_err() {
                callbacks(&lua)?.set(id.as_str(), Nil)?;
                callbacks(&lua)?.set(release_key(&id), Nil)?;
            }
            answer(&lua, result.map(|()| id))
        }
    })?;
    t.set("bind", bind)?;
    let c = ctx.clone();
    let unbind = lua.create_async_function(move |lua, id: String| {
        let c = c.clone();
        async move {
            callbacks(&lua)?.set(id.as_str(), Nil)?;
            callbacks(&lua)?.set(release_key(&id), Nil)?;
            let result = c.providers.hotkey.unbind(&c.caller, id).await;
            answer(&lua, result.map(|()| true))
        }
    })?;
    t.set("unbind", unbind)
}

fn clipboard(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(lua, t, "get", ctx, |c, ()| async move {
        c.providers.clipboard.get(&c.caller).await
    })?;
    async_fn(lua, t, "set", ctx, |c, text: String| async move {
        c.providers
            .clipboard
            .set(&c.caller, text)
            .await
            .map(|()| true)
    })?;
    async_fn(lua, t, "formats", ctx, |c, ()| async move {
        c.providers.clipboard.formats(&c.caller).await.map(AsJson)
    })?;
    async_fn(lua, t, "get_image", ctx, |c, ()| async move {
        c.providers.clipboard.get_image(&c.caller).await.map(AsJson)
    })?;
    async_fn(
        lua,
        t,
        "set_image",
        ctx,
        |c, (path,): (String,)| async move {
            c.providers
                .clipboard
                .set_image(&c.caller, path)
                .await
                .map(|()| true)
        },
    )?;
    async_fn(lua, t, "get_files", ctx, |c, ()| async move {
        c.providers.clipboard.get_files(&c.caller).await.map(AsJson)
    })?;
    async_fn(
        lua,
        t,
        "set_files",
        ctx,
        |c, (paths,): (Vec<String>,)| async move {
            c.providers
                .clipboard
                .set_files(&c.caller, paths)
                .await
                .map(|()| true)
        },
    )?;
    Ok(())
}

/// A provider value on its way into Lua as a table.
pub(crate) struct AsTable<T>(pub(crate) T);

fn table(lua: &Lua, fields: Vec<(&str, Value)>) -> mlua::Result<Value> {
    let t = lua.create_table()?;
    for (k, v) in fields {
        t.set(k, v)?;
    }
    Ok(Value::Table(t))
}

/// Frames use Hammerspoon's names: `x`, `y`, `w` and `h`.
impl IntoLua for AsTable<Rect> {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        let r = self.0;
        table(
            lua,
            vec![
                ("x", r.x.into_lua(lua)?),
                ("y", r.y.into_lua(lua)?),
                ("w", r.width.into_lua(lua)?),
                ("h", r.height.into_lua(lua)?),
            ],
        )
    }
}

/// A frame from Lua: `{x, y, w, h}`, or `width` and `height`.
struct FrameArg(Rect);

impl FromLua for FrameArg {
    fn from_lua(value: Value, _: &Lua) -> mlua::Result<Self> {
        let Value::Table(t) = value else {
            return Err(mlua::Error::runtime("a frame is a table {x=, y=, w=, h=}"));
        };
        let either = |a: &str, b: &str| -> mlua::Result<u32> {
            match t.get::<Option<u32>>(a)? {
                Some(v) => Ok(v),
                None => t.get::<u32>(b),
            }
        };
        Ok(FrameArg(Rect {
            x: t.get("x")?,
            y: t.get("y")?,
            width: either("w", "width")?,
            height: either("h", "height")?,
        }))
    }
}

impl IntoLua for AsTable<WindowInfo> {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        let w = self.0;
        table(
            lua,
            vec![
                ("id", w.id.into_lua(lua)?),
                ("app", w.app.into_lua(lua)?),
                ("title", w.title.into_lua(lua)?),
                ("frame", AsTable(w.frame).into_lua(lua)?),
            ],
        )
    }
}

impl IntoLua for AsTable<Screen> {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        let s = self.0;
        table(
            lua,
            vec![
                ("id", s.id.into_lua(lua)?),
                ("frame", AsTable(s.frame).into_lua(lua)?),
                ("visible_frame", AsTable(s.visible_frame).into_lua(lua)?),
                ("main", s.main.into_lua(lua)?),
            ],
        )
    }
}

impl IntoLua for AsTable<AppInfo> {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        let a = self.0;
        table(
            lua,
            vec![
                ("name", a.name.into_lua(lua)?),
                ("bundle_id", a.bundle_id.into_lua(lua)?),
                ("pid", a.pid.into_lua(lua)?),
            ],
        )
    }
}

impl IntoLua for AsTable<TtsStatus> {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        let s = self.0;
        table(
            lua,
            vec![
                ("speaking", s.speaking.into_lua(lua)?),
                ("queued", s.queued.into_lua(lua)?),
                ("voice", s.voice.into_lua(lua)?),
            ],
        )
    }
}

impl IntoLua for AsTable<Output> {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        let o = self.0;
        table(
            lua,
            vec![
                ("code", o.code.into_lua(lua)?),
                ("stdout", o.stdout.into_lua(lua)?),
                ("stderr", o.stderr.into_lua(lua)?),
            ],
        )
    }
}

/// Header names in lowercase; repeated headers joined with ", ".
impl IntoLua for AsTable<Response> {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        let r = self.0;
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        for (name, value) in r.headers {
            headers
                .entry(name.to_ascii_lowercase())
                .and_modify(|v| {
                    v.push_str(", ");
                    v.push_str(&value);
                })
                .or_insert(value);
        }
        table(
            lua,
            vec![
                ("status", r.status.into_lua(lua)?),
                ("headers", headers.into_lua(lua)?),
                ("body", BString::from(r.body).into_lua(lua)?),
            ],
        )
    }
}

/// `request(url)` for a GET, or `request{url=, method=, headers=, body=,
/// timeout_ms=}`.
struct RequestArg(Request);

impl FromLua for RequestArg {
    fn from_lua(value: Value, _: &Lua) -> mlua::Result<Self> {
        let t = match value {
            Value::String(url) => {
                return Ok(RequestArg(Request {
                    method: "GET".into(),
                    url: url.to_str()?.to_string(),
                    headers: Vec::new(),
                    body: Vec::new(),
                    timeout: None,
                }));
            }
            Value::Table(t) => t,
            _ => {
                return Err(mlua::Error::runtime(
                    "bricolo.http.request takes a URL or a table {url=, method=, headers=, body=}",
                ));
            }
        };
        let headers: BTreeMap<String, String> = t.get::<Option<_>>("headers")?.unwrap_or_default();
        Ok(RequestArg(Request {
            method: t
                .get::<Option<String>>("method")?
                .unwrap_or_else(|| "GET".into()),
            url: t.get("url")?,
            headers: headers.into_iter().collect(),
            body: t
                .get::<Option<BString>>("body")?
                .map(Vec::from)
                .unwrap_or_default(),
            timeout: t
                .get::<Option<u64>>("timeout_ms")?
                .map(Duration::from_millis),
        }))
    }
}

fn window(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(lua, t, "list", ctx, |c, ()| async move {
        let list = c.providers.window.list(&c.caller).await?;
        Ok(list.into_iter().map(AsTable).collect::<Vec<_>>())
    })?;
    async_fn(lua, t, "focused", ctx, |c, ()| async move {
        Ok(c.providers.window.focused(&c.caller).await?.map(AsTable))
    })?;
    async_fn(lua, t, "focus", ctx, |c, id: u64| async move {
        c.providers.window.focus(&c.caller, id).await.map(|()| true)
    })?;
    async_fn(
        lua,
        t,
        "set_frame",
        ctx,
        |c, (id, frame): (u64, FrameArg)| async move {
            c.providers
                .window
                .set_frame(&c.caller, id, frame.0)
                .await
                .map(|()| true)
        },
    )?;
    async_fn(lua, t, "close", ctx, |c, id: u64| async move {
        c.providers.window.close(&c.caller, id).await.map(|()| true)
    })?;
    async_fn(
        lua,
        t,
        "set_minimized",
        ctx,
        |c, (id, minimized): (u64, bool)| async move {
            c.providers
                .window
                .set_minimized(&c.caller, id, minimized)
                .await
                .map(|()| true)
        },
    )?;
    async_fn(lua, t, "screens", ctx, |c, ()| async move {
        let list = c.providers.window.screens(&c.caller).await?;
        Ok(list.into_iter().map(AsTable).collect::<Vec<_>>())
    })
}

fn app(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(lua, t, "launch", ctx, |c, app: String| async move {
        c.providers.app.launch(&c.caller, app).await.map(|()| true)
    })?;
    async_fn(lua, t, "open_url", ctx, |c, url: String| async move {
        c.providers
            .app
            .open_url(&c.caller, url)
            .await
            .map(|()| true)
    })?;
    async_fn(lua, t, "running", ctx, |c, ()| async move {
        let list = c.providers.app.running(&c.caller).await?;
        Ok(list.into_iter().map(AsTable).collect::<Vec<_>>())
    })?;
    async_fn(lua, t, "frontmost", ctx, |c, ()| async move {
        Ok(c.providers.app.frontmost(&c.caller).await?.map(AsTable))
    })?;
    async_fn(lua, t, "quit", ctx, |c, app: String| async move {
        c.providers.app.quit(&c.caller, app).await.map(|()| true)
    })?;
    async_fn(
        lua,
        t,
        "set_hidden",
        ctx,
        |c, (app, hidden): (String, bool)| async move {
            c.providers
                .app
                .set_hidden(&c.caller, app, hidden)
                .await
                .map(|()| true)
        },
    )?;
    watch_fn(lua, t, ctx, APP_WATCH, |c, on| async move {
        c.providers.app.watch(&c.caller, on).await
    })
}

/// The callbacks keys of the `app` and `system` watchers.
pub(crate) const APP_WATCH: &str = "app:watch";
pub(crate) const SYSTEM_WATCH: &str = "system:watch";

/// The callbacks key of one `files.watch`.
pub(crate) fn file_key(id: u64) -> String {
    format!("file:{id}")
}

/// `watch(fn)` starts events and keeps `fn` under `key`; `watch(nil)`
/// stops them. A failed start keeps nothing.
fn watch_fn<F, Fut>(
    lua: &Lua,
    t: &Table,
    ctx: &Arc<Ctx>,
    key: &'static str,
    start: F,
) -> mlua::Result<()>
where
    F: Fn(Arc<Ctx>, bool) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), CallError>> + Send + 'static,
{
    let c = ctx.clone();
    let watch = lua.create_async_function(move |lua, func: Option<Function>| {
        let call = start(c.clone(), func.is_some());
        async move {
            let previous: Value = callbacks(&lua)?.get(key)?;
            callbacks(&lua)?.set(key, func)?;
            let result = call.await;
            if result.is_err() {
                callbacks(&lua)?.set(key, previous)?;
            }
            answer(&lua, result.map(|()| true))
        }
    })?;
    t.set("watch", watch)
}

impl IntoLua for AsTable<AppEvent> {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        let AppEvent { change, app } = self.0;
        let change = match change {
            AppChange::Launched => "launched",
            AppChange::Terminated => "terminated",
            AppChange::Activated => "activated",
        };
        table(
            lua,
            vec![
                ("change", change.into_lua(lua)?),
                ("name", app.name.into_lua(lua)?),
                ("bundle_id", app.bundle_id.into_lua(lua)?),
                ("pid", app.pid.into_lua(lua)?),
            ],
        )
    }
}

/// The name a `system.watch` callback receives.
pub(crate) fn system_event_name(e: SystemEvent) -> &'static str {
    match e {
        SystemEvent::WillSleep => "will_sleep",
        SystemEvent::DidWake => "did_wake",
        SystemEvent::ScreenLocked => "screen_locked",
        SystemEvent::ScreenUnlocked => "screen_unlocked",
        SystemEvent::ScreensChanged => "screens_changed",
    }
}

impl IntoLua for AsTable<FileEvent> {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        let e = self.0;
        let change = match e.change {
            FileChange::Created => "created",
            FileChange::Modified => "modified",
            FileChange::Removed => "removed",
        };
        table(
            lua,
            vec![
                ("watch", e.watch.into_lua(lua)?),
                ("path", e.path.into_lua(lua)?),
                ("change", change.into_lua(lua)?),
            ],
        )
    }
}

impl IntoLua for AsTable<Power> {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        let p = self.0;
        table(
            lua,
            vec![
                ("battery", p.battery.into_lua(lua)?),
                ("plugged_in", p.plugged_in.into_lua(lua)?),
            ],
        )
    }
}

fn system(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(lua, t, "idle_seconds", ctx, |c, ()| async move {
        c.providers.system.idle_seconds(&c.caller).await
    })?;
    async_fn(lua, t, "power", ctx, |c, ()| async move {
        Ok(AsTable(c.providers.system.power(&c.caller).await?))
    })?;
    async_fn(lua, t, "keep_awake", ctx, |c, on: bool| async move {
        c.providers
            .system
            .keep_awake(&c.caller, on)
            .await
            .map(|()| true)
    })?;
    watch_fn(lua, t, ctx, SYSTEM_WATCH, |c, on| async move {
        c.providers.system.watch(&c.caller, on).await
    })
}

fn files(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    let c = ctx.clone();
    let watch = lua.create_async_function(move |lua, (path, func): (String, Function)| {
        let c = c.clone();
        async move {
            let result = c.providers.files.watch(&c.caller, path).await;
            if let Ok(id) = &result {
                callbacks(&lua)?.set(file_key(*id), func)?;
            }
            answer(&lua, result)
        }
    })?;
    t.set("watch", watch)?;
    let c = ctx.clone();
    let unwatch = lua.create_async_function(move |lua, id: u64| {
        let c = c.clone();
        async move {
            callbacks(&lua)?.set(file_key(id), Nil)?;
            let result = c.providers.files.unwatch(&c.caller, id).await;
            answer(&lua, result.map(|()| true))
        }
    })?;
    t.set("unwatch", unwatch)
}

fn notify(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(
        lua,
        t,
        "show",
        ctx,
        |c, (title, body): (String, Option<String>)| async move {
            c.providers
                .notify
                .show(&c.caller, title, body.unwrap_or_default())
                .await
                .map(|()| true)
        },
    )
}

fn shell(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    // `run(program, args, env)`: no shell, an absolute program path, and
    // `env` as a table of names to values.
    async_fn(
        lua,
        t,
        "run",
        ctx,
        |c,
         (program, args, env): (
            String,
            Option<Vec<String>>,
            Option<BTreeMap<String, String>>,
        )| async move {
            c.providers
                .shell
                .run(
                    &c.caller,
                    program,
                    args.unwrap_or_default(),
                    env.unwrap_or_default().into_iter().collect(),
                )
                .await
                .map(AsTable)
        },
    )
}

fn http(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(
        lua,
        t,
        "request",
        ctx,
        |c, request: RequestArg| async move {
            c.providers
                .http
                .send(&c.caller, request.0)
                .await
                .map(AsTable)
        },
    )
}

/// Registry keys for `bricolo.ui` callbacks. Tile callbacks are one per
/// script; panel callbacks are keyed by panel id, and a choice list also
/// keeps its entries so the callback gets the picked label.
pub const UI_SWITCH: &str = "ui:switch";
pub const UI_BUTTON: &str = "ui:button";

pub fn ui_panel_key(id: &str) -> String {
    format!("ui:panel:{id}")
}

pub fn ui_choices_key(id: &str) -> String {
    format!("ui:choices:{id}")
}

/// `set_tile{title=, status=, switches={{id=, label=, on=}}, buttons={{id=,
/// label=}}, on_switch=fn(id, on), on_button=fn(id)}`.
struct TileArg {
    tile: Tile,
    on_switch: Option<Function>,
    on_button: Option<Function>,
}

impl FromLua for TileArg {
    fn from_lua(value: Value, _: &Lua) -> mlua::Result<Self> {
        let Value::Table(t) = value else {
            return Err(mlua::Error::runtime(
                "bricolo.ui.set_tile takes a table {title=, status=, switches=, buttons=}",
            ));
        };
        let mut switches = Vec::new();
        for entry in t.get::<Option<Vec<Table>>>("switches")?.unwrap_or_default() {
            let id: String = entry.get("id")?;
            switches.push(Switch {
                label: entry.get::<Option<String>>("label")?.unwrap_or(id.clone()),
                on: entry.get::<Option<bool>>("on")?.unwrap_or(false),
                id,
            });
        }
        let mut buttons = Vec::new();
        for entry in t.get::<Option<Vec<Table>>>("buttons")?.unwrap_or_default() {
            let id: String = entry.get("id")?;
            buttons.push(Button {
                label: entry.get::<Option<String>>("label")?.unwrap_or(id.clone()),
                id,
            });
        }
        let status = match t.get::<Option<String>>("status")? {
            Some(s) => s,
            None => t.get::<Option<String>>("status_text")?.unwrap_or_default(),
        };
        Ok(TileArg {
            tile: Tile {
                title: t.get::<Option<String>>("title")?.unwrap_or_default(),
                status_text: status,
                switches,
                buttons,
            },
            on_switch: t.get("on_switch")?,
            on_button: t.get("on_button")?,
        })
    }
}

/// `set_widget{title=, rows={{label=, value=}}}`.
struct WidgetArg(Widget);

impl FromLua for WidgetArg {
    fn from_lua(value: Value, _: &Lua) -> mlua::Result<Self> {
        let Value::Table(t) = value else {
            return Err(mlua::Error::runtime(
                "bricolo.ui.set_widget takes a table {title=, rows={{label=, value=}}}",
            ));
        };
        let mut rows = Vec::new();
        for entry in t.get::<Option<Vec<Table>>>("rows")?.unwrap_or_default() {
            rows.push(WidgetRow {
                label: entry.get::<Option<String>>("label")?.unwrap_or_default(),
                value: entry.get::<Option<String>>("value")?.unwrap_or_default(),
            });
        }
        Ok(WidgetArg(Widget {
            title: t.get::<Option<String>>("title")?.unwrap_or_default(),
            rows,
        }))
    }
}

/// `show_panel{kind=, title=, text=, choices=, timeout_ms=, on_choice=fn(i,
/// label), on_review=fn(accepted, text)}`.
struct PanelArg {
    panel: Panel,
    callback: Option<Function>,
}

impl FromLua for PanelArg {
    fn from_lua(value: Value, _: &Lua) -> mlua::Result<Self> {
        let Value::Table(t) = value else {
            return Err(mlua::Error::runtime(
                "bricolo.ui.show_panel takes a table {kind=, text=, ...}",
            ));
        };
        let kind: String = t.get("kind")?;
        let kind = match kind.as_str() {
            "caption" => PanelKind::Caption,
            "choice-list" | "choice_list" | "choices" => PanelKind::ChoiceList,
            "toast" => PanelKind::Toast,
            "review" => PanelKind::Review,
            other => {
                return Err(mlua::Error::runtime(format!(
                    "unknown panel kind {other:?}; use caption, choice_list, toast or review"
                )));
            }
        };
        let callback = match kind {
            PanelKind::ChoiceList => t.get("on_choice")?,
            PanelKind::Review => t.get("on_review")?,
            _ => None,
        };
        Ok(PanelArg {
            panel: Panel {
                kind,
                title: t.get::<Option<String>>("title")?.unwrap_or_default(),
                text: t.get::<Option<String>>("text")?.unwrap_or_default(),
                choices: t.get::<Option<Vec<String>>>("choices")?.unwrap_or_default(),
                timeout_ms: t.get("timeout_ms")?,
            },
            callback,
        })
    }
}

fn show(lua: &Lua, c: &Ctx, arg: PanelArg) -> mlua::Result<MultiValue> {
    let choices = arg.panel.choices.clone();
    let result = c.providers.ui.show_panel(&c.caller, arg.panel);
    if let Ok(id) = &result {
        let cbs = callbacks(lua)?;
        cbs.set(ui_panel_key(id), arg.callback)?;
        cbs.set(ui_choices_key(id), choices)?;
    }
    answer(lua, result)
}

fn ui(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    let c = ctx.clone();
    let set_tile = lua.create_function(move |lua, arg: TileArg| {
        let result = c.providers.ui.set_tile(&c.caller, arg.tile);
        if result.is_ok() {
            let cbs = callbacks(lua)?;
            cbs.set(UI_SWITCH, arg.on_switch)?;
            cbs.set(UI_BUTTON, arg.on_button)?;
        }
        answer(lua, result.map(|()| true))
    })?;
    t.set("set_tile", set_tile)?;
    let c = ctx.clone();
    let clear_tile = lua.create_function(move |lua, ()| {
        let cbs = callbacks(lua)?;
        cbs.set(UI_SWITCH, Nil)?;
        cbs.set(UI_BUTTON, Nil)?;
        answer(lua, c.providers.ui.clear_tile(&c.caller).map(|()| true))
    })?;
    t.set("clear_tile", clear_tile)?;
    let c = ctx.clone();
    let set_widget = lua.create_function(move |lua, arg: WidgetArg| {
        answer(
            lua,
            c.providers.ui.set_widget(&c.caller, arg.0).map(|()| true),
        )
    })?;
    t.set("set_widget", set_widget)?;
    let c = ctx.clone();
    let clear_widget = lua.create_function(move |lua, ()| {
        answer(lua, c.providers.ui.clear_widget(&c.caller).map(|()| true))
    })?;
    t.set("clear_widget", clear_widget)?;
    let c = ctx.clone();
    let show_panel = lua.create_function(move |lua, arg: PanelArg| show(lua, &c, arg))?;
    t.set("show_panel", show_panel)?;
    // Shortcuts: `caption(text)` and `toast(text, timeout_ms)`.
    for (name, kind) in [("caption", PanelKind::Caption), ("toast", PanelKind::Toast)] {
        let c = ctx.clone();
        let f = lua.create_function(move |lua, (text, timeout_ms): (String, Option<u32>)| {
            let panel = Panel {
                kind,
                title: String::new(),
                text,
                choices: Vec::new(),
                timeout_ms,
            };
            show(
                lua,
                &c,
                PanelArg {
                    panel,
                    callback: None,
                },
            )
        })?;
        t.set(name, f)?;
    }
    let c = ctx.clone();
    let hide_panel = lua.create_function(move |lua, id: String| {
        let cbs = callbacks(lua)?;
        cbs.set(ui_panel_key(&id), Nil)?;
        cbs.set(ui_choices_key(&id), Nil)?;
        answer(
            lua,
            c.providers.ui.hide_panel(&c.caller, &id).map(|()| true),
        )
    })?;
    t.set("hide_panel", hide_panel)
}

/// `keystroke("cmd+v")`, or `keystroke({"cmd"}, "v")` as the WIT call
/// takes it.
fn keystroke_args(args: MultiValue) -> mlua::Result<(Vec<String>, String)> {
    let mut args = args.into_iter();
    match (args.next(), args.next()) {
        (Some(Value::Table(mods)), Some(Value::String(key))) => {
            let mods = mods.sequence_values().collect::<mlua::Result<_>>()?;
            Ok((mods, key.to_str()?.to_string()))
        }
        (Some(Value::String(chord)), None) => {
            let chord = chord.to_str()?.to_string();
            // "cmd++" names the + key, as in hotkey chords.
            let (head, key) = match chord.strip_suffix("++") {
                Some(head) => (head, "+"),
                None => chord.rsplit_once('+').unwrap_or(("", chord.as_str())),
            };
            let mods = head
                .split('+')
                .filter(|m| !m.is_empty())
                .map(String::from)
                .collect();
            Ok((mods, key.to_string()))
        }
        _ => Err(mlua::Error::runtime(
            "bricolo.keyboard.keystroke takes (chord) or (mods, key)",
        )),
    }
}

fn secrets(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(lua, t, "get", ctx, |c, name: String| async move {
        c.providers.secrets.get(&c.caller, name).await
    })?;
    async_fn(
        lua,
        t,
        "set",
        ctx,
        |c, (name, value): (String, String)| async move {
            c.providers
                .secrets
                .set(&c.caller, name, value)
                .await
                .map(|()| true)
        },
    )?;
    async_fn(lua, t, "delete", ctx, |c, name: String| async move {
        c.providers
            .secrets
            .delete(&c.caller, name)
            .await
            .map(|()| true)
    })
}

/// `state` is the WIT case name: "working", "finished"…
impl IntoLua for AsTable<Session> {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        let s = self.0;
        let state = match s.state {
            SessionState::Idle => "idle",
            SessionState::Working => "working",
            SessionState::Waiting => "waiting",
            SessionState::Finished => "finished",
            SessionState::Failed => "failed",
        };
        table(
            lua,
            vec![
                ("id", s.id.into_lua(lua)?),
                ("dir", s.dir.into_lua(lua)?),
                ("state", state.into_lua(lua)?),
                ("last_result", s.last_result.into_lua(lua)?),
                ("title", s.title.into_lua(lua)?),
                ("started", s.started.into_lua(lua)?),
                ("updated", s.updated.into_lua(lua)?),
            ],
        )
    }
}

/// `bricolo.claude_code`, the `claude-code` interface.
fn claude_code(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(
        lua,
        t,
        "start",
        ctx,
        // `~/` expands on the node that runs the session.
        |c, (dir, prompt): (String, String)| async move {
            c.providers
                .claude_code
                .start(&c.caller, dir, prompt)
                .await
                .map(AsTable)
        },
    )?;
    async_fn(
        lua,
        t,
        "send",
        ctx,
        |c, (id, prompt): (String, String)| async move {
            c.providers
                .claude_code
                .send(&c.caller, id, prompt)
                .await
                .map(AsTable)
        },
    )?;
    async_fn(lua, t, "list", ctx, |c, ()| async move {
        let list = c.providers.claude_code.list(&c.caller).await?;
        Ok(list.into_iter().map(AsTable).collect::<Vec<_>>())
    })?;
    async_fn(lua, t, "status", ctx, |c, id: String| async move {
        c.providers
            .claude_code
            .status(&c.caller, id)
            .await
            .map(AsTable)
    })?;
    async_fn(lua, t, "stop", ctx, |c, id: String| async move {
        c.providers
            .claude_code
            .stop(&c.caller, id)
            .await
            .map(|()| true)
    })
}

fn keyboard(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    let c = ctx.clone();
    let keystroke = lua.create_async_function(move |lua, args: MultiValue| {
        let c = c.clone();
        async move {
            let (mods, key) = keystroke_args(args)?;
            let result = c.providers.keyboard.keystroke(&c.caller, mods, key).await;
            answer(&lua, result.map(|()| true))
        }
    })?;
    t.set("keystroke", keystroke)?;
    async_fn(lua, t, "type_text", ctx, |c, text: String| async move {
        c.providers
            .keyboard
            .type_text(&c.caller, text)
            .await
            .map(|()| true)
    })
}

/// A Lua number as a whole coordinate: rounded half away from zero. NaN and
/// values outside the `i32` range fail.
fn round_coord(v: f64) -> Result<i32, CallError> {
    let r = v.round();
    if r.is_finite() && r >= i32::MIN as f64 && r <= i32::MAX as f64 {
        Ok(r as i32)
    } else {
        Err(CallError::Failed("coordinate out of range".into()))
    }
}

/// `position()` answers `{x=, y=}`; `click([button[, count]])` defaults to
/// `"left"` and 1, and an unknown button name raises.
fn mouse(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    let c = ctx.clone();
    let position = lua.create_async_function(move |lua, ()| {
        let c = c.clone();
        async move {
            let result = c.providers.mouse.position(&c.caller).await;
            let result = result.and_then(|p| {
                let point = lua.create_table().map_err(CallError::failed)?;
                point.set("x", p.x).map_err(CallError::failed)?;
                point.set("y", p.y).map_err(CallError::failed)?;
                Ok(point)
            });
            answer(&lua, result)
        }
    })?;
    t.set("position", position)?;
    async_fn(lua, t, "move_to", ctx, |c, (x, y): (f64, f64)| async move {
        let (x, y) = (round_coord(x)?, round_coord(y)?);
        c.providers
            .mouse
            .move_to(&c.caller, Point { x, y })
            .await
            .map(|()| true)
    })?;
    let c = ctx.clone();
    let click = lua.create_async_function(
        move |lua, (button, count): (Option<String>, Option<u8>)| {
            let c = c.clone();
            async move {
                let name = button.as_deref().unwrap_or("left");
                let button = MouseButton::parse(name).ok_or_else(|| {
                    mlua::Error::runtime(format!(
                        "bricolo.mouse.click: button must be \"left\", \"right\" or \"middle\", not {name:?}"
                    ))
                })?;
                let count = count.unwrap_or(1);
                let result = match check_click_count(count) {
                    Ok(()) => c.providers.mouse.click(&c.caller, button, count).await,
                    Err(e) => Err(e),
                };
                answer(&lua, result.map(|()| true))
            }
        },
    )?;
    t.set("click", click)?;
    async_fn(
        lua,
        t,
        "scroll",
        ctx,
        |c, (dx, dy): (f64, f64)| async move {
            let (dx, dy) = (round_coord(dx)?, round_coord(dy)?);
            c.providers
                .mouse
                .scroll(&c.caller, dx, dy)
                .await
                .map(|()| true)
        },
    )
}

/// `capture([area])`: the whole main screen, or `{x, y, w, h}`.
fn screen(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(
        lua,
        t,
        "capture",
        ctx,
        |c, area: Option<FrameArg>| async move {
            let area = area.map(|a| a.0);
            if let Some(a) = &area {
                bricolo_providers::screen::check_area(a)?;
            }
            c.providers.screen.capture(&c.caller, area).await
        },
    )
}

fn selection(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(lua, t, "get_text", ctx, |c, ()| async move {
        c.providers.selection.get_text(&c.caller).await
    })
}

/// `record_start([{max_secs=, format=}])`. `format` may only be
/// "wav16k-mono".
struct RecordArg(RecordOptions);

impl FromLua for RecordArg {
    fn from_lua(value: Value, _: &Lua) -> mlua::Result<Self> {
        let t = match value {
            Value::Nil => return Ok(RecordArg(RecordOptions::default())),
            Value::Table(t) => t,
            _ => {
                return Err(mlua::Error::runtime(
                    "bricolo.mic.record_start takes nothing or a table {max_secs=, format=}",
                ));
            }
        };
        let format = t.get::<Option<String>>("format")?;
        if format.as_deref().is_some_and(|f| f != "wav16k-mono") {
            return Err(mlua::Error::runtime(
                "bricolo.mic.record_start: format must be \"wav16k-mono\"",
            ));
        }
        Ok(RecordArg(RecordOptions {
            max_secs: t.get::<Option<u32>>("max_secs")?.unwrap_or(0),
            format: AudioFormat::Wav16kMono,
        }))
    }
}

fn mic(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(
        lua,
        t,
        "record_start",
        ctx,
        |c, options: RecordArg| async move { c.providers.mic.record_start(&c.caller, options.0).await },
    )?;
    async_fn(lua, t, "record_stop", ctx, |c, id: u64| async move {
        c.providers.mic.record_stop(&c.caller, id).await
    })?;
    async_fn(lua, t, "record_cancel", ctx, |c, id: u64| async move {
        c.providers
            .mic
            .record_cancel(&c.caller, id)
            .await
            .map(|()| true)
    })
}

fn camera(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(lua, t, "snapshot", ctx, |c, ()| async move {
        c.providers.camera.snapshot(&c.caller).await
    })
}

/// A volume from Lua: any whole number, held to 0 to 100.
fn volume_arg(volume: i64) -> u8 {
    volume.clamp(0, 100) as u8
}

/// `play(path[, {volume=, looping=}])`. The volume defaults to 100.
struct PlayArg(PlayOptions);

impl FromLua for PlayArg {
    fn from_lua(value: Value, _: &Lua) -> mlua::Result<Self> {
        let t = match value {
            Value::Nil => return Ok(PlayArg(PlayOptions::default())),
            Value::Table(t) => t,
            _ => {
                return Err(mlua::Error::runtime(
                    "bricolo.audio.play takes a path and nothing or a table {volume=, looping=}",
                ));
            }
        };
        Ok(PlayArg(PlayOptions {
            volume: volume_arg(t.get::<Option<i64>>("volume")?.unwrap_or(100)),
            looping: t.get::<Option<bool>>("looping")?.unwrap_or(false),
        }))
    }
}

impl IntoLua for AsTable<Sound> {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        let s = self.0;
        table(
            lua,
            vec![
                ("id", s.id.into_lua(lua)?),
                ("path", s.path.into_lua(lua)?),
                ("volume", s.volume.into_lua(lua)?),
                ("looping", s.looping.into_lua(lua)?),
            ],
        )
    }
}

fn audio(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(
        lua,
        t,
        "play",
        ctx,
        |c, (path, options): (String, PlayArg)| async move {
            c.providers.audio.play(&c.caller, path, options.0).await
        },
    )?;
    async_fn(
        lua,
        t,
        "set_volume",
        ctx,
        |c, (id, volume): (u64, i64)| async move {
            c.providers
                .audio
                .set_volume(&c.caller, id, volume_arg(volume))
                .await
                .map(|()| true)
        },
    )?;
    async_fn(lua, t, "stop", ctx, |c, id: u64| async move {
        c.providers.audio.stop(&c.caller, id).await.map(|()| true)
    })?;
    async_fn(lua, t, "sounds", ctx, |c, ()| async move {
        let list = c.providers.audio.sounds(&c.caller).await?;
        Ok(list.into_iter().map(AsTable).collect::<Vec<_>>())
    })
}

/// A provider value on its way into Lua through its JSON form: objects
/// become tables with the same keys, and frames use `x`, `y`, `w`, `h`.
pub(crate) struct AsJson<T>(pub(crate) T);

impl<T: serde::Serialize> IntoLua for AsJson<T> {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        let v = serde_json::to_value(&self.0).map_err(mlua::Error::external)?;
        json_to_lua(lua, v)
    }
}

fn json_to_lua(lua: &Lua, v: serde_json::Value) -> mlua::Result<Value> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => Nil,
        J::Bool(b) => Value::Boolean(b),
        J::Number(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Number(n.as_f64().unwrap_or(0.0)),
        },
        J::String(s) => Value::String(lua.create_string(&s)?),
        J::Array(items) => {
            let t = lua.create_table()?;
            for (i, item) in items.into_iter().enumerate() {
                t.set(i + 1, json_to_lua(lua, item)?)?;
            }
            Value::Table(t)
        }
        J::Object(map) => {
            let rect = map.len() == 4
                && ["x", "y", "width", "height"]
                    .iter()
                    .all(|k| map.contains_key(*k));
            let t = lua.create_table()?;
            for (k, item) in map {
                let k = match (rect, k.as_str()) {
                    (true, "width") => "w".to_string(),
                    (true, "height") => "h".to_string(),
                    _ => k,
                };
                t.set(k, json_to_lua(lua, item)?)?;
            }
            Value::Table(t)
        }
    })
}

/// `bricolo.accessibility`, the `accessibility` interface.
fn accessibility(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(lua, t, "list_apps", ctx, |c, ()| async move {
        c.providers
            .accessibility
            .list_apps(&c.caller)
            .await
            .map(AsJson)
    })?;
    async_fn(
        lua,
        t,
        "list_windows",
        ctx,
        |c, (app,): (String,)| async move {
            c.providers
                .accessibility
                .list_windows(&c.caller, app)
                .await
                .map(AsJson)
        },
    )?;
    async_fn(
        lua,
        t,
        "snapshot",
        ctx,
        |c,
         (app, window, query, element, screenshot): (
            String,
            Option<u32>,
            Option<String>,
            Option<u32>,
            Option<bool>,
        )| async move {
            c.providers
                .accessibility
                .snapshot(&c.caller, app, window, query, element, screenshot)
                .await
                .map(AsJson)
        },
    )?;
    async_fn(
        lua,
        t,
        "find",
        ctx,
        |c, (app, text): (String, String)| async move {
            c.providers
                .accessibility
                .find(&c.caller, app, text)
                .await
                .map(AsJson)
        },
    )?;
    async_fn(
        lua,
        t,
        "click",
        ctx,
        |c, (app, element): (String, u32)| async move {
            c.providers
                .accessibility
                .click(&c.caller, app, element)
                .await
                .map(AsJson)
        },
    )?;
    async_fn(
        lua,
        t,
        "set_value",
        ctx,
        |c, (app, element, value): (String, u32, String)| async move {
            c.providers
                .accessibility
                .set_value(&c.caller, app, element, value)
                .await
                .map(AsJson)
        },
    )?;
    async_fn(
        lua,
        t,
        "type_text",
        ctx,
        |c, (app, element, text): (String, Option<u32>, String)| async move {
            c.providers
                .accessibility
                .type_text(&c.caller, app, element, text)
                .await
                .map(AsJson)
        },
    )?;
    async_fn(
        lua,
        t,
        "press_key",
        ctx,
        |c, (app, key): (String, String)| async move {
            c.providers
                .accessibility
                .press_key(&c.caller, app, key)
                .await
                .map(AsJson)
        },
    )?;
    async_fn(
        lua,
        t,
        "perform",
        ctx,
        |c, (app, element, action): (String, u32, String)| async move {
            c.providers
                .accessibility
                .perform(&c.caller, app, element, action)
                .await
                .map(AsJson)
        },
    )?;
    async_fn(
        lua,
        t,
        "menu",
        ctx,
        |c, (app, path): (String, Vec<String>)| async move {
            c.providers
                .accessibility
                .menu(&c.caller, app, path)
                .await
                .map(AsJson)
        },
    )?;
    async_fn(
        lua,
        t,
        "wait",
        ctx,
        |c, (app, text, timeout_ms): (String, String, Option<u32>)| async move {
            c.providers
                .accessibility
                .wait(&c.caller, app, text, timeout_ms)
                .await
                .map(AsJson)
        },
    )?;
    Ok(())
}

/// `bricolo.process`, the `process` interface.
fn process(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(lua, t, "list", ctx, |c, ()| async move {
        c.providers.process.list(&c.caller).await.map(AsJson)
    })?;
    async_fn(lua, t, "get", ctx, |c, (pid,): (u32,)| async move {
        c.providers.process.get(&c.caller, pid).await.map(AsJson)
    })?;
    async_fn(lua, t, "stop", ctx, |c, (pid,): (u32,)| async move {
        c.providers
            .process
            .stop(&c.caller, pid)
            .await
            .map(|()| true)
    })?;
    async_fn(lua, t, "kill", ctx, |c, (pid,): (u32,)| async move {
        c.providers
            .process
            .kill(&c.caller, pid)
            .await
            .map(|()| true)
    })?;
    Ok(())
}

/// `bricolo.device`, the `device` interface.
fn device(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(lua, t, "volume", ctx, |c, ()| async move {
        c.providers.device.volume(&c.caller).await.map(AsJson)
    })?;
    async_fn(lua, t, "muted", ctx, |c, ()| async move {
        c.providers.device.muted(&c.caller).await.map(AsJson)
    })?;
    async_fn(lua, t, "brightness", ctx, |c, ()| async move {
        c.providers.device.brightness(&c.caller).await.map(AsJson)
    })?;
    async_fn(lua, t, "wifi", ctx, |c, ()| async move {
        c.providers.device.wifi(&c.caller).await.map(AsJson)
    })?;
    async_fn(lua, t, "dark_mode", ctx, |c, ()| async move {
        c.providers.device.dark_mode(&c.caller).await.map(AsJson)
    })?;
    async_fn(lua, t, "focus_mode", ctx, |c, ()| async move {
        c.providers.device.focus_mode(&c.caller).await.map(AsJson)
    })?;
    async_fn(lua, t, "set_volume", ctx, |c, (level,): (u8,)| async move {
        c.providers
            .device
            .set_volume(&c.caller, level)
            .await
            .map(|()| true)
    })?;
    async_fn(
        lua,
        t,
        "set_muted",
        ctx,
        |c, (muted,): (bool,)| async move {
            c.providers
                .device
                .set_muted(&c.caller, muted)
                .await
                .map(|()| true)
        },
    )?;
    async_fn(
        lua,
        t,
        "set_brightness",
        ctx,
        |c, (level,): (u8,)| async move {
            c.providers
                .device
                .set_brightness(&c.caller, level)
                .await
                .map(|()| true)
        },
    )?;
    async_fn(lua, t, "set_wifi", ctx, |c, (on,): (bool,)| async move {
        c.providers
            .device
            .set_wifi(&c.caller, on)
            .await
            .map(|()| true)
    })?;
    async_fn(
        lua,
        t,
        "set_dark_mode",
        ctx,
        |c, (on,): (bool,)| async move {
            c.providers
                .device
                .set_dark_mode(&c.caller, on)
                .await
                .map(|()| true)
        },
    )?;
    async_fn(
        lua,
        t,
        "set_focus_mode",
        ctx,
        |c, (on,): (bool,)| async move {
            c.providers
                .device
                .set_focus_mode(&c.caller, on)
                .await
                .map(|()| true)
        },
    )?;
    async_fn(lua, t, "lock", ctx, |c, ()| async move {
        c.providers.device.lock(&c.caller).await.map(|()| true)
    })?;
    async_fn(lua, t, "sleep", ctx, |c, ()| async move {
        c.providers.device.sleep(&c.caller).await.map(|()| true)
    })?;
    Ok(())
}

/// `bricolo.scripting`, the `scripting` interface.
fn scripting(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(
        lua,
        t,
        "run_applescript",
        ctx,
        |c, (source,): (String,)| async move {
            c.providers
                .scripting
                .run_applescript(&c.caller, source)
                .await
                .map(AsJson)
        },
    )?;
    async_fn(
        lua,
        t,
        "run_jxa",
        ctx,
        |c, (source,): (String,)| async move {
            c.providers
                .scripting
                .run_jxa(&c.caller, source)
                .await
                .map(AsJson)
        },
    )?;
    async_fn(lua, t, "list_shortcuts", ctx, |c, ()| async move {
        c.providers
            .scripting
            .list_shortcuts(&c.caller)
            .await
            .map(AsJson)
    })?;
    async_fn(
        lua,
        t,
        "run_shortcut",
        ctx,
        |c, (name, input): (String, Option<String>)| async move {
            c.providers
                .scripting
                .run_shortcut(&c.caller, name, input)
                .await
                .map(AsJson)
        },
    )?;
    async_fn(
        lua,
        t,
        "run_powershell",
        ctx,
        |c, (source,): (String,)| async move {
            c.providers
                .scripting
                .run_powershell(&c.caller, source)
                .await
                .map(AsJson)
        },
    )?;
    Ok(())
}

/// `bricolo.virtual_screen`, the `virtual-screen` interface.
fn virtual_screen(lua: &Lua, t: &Table, ctx: &Arc<Ctx>) -> mlua::Result<()> {
    async_fn(
        lua,
        t,
        "create",
        ctx,
        |c, (width, height): (u32, u32)| async move {
            c.providers
                .virtual_screen
                .create(&c.caller, width, height)
                .await
                .map(AsJson)
        },
    )?;
    async_fn(lua, t, "list", ctx, |c, ()| async move {
        c.providers.virtual_screen.list(&c.caller).await.map(AsJson)
    })?;
    async_fn(
        lua,
        t,
        "launch",
        ctx,
        |c, (id, program, args): (u32, String, Vec<String>)| async move {
            c.providers
                .virtual_screen
                .launch(&c.caller, id, program, args)
                .await
                .map(AsJson)
        },
    )?;
    async_fn(lua, t, "capture", ctx, |c, (id,): (u32,)| async move {
        c.providers
            .virtual_screen
            .capture(&c.caller, id)
            .await
            .map(AsJson)
    })?;
    async_fn(
        lua,
        t,
        "click",
        ctx,
        |c, (id, x, y): (u32, i32, i32)| async move {
            c.providers
                .virtual_screen
                .click(&c.caller, id, x, y)
                .await
                .map(|()| true)
        },
    )?;
    async_fn(
        lua,
        t,
        "type_text",
        ctx,
        |c, (id, text): (u32, String)| async move {
            c.providers
                .virtual_screen
                .type_text(&c.caller, id, text)
                .await
                .map(|()| true)
        },
    )?;
    async_fn(
        lua,
        t,
        "press_key",
        ctx,
        |c, (id, key): (u32, String)| async move {
            c.providers
                .virtual_screen
                .press_key(&c.caller, id, key)
                .await
                .map(|()| true)
        },
    )?;
    async_fn(lua, t, "destroy", ctx, |c, (id,): (u32,)| async move {
        c.providers
            .virtual_screen
            .destroy(&c.caller, id)
            .await
            .map(|()| true)
    })?;
    Ok(())
}
