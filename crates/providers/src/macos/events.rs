//! Notification observers shared by the `app` and `system` watchers.
//! NSWorkspace posts its notifications on the main thread, which the
//! node's main-thread loop runs, so these observers exist only when a
//! `Desktop` does. The handler runs on whichever thread posts, with no
//! queue in between, and only forwards to the event sink.

use std::collections::BTreeSet;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2_foundation::{NSNotification, NSNotificationCenter, NSNotificationName};

/// The plugins that asked for one kind of event.
pub type Watchers = Arc<Mutex<BTreeSet<String>>>;

/// Calls `handler` for each `name` notification on `center`, for the life
/// of the process. The center keeps the observer.
pub fn observe(
    center: &NSNotificationCenter,
    name: &NSNotificationName,
    handler: impl Fn(&NSNotification) + Send + Sync + 'static,
) {
    let block = RcBlock::new(move |n: NonNull<NSNotification>| {
        // SAFETY: the center passes a live notification for the call.
        handler(unsafe { n.as_ref() });
    });
    // SAFETY: no object filter and no queue; the block is `Send + Sync`
    // through its handler.
    let token = unsafe {
        center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
    };
    // The center holds the observer until it is removed; this one lives as
    // long as the process.
    std::mem::forget(token);
}

/// Adds `plugin` to `set` when `on`, removes it otherwise.
pub fn toggle(set: &Watchers, plugin: &str, on: bool) {
    let mut set = set.lock().unwrap();
    if on {
        set.insert(plugin.to_string());
    } else {
        set.remove(plugin);
    }
}

/// A copy of the watching plugins' names.
pub fn names(set: &Watchers) -> Vec<String> {
    set.lock().unwrap().iter().cloned().collect()
}
