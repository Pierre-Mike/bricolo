//! `bricolo install`, `bricolo check` and `bricolo uninstall`: validate a plugin or a set
//! of scripts, then copy it under `plugins/` or `scripts/` and record its
//! source and hash (and, for a plugin, its grants).
//!
//! A source is a built plugin folder, a `.lua` file, a folder of scripts,
//! a bundle file from `bricolo pack`, or a GitHub release (see [`resolve`]).

use std::path::{Path, PathBuf};

use bricolo_providers::kv::write_atomic;
use serde::Serialize;

use crate::bundle::{self, Files, PLUGIN_SUFFIX, SCRIPTS_SUFFIX};
use crate::engine::WasmEngine;
use crate::error::{Error, Result};
use crate::github::{GitHub, Release};
use crate::lock::{Lock, LockEntry, ScriptEntry, sha256_hex};
use crate::manifest::Manifest;
use crate::paths::Paths;
use crate::permissions::Permissions;
use crate::source::{GitHubRef, Source};

/// A checked plugin, ready to install.
#[derive(Debug, Serialize)]
pub struct Plan {
    pub name: String,
    pub version: String,
    pub source: String,
    pub sha256: String,
    /// Capabilities to approve: imports, then extra permissions.
    pub requested: Vec<String>,
    /// What the component actually imports.
    pub imports: Vec<String>,
    #[serde(skip)]
    pub manifest: Manifest,
    #[serde(skip)]
    pub wasm: Vec<u8>,
}

/// Scripts to copy into `scripts/`: one `.lua` file or a bundle.
#[derive(Debug, Serialize)]
pub struct ScriptPlan {
    /// The lock key: the file's or the bundle's name.
    pub name: String,
    pub version: String,
    pub source: String,
    pub sha256: String,
    /// The scripts that run: the `.lua` files at the bundle root.
    pub scripts: Vec<String>,
    #[serde(skip)]
    pub files: Files,
}

/// One thing a source holds. A command handles a few at most, so the
/// size difference between the variants doesn't matter.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Package {
    Plugin(Plan),
    Scripts(ScriptPlan),
}

impl Package {
    pub fn name(&self) -> &str {
        match self {
            Package::Plugin(p) => &p.name,
            Package::Scripts(s) => &s.name,
        }
    }

    pub fn sha256(&self) -> &str {
        match self {
            Package::Plugin(p) => &p.sha256,
            Package::Scripts(s) => &s.sha256,
        }
    }
}

/// What a source resolved to. A GitHub source carries the release tag.
#[derive(Debug)]
pub struct Resolved {
    pub source: Source,
    pub packages: Vec<Package>,
}

/// Finds the component: the manifest's `wasm` path (a fresh build), else
/// `plugin.wasm` in the folder (a prebuilt release).
pub fn wasm_path(src: &Path, manifest: &Manifest) -> Result<PathBuf> {
    let built = manifest.wasm.as_ref().map(|rel| src.join(rel));
    let candidates = built.iter().cloned().chain([src.join("plugin.wasm")]);
    if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
        return Ok(found);
    }
    Err(Error::Invalid(match built {
        Some(path) => format!(
            "{} does not exist; build the plugin first \
             (cargo build --target wasm32-wasip2 --release)",
            path.display()
        ),
        None => format!(
            "{} has no plugin.wasm and plugin.toml sets no wasm path",
            src.display()
        ),
    }))
}

/// Reads and checks the plugin in `src` without changing anything.
pub fn plan(engine: &WasmEngine, src: &Path) -> Result<Plan> {
    let manifest = Manifest::load(&src.join("plugin.toml"))?;
    let path = wasm_path(src, &manifest)?;
    let wasm = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
    let source = src.canonicalize().unwrap_or_else(|_| src.to_path_buf());
    plan_parts(engine, manifest, wasm, format!("path:{}", source.display()))
}

