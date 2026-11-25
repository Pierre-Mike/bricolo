//! `log`: plugin lines go to `tracing` (the daily log file) and to a small
//! in-memory buffer that `bricolo status` and tests read. They also go to the
//! process-wide [`feed`], which the control socket streams to the UI; the
//! node adds its own lines there too.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::broadcast;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Serialize)]
pub struct LogLine {
    /// Milliseconds since the Unix epoch.
    pub ts_ms: u64,
    /// The plugin, the script as `<name>.lua`, or `node` for the node's
    /// own lines.
    pub plugin: String,
    pub level: Level,
    pub message: String,
}

impl LogLine {
    pub fn now(plugin: &str, level: Level, message: &str) -> Self {
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default();
        Self {
            ts_ms,
            plugin: plugin.to_string(),
            level,
            message: message.to_string(),
        }
    }
}

/// Lines the feed keeps for a new subscriber.
const FEED_KEEP: usize = 500;

/// Every log line of this process, for the UI: the last [`FEED_KEEP`] and
/// a broadcast of new ones.
pub struct LogFeed {
    recent: Mutex<VecDeque<LogLine>>,
    tx: broadcast::Sender<LogLine>,
}

impl LogFeed {
    fn new() -> Self {
        Self {
            recent: Mutex::default(),
            tx: broadcast::channel(1024).0,
        }
    }

    pub fn push(&self, line: LogLine) {
        let mut recent = self.recent.lock().unwrap();
        if recent.len() == FEED_KEEP {
            recent.pop_front();
        }
        recent.push_back(line.clone());
        // No subscribers is fine.
        let _ = self.tx.send(line);
    }

    /// The kept lines, oldest first, and a receiver for the lines after
    /// them. Taken together so no line falls between the two.
    pub fn subscribe(&self) -> (Vec<LogLine>, broadcast::Receiver<LogLine>) {
        let recent = self.recent.lock().unwrap();
        (recent.iter().cloned().collect(), self.tx.subscribe())
    }
}

/// The process-wide feed.
pub fn feed() -> &'static LogFeed {
    static FEED: OnceLock<LogFeed> = OnceLock::new();
    FEED.get_or_init(LogFeed::new)
}

pub trait LogProvider: Send + Sync {
    fn write(&self, plugin: &str, level: Level, message: &str);
    /// The last `limit` lines, oldest first, from one plugin or from all.
    fn recent(&self, plugin: Option<&str>, limit: usize) -> Vec<LogLine>;
}

/// Lines kept in memory, across all plugins.
const KEEP: usize = 1000;

#[derive(Default)]
pub struct TracingLog {
    recent: Mutex<VecDeque<LogLine>>,
}

impl LogProvider for TracingLog {
    fn write(&self, plugin: &str, level: Level, message: &str) {
        match level {
            Level::Trace => tracing::trace!(target: "plugin", plugin, "{message}"),
            Level::Debug => tracing::debug!(target: "plugin", plugin, "{message}"),
            Level::Info => tracing::info!(target: "plugin", plugin, "{message}"),
            Level::Warn => tracing::warn!(target: "plugin", plugin, "{message}"),
            Level::Error => tracing::error!(target: "plugin", plugin, "{message}"),
        }
        let line = LogLine::now(plugin, level, message);
        let mut recent = self.recent.lock().unwrap();
        if recent.len() == KEEP {
            recent.pop_front();
        }
        recent.push_back(line.clone());
        drop(recent);
        feed().push(line);
    }

    fn recent(&self, plugin: Option<&str>, limit: usize) -> Vec<LogLine> {
        let recent = self.recent.lock().unwrap();
        let mut lines: Vec<LogLine> = recent
            .iter()
            .rev()
            .filter(|l| plugin.is_none_or(|p| l.plugin == p))
            .take(limit)
            .cloned()
            .collect();
        lines.reverse();
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_filters_by_plugin_and_keeps_order() {
        let log = TracingLog::default();
        log.write("a", Level::Info, "one");
        log.write("b", Level::Warn, "two");
        log.write("a", Level::Error, "three");
        let a: Vec<_> = log
            .recent(Some("a"), 10)
            .into_iter()
            .map(|l| l.message)
            .collect();
        assert_eq!(a, ["one", "three"]);
        let last: Vec<_> = log.recent(None, 2).into_iter().map(|l| l.message).collect();
        assert_eq!(last, ["two", "three"]);
    }

    #[test]
    fn lines_reach_the_feed() {
        let log = TracingLog::default();
        let (_, mut rx) = feed().subscribe();
        log.write("feed-test", Level::Warn, "hello");
        let line = loop {
            let line = rx.try_recv().unwrap();
            if line.plugin == "feed-test" {
                break line;
            }
        };
        assert_eq!(line.message, "hello");
        assert_eq!(line.level, Level::Warn);
        assert!(line.ts_ms > 0);
        let (recent, _) = feed().subscribe();
        assert!(recent.iter().any(|l| l.plugin == "feed-test"));
    }

    #[test]
    fn buffer_is_bounded() {
        let log = TracingLog::default();
        for i in 0..KEEP + 5 {
            log.write("a", Level::Info, &i.to_string());
        }
        let all = log.recent(None, usize::MAX);
        assert_eq!(all.len(), KEEP);
        assert_eq!(all[0].message, "5");
    }
}
