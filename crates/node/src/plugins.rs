//! `bricolo install`, `bricolo update`, `bricolo pack`, `bricolo uninstall` and `bricolo check`.

use std::io::{BufRead, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use bricolo_runtime::Paths;
use bricolo_runtime::bundle::{self, PLUGIN_SUFFIX, SCRIPTS_SUFFIX};
use bricolo_runtime::engine::WasmEngine;
use bricolo_runtime::github::GitHub;
use bricolo_runtime::install::{self, Package, Plan, ScriptPlan};
use bricolo_runtime::source::Source;
use bricolo_runtime::update;

fn engine() -> Result<WasmEngine> {
    WasmEngine::new().map_err(anyhow::Error::msg)
}

/// A plugin folder prints its checked plan as JSON. A `.lua` file is
/// compiled without running it.
pub fn check(path: &Path) -> Result<()> {
    if path.extension().is_some_and(|e| e == "lua") {
        let source =
            std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        bricolo_lua::check_syntax(&source, &name).map_err(anyhow::Error::msg)?;
        println!("{} compiles", path.display());
        return Ok(());
    }
    let plan = install::plan(&engine()?, path)?;
    println!("{}", serde_json::to_string_pretty(&plan)?);
    Ok(())
}

/// `bricolo install <source>`: everything the source holds, asking to approve
/// each plugin's capabilities and each set of scripts unless `yes`.
pub fn install(paths: &Paths, target: &str, yes: bool) -> Result<ExitCode> {
    let source = Source::parse(target)?;
    if let Source::Peer { peer, sha256 } = &source {
        let package = from_peer(paths, peer, sha256)?;
        return Ok(if install_package(paths, &package, yes)? {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }
    let github = GitHub::from_env();
    if let Source::GitHub(repo) = &source {
        println!("fetching {repo}");
    }
    let resolved = install::resolve(&engine()?, &github, &source, None)?;
    if let Source::GitHub(_) = &source {
        println!("release {}", resolved.source);
    }
    let mut code = ExitCode::SUCCESS;
    for package in &resolved.packages {
        if !install_package(paths, package, yes)? {
            code = ExitCode::FAILURE;
        }
    }
    Ok(code)
}

/// Asks the running node to fetch the plugin from `peer` over the mesh,
/// then plans its install like any bundle, with `peer:` as its source.
fn from_peer(paths: &Paths, peer: &str, sha256: &str) -> Result<Package> {
    println!("fetching {sha256} from {peer}");
    let ask = serde_json::json!({ "cmd": "fetch-plugin", "peer": peer, "sha256": sha256 });
    let answer = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(crate::control::request(paths, &ask))
        .with_context(|| {
            format!(
                "fetching from a peer needs a running node; none answers on {}",
                paths.socket().display()
            )
        })?;
    if let Some(e) = answer["error"].as_str() {
        bail!("{e}");
    }
    let path = answer["path"]
        .as_str()
        .context("the node answered no path")?;
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {path}"))?;
    // The node checked the hash; check again, since the file sat on disk.
    crate::share::verify(&bytes, sha256).map_err(anyhow::Error::msg)?;
    let (manifest, wasm) = bundle::plugin_parts(&bundle::unpack(&bytes)?)?;
    let source = Source::Peer {
        peer: peer.to_string(),
        sha256: sha256.to_string(),
    };
    Ok(Package::Plugin(install::plan_parts(
        &engine()?,
        manifest,
        wasm,
        source.to_string(),
    )?))
}

/// Shows what `package` asks for, asks unless `yes`, installs it. Returns
/// false when you said no.
fn install_package(paths: &Paths, package: &Package, yes: bool) -> Result<bool> {
    match package {
        Package::Plugin(plan) => {
            let new = install::new_requests(paths, plan)?;
            print_requests(plan, &new);
            if !yes && !new.is_empty() && !confirm("Approve all?")? {
                eprintln!("cancelled; {} not installed", plan.name);
                return Ok(false);
            }
            let installed = install::apply(paths, plan)?;
            println!(
                "installed {} {} to {}",
                plan.name,
                plan.version,
                installed.display()
            );
        }
        Package::Scripts(plan) => {
            print_scripts(paths, plan);
            if !yes && !confirm("Install these scripts?")? {
                eprintln!("cancelled; {} not installed", plan.name);
                return Ok(false);
            }
            install::apply_scripts(paths, plan)?;
            println!(
                "installed {} to {}; a running node starts them now",
                plan.scripts
                    .iter()
                    .map(|s| format!("{s}.lua"))
                    .collect::<Vec<_>>()
                    .join(", "),
                paths.scripts_dir().display()
            );
        }
    }
    Ok(true)
}

fn print_requests(plan: &Plan, new: &[String]) {
    println!("{} {} asks for:", plan.name, plan.version);
    if plan.requested.is_empty() {
        println!("  nothing");
    }
    for cap in &plan.requested {
        let mark = if new.contains(cap) { "new" } else { "granted" };
        println!("  {cap:<32} {mark}");
    }
}

fn print_scripts(paths: &Paths, plan: &ScriptPlan) {
    println!("{} {} ({}):", plan.name, plan.version, plan.source);
    for (rel, bytes) in &plan.files {
        let kind = if rel.contains('/') {
            "module"
        } else {
            "script"
        };
        let existing = std::fs::read(paths.scripts_dir().join(rel)).ok();
        let mark = match existing {
            Some(old) if &old == bytes => "unchanged",
            Some(_) => "replaces your copy",
            None => "new",
        };
        println!("  {rel:<32} {kind:<7} {mark}");
    }
    println!(
        "warning: scripts are trusted. They run with full access to your files, \
         any program and any host, with no permission prompt. Read them first."
    );
}

fn confirm(question: &str) -> Result<bool> {
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

/// `bricolo install --from-lock`: everything in plugins.lock, at its pinned
/// hash, for a new device.
pub fn install_from_lock(paths: &Paths, yes: bool) -> Result<ExitCode> {
    let restored = update::restore(&engine()?, &GitHub::from_env(), paths)?;
    if restored.is_empty() {
        println!("plugins.lock lists nothing");
    }
    let mut code = ExitCode::SUCCESS;
    for (name, result) in restored {
        match result {
            Ok(None) => println!("{name}: already installed at the locked hash"),
            Ok(Some(package)) => {
                if !install_package(paths, &package, yes)? {
                    code = ExitCode::FAILURE;
                }
            }
            Err(e) => {
                // A plugin a peer shared comes back through the mesh.
                let locked = bricolo_runtime::lock::Lock::load(&paths.lock_file())?
                    .plugins
                    .get(&name)
                    .and_then(|entry| Source::parse(&entry.source).ok());
                let fetched = match locked {
                    Some(Source::Peer { peer, sha256 }) => Some(from_peer(paths, &peer, &sha256)),
                    _ => None,
                };
                match fetched {
                    Some(Ok(package)) => {
                        if !install_package(paths, &package, yes)? {
                            code = ExitCode::FAILURE;
                        }
                    }
                    Some(Err(e)) => {
                        eprintln!("error: {name}: {e:#}");
                        code = ExitCode::FAILURE;
                    }
                    None => {
                        eprintln!("error: {name}: {e}");
                        code = ExitCode::FAILURE;
                    }
                }
            }
        }
    }
    Ok(code)
}

/// `bricolo update [name]`: newer GitHub releases of what plugins.lock lists.
/// A plugin whose new version asks for a capability you haven't granted
/// installs, but stays off until you approve (`-y`, or yes at the prompt).
pub fn update(paths: &Paths, name: Option<&str>, yes: bool) -> Result<ExitCode> {
    let checks = update::check(&engine()?, &GitHub::from_env(), paths, name)?;
    if checks.is_empty() {
        println!("nothing in plugins.lock comes from GitHub");
    }
    let mut code = ExitCode::SUCCESS;
    for (name, result) in checks {
        let check = match result {
            Ok(check) => check,
            Err(e) => {
                eprintln!("error: {name}: {e}");
                code = ExitCode::FAILURE;
                continue;
            }
        };
        let Some(package) = &check.update else {
            if check.ungranted.is_empty() {
                println!("{name} is up to date ({})", check.current);
            } else if yes {
                approve(paths, &name)?;
                println!("{name}: approved {}", check.ungranted.join(", "));
            } else {
                println!(
                    "{name} waits for approval of {}; run `bricolo update {name} -y`",
                    check.ungranted.join(", ")
                );
            }
            continue;
        };
        println!("{name}: {} -> {}", check.current, check.latest);
        match package {
            Package::Scripts(plan) => {
                print_scripts(paths, plan);
                install::apply_scripts(paths, plan)?;
                println!("updated {name} to {}", check.latest);
            }
            Package::Plugin(plan) => {
                for cap in &check.added {
                    println!("  + {cap}");
                }
                for cap in &check.removed {
                    println!("  - {cap}");
                }
                if check.ungranted.is_empty() {
                    install::apply(paths, plan)?;
                    println!("updated {name} to {} ({})", check.latest, plan.version);
                } else if yes || confirm(&format!("Approve {}?", check.ungranted.join(", ")))? {
                    install::apply(paths, plan)?;
                    println!(
                        "updated {name} to {} ({}) and approved {}",
                        check.latest,
                        plan.version,
                        check.ungranted.join(", ")
                    );
                } else {
                    install::apply_ungranted(paths, plan)?;
                    println!(
                        "updated {name} to {}, but it stays off until you approve {}: \
                         bricolo update {name} -y",
                        check.latest,
                        check.ungranted.join(", ")
                    );
                    code = ExitCode::FAILURE;
                }
            }
        }
    }
    Ok(code)
}

/// Grants what the installed plugin `name` asks for.
fn approve(paths: &Paths, name: &str) -> Result<()> {
    let manifest = update::installed_manifest(paths, name)
        .with_context(|| format!("{name} is not installed"))?;
    install::grant_all(paths, &manifest)?;
    Ok(())
}

/// `bricolo pack <dir>`: a plugin folder becomes `<name>.rhplugin.tar.gz`, a
/// folder of Lua scripts `<folder>.rhscripts.tar.gz`, ready to attach to a
/// GitHub release.
pub fn pack(dir: &Path, out: &Path) -> Result<()> {
    let dir = dir
        .canonicalize()
        .with_context(|| format!("cannot open {}", dir.display()))?;
    let (file, files, tag) = if dir.join("plugin.toml").is_file() {
        let plan = install::plan(&engine()?, &dir)?;
        let files = bundle::plugin_files(&plan.manifest, &plan.wasm)?;
        (
            format!("{}{PLUGIN_SUFFIX}", plan.name),
            files,
            format!("v{}", plan.version),
        )
    } else {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !bundle::valid_script_name(&name) {
            bail!("the folder name {name:?} must be letters, digits, dashes and underscores");
        }
        let files = bundle::read_scripts_dir(&dir)?;
        if files.is_empty() {
            bail!("{} has no plugin.toml and no .lua files", dir.display());
        }
        bundle::script_names(&files)?;
        (format!("{name}{SCRIPTS_SUFFIX}"), files, "v1".into())
    };
    let bytes = bundle::pack(&files)?;
    std::fs::create_dir_all(out).with_context(|| format!("cannot create {}", out.display()))?;
    let path = out.join(&file);
    std::fs::write(&path, &bytes).with_context(|| format!("cannot write {}", path.display()))?;
    println!(
        "packed {} ({} KiB, {} files)",
        path.display(),
        bytes.len().div_ceil(1024),
        files.len()
    );
    println!("publish: gh release create {tag} {}", path.display());
    println!("install: bricolo install github:<owner>/<repo>@{tag}");
    Ok(())
}

pub fn uninstall(paths: &Paths, name: &str) -> Result<()> {
    if install::uninstall(paths, name)? {
        println!("uninstalled {name}");
    } else if let Some(removed) = install::uninstall_scripts(paths, name)? {
        println!("uninstalled {name}: removed {}", removed.join(", "));
    } else {
        println!("{name} is not installed");
    }
    Ok(())
}