/// Checks a manifest and its component, from a folder or a bundle.
pub fn plan_parts(
    engine: &WasmEngine,
    manifest: Manifest,
    wasm: Vec<u8>,
    source: String,
) -> Result<Plan> {
    let checked = engine
        .check(&manifest, &wasm)
        .map_err(|e| Error::Invalid(format!("{}: {e}", manifest.name)))?;
    Ok(Plan {
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        source,
        sha256: sha256_hex(&wasm),
        requested: manifest.requested(),
        imports: checked.imports,
        manifest,
        wasm,
    })
}

pub fn script_plan(name: &str, version: &str, source: String, files: Files) -> Result<ScriptPlan> {
    if !bundle::valid_script_name(name) {
        return Err(Error::Invalid(format!(
            "{name:?} must be letters, digits, dashes and underscores"
        )));
    }
    let scripts = bundle::script_names(&files)?;
    Ok(ScriptPlan {
        name: name.to_string(),
        version: version.to_string(),
        source,
        sha256: bundle::content_hash(&files),
        scripts,
        files,
    })
}

/// Reads and checks everything `source` holds, without installing it.
/// `only` keeps the plugin or script bundle with that name.
///
/// - A folder with `plugin.toml` is a built plugin.
/// - Another folder is a scripts bundle named after the folder.
/// - `<name>.lua`, `<name>.rhplugin.tar.gz` and `<name>.rhscripts.tar.gz`
///   are a script, a plugin bundle and a scripts bundle.
/// - `github:owner/repo[@tag]` is every bundle in that release (the latest
///   without a tag). A release with plain `plugin.toml` and `plugin.wasm`
///   assets is one plugin.
pub fn resolve(
    engine: &WasmEngine,
    github: &GitHub,
    source: &Source,
    only: Option<&str>,
) -> Result<Resolved> {
    match source {
        Source::Path(path) => {
            let packages = resolve_path(engine, path)?
                .into_iter()
                .filter(|p| only.is_none_or(|o| p.name() == o))
                .collect::<Vec<_>>();
            if packages.is_empty() {
                return Err(Error::Invalid(format!(
                    "{} does not hold {}",
                    path.display(),
                    only.unwrap_or("anything to install")
                )));
            }
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            Ok(Resolved {
                source: Source::Path(canonical),
                packages,
            })
        }
        Source::GitHub(repo) => {
            let release = github.release(repo)?;
            resolve_release(engine, github, repo, &release, only)
        }
        Source::Peer { .. } => Err(Error::Invalid(format!(
            "{source} comes from another node; `bricolo install {source}` fetches it \
             through the running node's mesh"
        ))),
    }
}

fn resolve_path(engine: &WasmEngine, path: &Path) -> Result<Vec<Package>> {
    let canonical = path.canonicalize().map_err(|e| Error::io(path, e))?;
    let source = format!("path:{}", canonical.display());
    let file_name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if canonical.is_dir() {
        if canonical.join("plugin.toml").is_file() {
            return Ok(vec![Package::Plugin(plan(engine, &canonical)?)]);
        }
        let files = bundle::read_scripts_dir(&canonical)?;
        if files.is_empty() {
            return Err(Error::Invalid(format!(
                "{} has no plugin.toml and no .lua files",
                path.display()
            )));
        }
        let plan = script_plan(&file_name, "local", source, files)?;
        return Ok(vec![Package::Scripts(plan)]);
    }
    let read = || std::fs::read(&canonical).map_err(|e| Error::io(&canonical, e));
    if file_name.ends_with(PLUGIN_SUFFIX) {
        let (manifest, wasm) = bundle::plugin_parts(&bundle::unpack(&read()?)?)?;
        return Ok(vec![Package::Plugin(plan_parts(
            engine, manifest, wasm, source,
        )?)]);
    }
    if let Some(name) = file_name.strip_suffix(SCRIPTS_SUFFIX) {
        let files = bundle::unpack(&read()?)?;
        return Ok(vec![Package::Scripts(script_plan(
            name, "local", source, files,
        )?)]);
    }
    if let Some(name) = file_name.strip_suffix(".lua") {
        let files = Files::from([(file_name.clone(), read()?)]);
        return Ok(vec![Package::Scripts(script_plan(
            name, "local", source, files,
        )?)]);
    }
    Err(Error::Invalid(format!(
        "{} is not a plugin folder, a scripts folder, a .lua file or a bundle \
         ({PLUGIN_SUFFIX}, {SCRIPTS_SUFFIX})",
        path.display()
    )))
}

