//! `files`: watches folders with the `notify` crate (FSEvents on macOS,
//! inotify on Linux). A change reaches the plugin that asked as a
//! [`ProviderEvent::File`] through the event sink. `files` never runs on a
//! peer. A folder with a subfolder it cannot read, such as /tmp on Linux,
//! is watched without its subfolders.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use notify::event::{ModifyKind, RenameMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::caller::Caller;
use crate::disk::{authorize, home};
use crate::error::CallError;
use crate::event::{EventSink, FileChange, FileEvent, ProviderEvent};

#[async_trait]
pub trait FilesProvider: Send + Sync {
    /// Watches `path` and everything under it; returns the watch id.
    async fn watch(&self, caller: &Caller, path: String) -> Result<u64, CallError>;
    /// Stops a watch. Stopping one that is already stopped does nothing.
    async fn unwatch(&self, caller: &Caller, id: u64) -> Result<(), CallError>;
    /// Stops all of `plugin`'s watches.
    fn release(&self, plugin: &str);
}

struct Entry {
    plugin: String,
    /// Dropping it stops the watch.
    _watcher: RecommendedWatcher,
}

/// Watches with the `notify` crate, and delivers `ProviderEvent::File`
/// through `sink` to the plugin that asked.
pub struct NotifyFiles {
    sink: EventSink,
    next_id: AtomicU64,
    watches: Mutex<HashMap<u64, Entry>>,
}

impl NotifyFiles {
    pub fn new(sink: EventSink) -> Self {
        Self {
            sink,
            next_id: AtomicU64::new(1),
            watches: Mutex::default(),
        }
    }
}

/// The `(path, change)` pairs for one notify event, one per path.
fn changes(event: &notify::Event) -> Vec<(String, FileChange)> {
    let name = |p: &Path| p.to_string_lossy().into_owned();
    let paths = &event.paths;
    let mut out: Vec<(String, FileChange)> = match &event.kind {
        EventKind::Access(_) => Vec::new(),
        EventKind::Create(_) => paths
            .iter()
            .map(|p| (name(p), FileChange::Created))
            .collect(),
        EventKind::Remove(_) => paths
            .iter()
            .map(|p| (name(p), FileChange::Removed))
            .collect(),
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if paths.len() == 2 => vec![
            (name(&paths[0]), FileChange::Removed),
            (name(&paths[1]), FileChange::Created),
        ],
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => paths
            .iter()
            .map(|p| (name(p), FileChange::Removed))
            .collect(),
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => paths
            .iter()
            .map(|p| (name(p), FileChange::Created))
            .collect(),
        _ => paths
            .iter()
            .map(|p| (name(p), FileChange::Modified))
            .collect(),
    };
    let mut seen = std::collections::HashSet::new();
    out.retain(|(path, _)| seen.insert(path.clone()));
    out
}

#[async_trait]
impl FilesProvider for NotifyFiles {
    async fn watch(&self, caller: &Caller, path: String) -> Result<u64, CallError> {
        let full = authorize(&path, &caller.disk, &home(), false)?;
        if !full.exists() {
            return Err(CallError::Failed(format!("{path} does not exist")));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let sink = self.sink.clone();
        let plugin = caller.plugin.clone();
        let owner = plugin.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    for (path, change) in changes(&event) {
                        sink(
                            &owner,
                            ProviderEvent::File(FileEvent {
                                watch: id,
                                path,
                                change,
                            }),
                        );
                    }
                }
                Err(e) => tracing::debug!(plugin = %owner, "files: {e}"),
            })
            .map_err(CallError::failed)?;
        if let Err(e) = watcher.watch(&full, RecursiveMode::Recursive) {
            // inotify adds a watch per subfolder and stops at the first it
            // cannot read, as in /tmp on a systemd machine. The folder
            // itself is still worth watching.
            if !matches!(&e.kind, notify::ErrorKind::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied)
            {
                return Err(CallError::failed(e));
            }
            tracing::warn!(plugin = %plugin, "files: {e}; watching {} without its subfolders", full.display());
            let _ = watcher.unwatch(&full);
            watcher
                .watch(&full, RecursiveMode::NonRecursive)
                .map_err(CallError::failed)?;
        }
        tracing::info!(plugin = %plugin, "files: watch {id} on {}", full.display());
        self.watches.lock().unwrap().insert(
            id,
            Entry {
                plugin,
                _watcher: watcher,
            },
        );
        Ok(id)
    }

    async fn unwatch(&self, caller: &Caller, id: u64) -> Result<(), CallError> {
        let mut watches = self.watches.lock().unwrap();
        if watches.get(&id).is_some_and(|e| e.plugin == caller.plugin) {
            watches.remove(&id);
        }
        Ok(())
    }

    fn release(&self, plugin: &str) {
        self.watches
            .lock()
            .unwrap()
            .retain(|_, e| e.plugin != plugin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::DiskGrant;
    use std::sync::Arc;
    use std::time::Duration;

    fn granted(root: &Path) -> Caller {
        let mut caller = Caller::new("w");
        caller.disk = vec![DiskGrant {
            root: root.to_path_buf(),
            write: false,
        }];
        caller
    }

    #[tokio::test]
    async fn watching_needs_a_grant_and_an_existing_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let files = NotifyFiles::new(Arc::new(|_: &str, _: ProviderEvent| {}));
        let path = root.to_string_lossy().into_owned();
        assert_eq!(
            files.watch(&Caller::new("w"), path.clone()).await,
            Err(CallError::Denied)
        );
        let caller = granted(&root);
        let missing = root.join("missing").to_string_lossy().into_owned();
        assert!(matches!(
            files.watch(&caller, missing).await,
            Err(CallError::Failed(_))
        ));
        let first = files.watch(&caller, path.clone()).await.unwrap();
        let second = files.watch(&caller, path).await.unwrap();
        assert!(second > first);
        files.unwatch(&caller, first).await.unwrap();
        files.unwatch(&caller, first).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn changes_arrive_as_events_for_the_watching_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let files = NotifyFiles::new(Arc::new(move |plugin: &str, e: ProviderEvent| {
            let _ = tx.send((plugin.to_string(), e));
        }));
        let caller = granted(&root);
        let id = files
            .watch(&caller, root.to_string_lossy().into_owned())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::fs::write(root.join("a.txt"), "x").unwrap();
        let got = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some((plugin, ProviderEvent::File(e))) = rx.recv().await
                    && e.path.ends_with("a.txt")
                {
                    return (plugin, e);
                }
            }
        })
        .await
        .expect("no event for a.txt");
        assert_eq!(got.0, "w");
        assert_eq!(got.1.watch, id);
        assert_ne!(got.1.change, FileChange::Removed);
        files.release("w");
    }
}
