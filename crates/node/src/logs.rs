//! Daily log files under `logs/`, `bricolo logs`, and the layer that copies the
//! node's own lines to the log feed the UI streams.

use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bricolo_providers::log::{Level, LogLine, feed};
use bricolo_runtime::Paths;
use tracing::field::{Field, Visit};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::Context as LayerContext;
use tracing_subscriber::prelude::*;

pub const PREFIX: &str = "bricolo.log";

/// Logs to stderr and to `logs/bricolo.log.<date>`. `BRICOLO_LOG` sets the filter.
/// Keep the guard alive until exit, or the last lines are lost.
pub fn init(paths: &Paths) -> Result<WorkerGuard> {
    let dir = paths.logs_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let (file, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::daily(&dir, PREFIX));
    let filter =
        || EnvFilter::try_from_env("BRICOLO_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                // No colour codes when launchd or systemd writes stderr to a file.
                .with_ansi(std::io::stderr().is_terminal())
                .with_filter(filter()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file)
                .with_filter(filter()),
        )
        .with(FeedLayer.with_filter(filter()))
        .init();
    Ok(guard)
}

/// Copies the node's own lines to the log feed. Plugin and script lines
/// (target `plugin`) are already there: the `log` provider pushes them.
/// A line about one plugin, script or sidecar is filed under it.
pub struct FeedLayer;

#[derive(Default)]
struct Fields {
    message: String,
    owner: Option<String>,
    rest: String,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record(field, format!("{value:?}"));
    }
}

impl Fields {
    fn record(&mut self, field: &Field, value: String) {
        match field.name() {
            "message" => self.message = value,
            "plugin" => self.owner = Some(value),
            "sidecar" => self.owner = Some(format!("sidecar:{value}")),
            name => {
                let _ = write!(self.rest, " {name}={value}");
            }
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for FeedLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _: LayerContext<'_, S>) {
        let meta = event.metadata();
        if meta.target() == "plugin" {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let level = match *meta.level() {
            tracing::Level::TRACE => Level::Trace,
            tracing::Level::DEBUG => Level::Debug,
            tracing::Level::INFO => Level::Info,
            tracing::Level::WARN => Level::Warn,
            tracing::Level::ERROR => Level::Error,
        };
        let owner = fields.owner.unwrap_or_else(|| "node".into());
        let message = fields.message + &fields.rest;
        feed().push(LogLine::now(&owner, level, &message));
    }
}

/// The newest daily file, if any.
fn latest(dir: &Path) -> Option<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(PREFIX))
        })
        .collect();
    // Names end in the date, so the last one sorts last.
    files.sort();
    files.pop()
}

pub fn print(paths: &Paths, plugin: Option<&str>, lines: usize) -> Result<()> {
    let Some(file) = latest(&paths.logs_dir()) else {
        eprintln!("no log files in {}", paths.logs_dir().display());
        return Ok(());
    };
    let text = std::fs::read_to_string(&file)
        .with_context(|| format!("cannot read {}", file.display()))?;
    let selected: Vec<&str> = text
        .lines()
        .filter(|l| plugin.is_none_or(|p| mentions(l, p)))
        .collect();
    for line in &selected[selected.len().saturating_sub(lines)..] {
        println!("{line}");
    }
    Ok(())
}

fn mentions(line: &str, plugin: &str) -> bool {
    line.contains(&format!("plugin={plugin} "))
        || line.contains(&format!("plugin=\"{plugin}\""))
        || line.ends_with(&format!("plugin={plugin}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_by_plugin_field() {
        assert!(mentions("INFO plugin: plugin=hello tick 3", "hello"));
        assert!(mentions(
            "INFO bricolo_runtime: started plugin=\"hello\"",
            "hello"
        ));
        assert!(!mentions("INFO plugin: plugin=hello2 tick", "hello"));
    }

    #[test]
    fn the_layer_files_node_lines_under_their_plugin() {
        let subscriber = tracing_subscriber::registry().with(FeedLayer);
        let (_, mut rx) = feed().subscribe();
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(plugin = %"layer-test", "refused: {}", "no lock");
            tracing::info!(sidecar = "layer-sc", port = 8765, "started");
            tracing::info!(target: "plugin", plugin = "layer-test", "skipped");
        });
        let mut got = Vec::new();
        while let Ok(line) = rx.try_recv() {
            if line.plugin.contains("layer-") {
                got.push((line.plugin, line.level, line.message));
            }
        }
        assert_eq!(
            got,
            [
                (
                    "layer-test".to_string(),
                    Level::Warn,
                    "refused: no lock".to_string()
                ),
                (
                    "sidecar:layer-sc".to_string(),
                    Level::Info,
                    "started port=8765".to_string()
                ),
            ]
        );
    }

    #[test]
    fn latest_picks_the_newest_day() {
        let dir = tempfile::tempdir().unwrap();
        for day in [
            "bricolo.log.2026-11-14",
            "bricolo.log.2026-11-16",
            "bricolo.log.2026-11-15",
            "other",
        ] {
            std::fs::write(dir.path().join(day), "").unwrap();
        }
        let file = latest(dir.path()).unwrap();
        assert!(file.ends_with("bricolo.log.2026-11-16"));
    }
}