/// The packages in one release. Each bundle is downloaded and checked.
pub fn resolve_release(
    engine: &WasmEngine,
    github: &GitHub,
    repo: &GitHubRef,
    release: &Release,
    only: Option<&str>,
) -> Result<Resolved> {
    let pinned = repo.at(&release.tag_name);
    let source = pinned.to_string();
    let wanted = |name: &str| only.is_none_or(|o| o == name);
    let mut packages = Vec::new();
    for asset in &release.assets {
        if let Some(name) = asset.name.strip_suffix(PLUGIN_SUFFIX) {
            if !wanted(name) {
                continue;
            }
            let files = bundle::unpack(&github.download(asset)?)?;
            let (manifest, wasm) = bundle::plugin_parts(&files)?;
            if manifest.name != name {
                return Err(Error::Invalid(format!(
                    "{} holds plugin {:?}; the asset must be named {}{PLUGIN_SUFFIX}",
                    asset.name, manifest.name, manifest.name
                )));
            }
            let plan = plan_parts(engine, manifest, wasm, source.clone())?;
            packages.push(Package::Plugin(plan));
        } else if let Some(name) = asset.name.strip_suffix(SCRIPTS_SUFFIX) {
            if !wanted(name) {
                continue;
            }
            let files = bundle::unpack(&github.download(asset)?)?;
            let plan = script_plan(name, &release.tag_name, source.clone(), files)?;
            packages.push(Package::Scripts(plan));
        }
    }
    if packages.is_empty()
        && let (Some(toml), Some(wasm)) =
            (release.asset("plugin.toml"), release.asset("plugin.wasm"))
    {
        let text = String::from_utf8(github.download(toml)?)
            .map_err(|_| Error::Invalid("plugin.toml is not UTF-8".into()))?;
        let manifest =
            Manifest::parse(&text).map_err(|e| Error::Invalid(format!("plugin.toml: {e}")))?;
        if wanted(&manifest.name) {
            let plan = plan_parts(engine, manifest, github.download(wasm)?, source.clone())?;
            packages.push(Package::Plugin(plan));
        }
    }
    if packages.is_empty() {
        let names: Vec<&str> = release.assets.iter().map(|a| a.name.as_str()).collect();
        let what = match only {
            Some(name) => format!("{name}{PLUGIN_SUFFIX} or {name}{SCRIPTS_SUFFIX}"),
            None => format!("a {PLUGIN_SUFFIX} or {SCRIPTS_SUFFIX} asset"),
        };
        return Err(Error::Fetch(format!(
            "{source} has no {what}; its assets: {}",
            if names.is_empty() {
                "none".into()
            } else {
                names.join(", ")
            }
        )));
    }
    Ok(Resolved {
        source: Source::GitHub(pinned),
        packages,
    })
}

/// Capabilities in `plan` that `permissions.toml` does not grant yet.
pub fn new_requests(paths: &Paths, plan: &Plan) -> Result<Vec<String>> {
    Ok(Permissions::load(&paths.permissions_toml())?.missing(&plan.manifest))
}

/// Records the hash and grants, then writes the plugin files. The lock goes
/// first, so a running node that sees the new `plugin.wasm` finds its hash.
pub fn apply(paths: &Paths, plan: &Plan) -> Result<PathBuf> {
    write_plugin(paths, plan, true)
}

