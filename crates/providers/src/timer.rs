//! `timer`: one Tokio task per timer. A fired timer goes to the owner's
//! queue through the event sink.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::event::{EventSink, ProviderEvent};

/// `every` faster than this would flood a plugin's queue.
pub const MIN_EVERY: Duration = Duration::from_millis(10);

#[async_trait]
pub trait TimerProvider: Send + Sync {
    fn after(&self, plugin: &str, ms: u32) -> u64;
    fn every(&self, plugin: &str, ms: u32) -> u64;
    /// Only the plugin that started a timer may cancel it.
    fn cancel(&self, plugin: &str, id: u64) -> bool;
    /// Cancels every timer of `plugin`; returns how many.
    fn cancel_owned(&self, plugin: &str) -> usize;
    fn count(&self, plugin: &str) -> usize;
    async fn sleep(&self, ms: u32);
}

struct Entry {
    plugin: String,
    task: JoinHandle<()>,
}

pub struct LocalTimers {
    handle: Handle,
    sink: EventSink,
    next_id: AtomicU64,
    entries: Arc<Mutex<HashMap<u64, Entry>>>,
}

impl LocalTimers {
    /// Must be called inside a Tokio runtime; timers run on it.
    pub fn new(sink: EventSink) -> Self {
        Self {
            handle: Handle::current(),
            sink,
            next_id: AtomicU64::new(1),
            entries: Arc::default(),
        }
    }

    fn start(&self, plugin: &str, delay: Duration, repeat: bool) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let sink = self.sink.clone();
        let entries = self.entries.clone();
        let owner = plugin.to_string();
        // Hold the lock across spawn so a zero-delay timer cannot finish
        // and remove its entry before the entry exists.
        let mut table = self.entries.lock().unwrap();
        let task = self.handle.spawn(async move {
            if repeat {
                let mut ticks =
                    tokio::time::interval_at(tokio::time::Instant::now() + delay, delay);
                ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticks.tick().await;
                    sink(&owner, ProviderEvent::Timer(id));
                }
            } else {
                tokio::time::sleep(delay).await;
                entries.lock().unwrap().remove(&id);
                sink(&owner, ProviderEvent::Timer(id));
            }
        });
        table.insert(
            id,
            Entry {
                plugin: plugin.to_string(),
                task,
            },
        );
        id
    }
}

#[async_trait]
impl TimerProvider for LocalTimers {
    fn after(&self, plugin: &str, ms: u32) -> u64 {
        self.start(plugin, Duration::from_millis(ms.into()), false)
    }

    fn every(&self, plugin: &str, ms: u32) -> u64 {
        let every = Duration::from_millis(ms.into()).max(MIN_EVERY);
        self.start(plugin, every, true)
    }

    fn cancel(&self, plugin: &str, id: u64) -> bool {
        let mut entries = self.entries.lock().unwrap();
        if entries.get(&id).is_some_and(|e| e.plugin == plugin) {
            entries.remove(&id).unwrap().task.abort();
            true
        } else {
            false
        }
    }

    fn cancel_owned(&self, plugin: &str) -> usize {
        let mut entries = self.entries.lock().unwrap();
        let before = entries.len();
        entries.retain(|_, e| {
            let keep = e.plugin != plugin;
            if !keep {
                e.task.abort();
            }
            keep
        });
        before - entries.len()
    }

    fn count(&self, plugin: &str) -> usize {
        let entries = self.entries.lock().unwrap();
        entries.values().filter(|e| e.plugin == plugin).count()
    }

    async fn sleep(&self, ms: u32) {
        tokio::time::sleep(Duration::from_millis(ms.into())).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn timers() -> (
        LocalTimers,
        mpsc::UnboundedReceiver<(String, ProviderEvent)>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let sink: EventSink = Arc::new(move |p, e| {
            let _ = tx.send((p.to_string(), e));
        });
        (LocalTimers::new(sink), rx)
    }

    #[tokio::test]
    async fn after_fires_once_for_its_owner() {
        let (t, mut rx) = timers();
        let id = t.after("a", 5);
        assert_eq!(
            rx.recv().await.unwrap(),
            ("a".into(), ProviderEvent::Timer(id))
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(rx.try_recv().is_err());
        assert_eq!(t.count("a"), 0);
    }

    #[tokio::test]
    async fn every_repeats_until_cancelled_by_its_owner() {
        let (t, mut rx) = timers();
        let id = t.every("a", 10);
        rx.recv().await.unwrap();
        rx.recv().await.unwrap();
        assert!(!t.cancel("b", id));
        assert!(t.cancel("a", id));
        tokio::time::sleep(Duration::from_millis(30)).await;
        while rx.try_recv().is_ok() {}
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn cancel_owned_drops_only_that_plugin() {
        let (t, _rx) = timers();
        t.after("a", 10_000);
        t.every("a", 10_000);
        t.after("b", 10_000);
        assert_eq!(t.cancel_owned("a"), 2);
        assert_eq!(t.count("a"), 0);
        assert_eq!(t.count("b"), 1);
    }
}
