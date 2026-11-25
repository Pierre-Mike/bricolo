//! `bricolo update` and `bricolo install --from-lock`, both driven by
//! `plugins.lock`.

use crate::bundle;
use crate::engine::WasmEngine;
use crate::error::{Error, Result};
use crate::github::GitHub;
use crate::install::{self, Package};
use crate::lock::{Lock, sha256_hex};
use crate::manifest::Manifest;
use crate::paths::Paths;
use crate::permissions::Permissions;
use crate::source::{GitHubRef, Source, newer};

/// What `bricolo update` found for one lock entry from GitHub.
#[derive(Debug)]
pub struct Check {
    pub name: String,
    /// The locked source, `github:owner/repo@tag`.
    pub current: String,
    /// The latest release tag.
    pub latest: String,
    /// The newer release's package, checked, when there is one.
    pub update: Option<Package>,
    /// Capabilities the new version asks for that the installed one
    /// doesn't, and the other way round.
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// Capabilities not granted yet: those of the new version, or, with no
    /// update, those of the installed plugin still waiting for approval.
    pub ungranted: Vec<String>,
}

/// Looks for newer releases of the plugins and script bundles installed
/// from GitHub, or only of `only`. Entries from a path are skipped. One
/// entry failing doesn't stop the others.
pub fn check(
    engine: &WasmEngine,
    github: &GitHub,
    paths: &Paths,
    only: Option<&str>,
) -> Result<Vec<(String, Result<Check>)>> {
    let lock = Lock::load(&paths.lock_file())?;
    let entries = lock
        .plugins
        .iter()
        .map(|(n, e)| (n, &e.source))
        .chain(lock.scripts.iter().map(|(n, e)| (n, &e.source)));
    let mut found = false;
    let mut checks = Vec::new();
    for (name, source) in entries {
        if only.is_some_and(|o| o != name) {
            continue;
        }
        found = true;
        if !source.starts_with("github:") {
            continue;
        }
        checks.push((name.clone(), check_one(engine, github, paths, name, source)));
    }
    if let Some(name) = only
        && !found
    {
        return Err(Error::Invalid(format!(
            "{name} is not in plugins.lock; install it first"
        )));
    }
    Ok(checks)
}

fn check_one(
    engine: &WasmEngine,
    github: &GitHub,
    paths: &Paths,
    name: &str,
    source: &str,
) -> Result<Check> {
    let Source::GitHub(locked) = Source::parse(source)? else {
        unreachable!("only github: sources reach here");
    };
    let current_tag = locked.tag.clone().unwrap_or_default();
    let latest_release = github.release(&GitHubRef {
        tag: None,
        ..locked.clone()
    })?;
    let latest = latest_release.tag_name.clone();
    let installed = installed_manifest(paths, name);
    let permissions = Permissions::load(&paths.permissions_toml())?;
    let mut check = Check {
        name: name.to_string(),
        current: source.to_string(),
        latest: latest.clone(),
        update: None,
        added: Vec::new(),
        removed: Vec::new(),
        ungranted: installed
            .as_ref()
            .map(|m| permissions.missing(m))
            .unwrap_or_default(),
    };
    if !newer(&latest, &current_tag) {
        return Ok(check);
    }
    let mut resolved =
        install::resolve_release(engine, github, &locked, &latest_release, Some(name))?;
    let package = resolved.packages.remove(0);
    if let Package::Plugin(plan) = &package {
        let old = installed.map(|m| m.requested()).unwrap_or_default();
        check.added = diff(&plan.requested, &old);
        check.removed = diff(&old, &plan.requested);
        check.ungranted = permissions.missing(&plan.manifest);
    }
    check.update = Some(package);
    Ok(check)
}

fn diff(a: &[String], b: &[String]) -> Vec<String> {
    a.iter().filter(|x| !b.contains(x)).cloned().collect()
}

/// The manifest of the installed plugin `name`, if there is one.
pub fn installed_manifest(paths: &Paths, name: &str) -> Option<Manifest> {
    Manifest::load(&paths.plugin_dir(name).join("plugin.toml")).ok()
}

/// For each entry in `plugins.lock`: None when it is installed with the
/// locked hash, else its package from the locked source, whose hash must
/// match the lock. One entry failing doesn't stop the others.
pub fn restore(
    engine: &WasmEngine,
    github: &GitHub,
    paths: &Paths,
) -> Result<Vec<(String, Result<Option<Package>>)>> {
    let lock = Lock::load(&paths.lock_file())?;
    let mut out = Vec::new();
    for (name, entry) in &lock.plugins {
        let wasm = paths.plugin_dir(name).join("plugin.wasm");
        let present = std::fs::read(&wasm).is_ok_and(|b| sha256_hex(&b) == entry.sha256);
        let result = if present {
            Ok(None)
        } else {
            fetch_pinned(engine, github, name, &entry.source, &entry.sha256).map(Some)
        };
        out.push((name.clone(), result));
    }
    for (name, entry) in &lock.scripts {
        let dir = paths.scripts_dir();
        let files: Option<bundle::Files> = entry
            .files
            .iter()
            .map(|rel| std::fs::read(dir.join(rel)).ok().map(|b| (rel.clone(), b)))
            .collect();
        let present = files.is_some_and(|f| bundle::content_hash(&f) == entry.sha256);
        let result = if present {
            Ok(None)
        } else {
            fetch_pinned(engine, github, name, &entry.source, &entry.sha256).map(Some)
        };
        out.push((name.clone(), result));
    }
    Ok(out)
}

fn fetch_pinned(
    engine: &WasmEngine,
    github: &GitHub,
    name: &str,
    source: &str,
    sha256: &str,
) -> Result<Package> {
    let source = Source::parse(source)?;
    let mut resolved = install::resolve(engine, github, &source, Some(name))?;
    let package = resolved.packages.remove(0);
    if package.sha256() != sha256 {
        return Err(Error::Invalid(format!(
            "{name} from {source} has sha256 {}, but plugins.lock pins {sha256}",
            package.sha256()
        )));
    }
    Ok(package)
}