/// Like [`apply`], but leaves `permissions.toml` alone. A plugin that asks
/// for something not granted yet is refused at start until you approve it.
pub fn apply_ungranted(paths: &Paths, plan: &Plan) -> Result<PathBuf> {
    write_plugin(paths, plan, false)
}

fn write_plugin(paths: &Paths, plan: &Plan, grant: bool) -> Result<PathBuf> {
    let mut lock = Lock::load(&paths.lock_file())?;
    lock.plugins.insert(
        plan.name.clone(),
        LockEntry {
            source: plan.source.clone(),
            version: plan.version.clone(),
            sha256: plan.sha256.clone(),
        },
    );
    lock.save(&paths.lock_file())?;
    if grant {
        grant_all(paths, &plan.manifest)?;
    }
    let dir = paths.plugin_dir(&plan.name);
    for (rel, bytes) in bundle::plugin_files(&plan.manifest, &plan.wasm)? {
        let path = dir.join(rel);
        write_atomic(&path, &bytes).map_err(|e| Error::io(&path, e))?;
    }
    Ok(dir)
}

/// Grants everything `manifest` asks for.
pub fn grant_all(paths: &Paths, manifest: &Manifest) -> Result<()> {
    let mut permissions = Permissions::load(&paths.permissions_toml())?;
    permissions.grant(manifest);
    permissions.save(&paths.permissions_toml())
}

/// Records the scripts in the lock, then writes them into `scripts/`,
/// replacing files with the same path. Files an earlier version of the
/// same bundle installed and this one lacks are removed. Returns the
/// paths written.
pub fn apply_scripts(paths: &Paths, plan: &ScriptPlan) -> Result<Vec<PathBuf>> {
    let dir = paths.scripts_dir();
    let mut lock = Lock::load(&paths.lock_file())?;
    let old = lock.scripts.insert(
        plan.name.clone(),
        ScriptEntry {
            source: plan.source.clone(),
            version: plan.version.clone(),
            sha256: plan.sha256.clone(),
            files: plan.files.keys().cloned().collect(),
        },
    );
    lock.save(&paths.lock_file())?;
    for gone in old.iter().flat_map(|e| &e.files) {
        if !plan.files.contains_key(gone) && bundle::safe_path(gone) {
            let _ = std::fs::remove_file(dir.join(gone));
        }
    }
    let mut written = Vec::new();
    for (rel, bytes) in &plan.files {
        let path = dir.join(rel);
        write_atomic(&path, bytes).map_err(|e| Error::io(&path, e))?;
        written.push(path);
    }
    Ok(written)
}

/// Removes the plugin folder, its lock entry and its grants. Its data
/// folder stays. Returns false if it was not installed.
pub fn uninstall(paths: &Paths, name: &str) -> Result<bool> {
    let dir = paths.plugin_dir(name);
    let existed = dir.exists();
    if existed {
        std::fs::remove_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    }
    let mut lock = Lock::load(&paths.lock_file())?;
    let locked = lock.plugins.remove(name).is_some();
    if locked {
        lock.save(&paths.lock_file())?;
    }
    let mut permissions = Permissions::load(&paths.permissions_toml())?;
    if permissions.plugins.remove(name).is_some() {
        permissions.save(&paths.permissions_toml())?;
    }
    Ok(existed || locked)
}

/// Removes the files `bricolo install` wrote for the script bundle `name`, and
/// its lock entry. Returns the files removed, or None if no bundle has
/// that name.
pub fn uninstall_scripts(paths: &Paths, name: &str) -> Result<Option<Vec<String>>> {
    let mut lock = Lock::load(&paths.lock_file())?;
    let Some(entry) = lock.scripts.remove(name) else {
        return Ok(None);
    };
    lock.save(&paths.lock_file())?;
    let dir = paths.scripts_dir();
    let removed = entry
        .files
        .into_iter()
        .filter(|rel| bundle::safe_path(rel) && std::fs::remove_file(dir.join(rel)).is_ok())
        .collect();
    Ok(Some(removed))
}
