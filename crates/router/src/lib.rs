//! The router: reads `rules.toml` and wraps each routable capability in a
//! provider that tries the listed targets in order.
//!
//! `rules.toml`:
//!
//! ```toml
//! # Seconds one target may take before the router counts it as timed out
//! # and tries the next. Optional, default 30.
//! timeout_secs = 30
//!
//! [routes]
//! # capability = targets, tried in order. "local" is this node; any other
//! # name is a peer from peers.toml. Unlisted capabilities run locally.
//! speech = ["desktop", "local"]
//! tts = ["local"]
//! ```
//!
//! Only `tts`, `speech`, `disk`, `clipboard`, `notify` and `claude-code`
//! can be routed.
//! The rest always run on this node: `log`, `timer`, `kv`, the desktop
//! capabilities (`hotkey`, `window`, `app`), and `shell` and `http`, whose
//! allow lists belong to the calling plugin's manifest.
//!
//! A target that answers `unavailable` or `timeout` passes the call to the
//! next one. Any other answer, success or error, is final. When every
//! target fails, the caller gets the last error.

mod fallback;
mod rules;

use std::sync::Arc;
use std::time::Duration;

pub use fallback::{Fallback, Target};
pub use rules::{LOCAL, Rules, RulesError};

use bricolo_providers::Providers;
use bricolo_providers::claude_code::ClaudeCodeProvider;
use bricolo_providers::clipboard::ClipboardProvider;
use bricolo_providers::disk::DiskProvider;
use bricolo_providers::notify::NotifyProvider;
use bricolo_providers::speech::SpeechProvider;
use bricolo_providers::tts::TtsProvider;

/// Providers on other nodes, by peer name. The mesh implements this in
/// phase 2. `None` means the peer is unknown or offline right now.
pub trait Peers: Send + Sync {
    fn tts(&self, peer: &str) -> Option<Arc<dyn TtsProvider>>;
    fn speech(&self, peer: &str) -> Option<Arc<dyn SpeechProvider>>;
    fn disk(&self, peer: &str) -> Option<Arc<dyn DiskProvider>>;
    fn clipboard(&self, peer: &str) -> Option<Arc<dyn ClipboardProvider>>;
    fn notify(&self, _peer: &str) -> Option<Arc<dyn NotifyProvider>> {
        None
    }
    fn claude_code(&self, _peer: &str) -> Option<Arc<dyn ClaudeCodeProvider>> {
        None
    }
}

/// A node without a mesh: every peer is offline.
pub struct NoPeers;

impl Peers for NoPeers {
    fn tts(&self, _: &str) -> Option<Arc<dyn TtsProvider>> {
        None
    }
    fn speech(&self, _: &str) -> Option<Arc<dyn SpeechProvider>> {
        None
    }
    fn disk(&self, _: &str) -> Option<Arc<dyn DiskProvider>> {
        None
    }
    fn clipboard(&self, _: &str) -> Option<Arc<dyn ClipboardProvider>> {
        None
    }
}

/// Builds the providers plugins call: local ones as they are, routable ones
/// wrapped in a [`Fallback`] over the targets in `rules`.
pub fn route(rules: &Rules, local: Providers, peers: Arc<dyn Peers>) -> Providers {
    let timeout = Duration::from_secs(rules.timeout_secs);
    let p = peers.clone();
    let tts = Fallback::new(
        "tts",
        timeout,
        resolve(&rules.targets("tts"), &local.tts, move |n| p.tts(n)),
    );
    let p = peers.clone();
    let speech = Fallback::new(
        "speech",
        timeout,
        resolve(&rules.targets("speech"), &local.speech, move |n| {
            p.speech(n)
        }),
    );
    let p = peers.clone();
    let disk = Fallback::new(
        "disk",
        timeout,
        resolve(&rules.targets("disk"), &local.disk, move |n| p.disk(n)),
    );
    let p = peers.clone();
    let clipboard = Fallback::new(
        "clipboard",
        timeout,
        resolve(&rules.targets("clipboard"), &local.clipboard, move |n| {
            p.clipboard(n)
        }),
    );
    let p = peers.clone();
    let notify = Fallback::new(
        "notify",
        timeout,
        resolve(&rules.targets("notify"), &local.notify, move |n| {
            p.notify(n)
        }),
    );
    let claude_code = Fallback::new(
        "claude-code",
        timeout,
        resolve(
            &rules.targets("claude-code"),
            &local.claude_code,
            move |n| peers.claude_code(n),
        ),
    );
    Providers {
        tts: Arc::new(tts),
        speech: Arc::new(speech),
        disk: Arc::new(disk),
        clipboard: Arc::new(clipboard),
        notify: Arc::new(notify),
        claude_code: Arc::new(claude_code),
        ..local
    }
}

/// Local targets are fixed. Peer targets are looked up on each call, so a
/// peer that comes online later starts receiving calls.
fn resolve<P: ?Sized + Send + Sync + 'static>(
    names: &[String],
    local: &Arc<P>,
    peer: impl Fn(&str) -> Option<Arc<P>> + Clone + Send + Sync + 'static,
) -> Vec<Target<P>> {
    names
        .iter()
        .map(|name| {
            if name == LOCAL {
                Target::fixed(name, local.clone())
            } else {
                let (peer, n) = (peer.clone(), name.clone());
                Target::lookup(name, move || peer(&n))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bricolo_providers::{CallError, Caller, LocalOptions, ProviderEvent};

    #[tokio::test]
    async fn unrouted_capabilities_reach_the_local_provider() {
        let dir = std::env::temp_dir();
        let sink: bricolo_providers::EventSink = Arc::new(|_: &str, _: ProviderEvent| {});
        let local = Providers::local(LocalOptions::new(dir), sink);
        let rules = Rules::parse("[routes]\ntts = [\"desktop\"]\nnotify = [\"phone\"]").unwrap();
        let routed = route(&rules, local, Arc::new(NoPeers));
        let caller = Caller::new("p");
        // tts goes only to an offline peer.
        assert_eq!(
            routed.tts.speak(&caller, "hi".into(), None).await,
            Err(CallError::Unavailable)
        );
        assert_eq!(
            routed.notify.show(&caller, "t".into(), "b".into()).await,
            Err(CallError::Unavailable)
        );
        // disk is unrouted, so the local provider answers, and refuses.
        assert_eq!(
            routed.disk.read(&caller, "/etc/hosts".into()).await,
            Err(CallError::Denied)
        );
    }
}
