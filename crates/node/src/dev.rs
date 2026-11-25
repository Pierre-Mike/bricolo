//! `bricolo dev <dir>`: build the plugin for wasm32-wasip2 and install it each
//! time its sources change. A running node then reloads it. New
//! capabilities still need `bricolo install`, so approval stays explicit.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bricolo_runtime::Paths;
use bricolo_runtime::engine::WasmEngine;
use bricolo_runtime::install;
use notify::Watcher;
use tokio::sync::mpsc;

const SETTLE: Duration = Duration::from_millis(300);

pub async fn run(paths: Paths, dir: PathBuf) -> Result<()> {
    let dir = dir
        .canonicalize()
        .with_context(|| format!("cannot open {}", dir.display()))?;
    let engine = WasmEngine::new().map_err(anyhow::Error::msg)?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res
            && event.paths.iter().any(|p| is_source(p))
        {
            let _ = tx.send(());
        }
    })?;
    for path in [
        dir.join("src"),
        dir.join("Cargo.toml"),
        dir.join("plugin.toml"),
    ] {
        if path.exists() {
            watcher.watch(&path, notify::RecursiveMode::Recursive)?;
        }
    }
    println!("watching {}; Ctrl+C to stop", dir.display());
    loop {
        if let Err(e) = build_and_install(&paths, &engine, &dir) {
            eprintln!("error: {e:#}");
        }
        rx.recv().await.context("the watcher stopped")?;
        while tokio::time::timeout(SETTLE, rx.recv()).await.is_ok() {}
    }
}

fn is_source(path: &Path) -> bool {
    !path.components().any(|c| c.as_os_str() == "target")
}

fn build_and_install(paths: &Paths, engine: &WasmEngine, dir: &Path) -> Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "--target", "wasm32-wasip2", "--release"])
        .current_dir(dir)
        .status()
        .context("cannot run cargo")?;
    if !status.success() {
        bail!("build failed");
    }
    let plan = install::plan(engine, dir)?;
    let new = install::new_requests(paths, &plan)?;
    if !new.is_empty() {
        bail!(
            "{} asks for {}, which you have not approved; run `bricolo install {}`",
            plan.name,
            new.join(", "),
            dir.display()
        );
    }
    install::apply(paths, &plan)?;
    println!("installed {} ({})", plan.name, &plan.sha256[..12]);
    Ok(())
}
