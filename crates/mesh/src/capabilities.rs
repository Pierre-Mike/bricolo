//! The capabilities a peer can call, and how each method travels.
//!
//! [`remote_capabilities!`] is the table. Each entry names the capability,
//! its field in [`Providers`], its provider trait, the client type to
//! generate, and the trait's methods with their argument and result types,
//! minus `&self` and the caller. From one entry the macro builds:
//!
//! - the client type, which implements the provider trait by sending each
//!   call to a peer, with the arguments as a `postcard` tuple;
//! - its arm in [`dispatch`], which decodes the arguments, calls this
//!   node's provider and encodes the result.
//!
//! To make another capability remote:
//!
//! 1. Add its entry to the table below.
//! 2. Add one line to `impl Peers for Mesh` in `mesh.rs`.
//! 3. Add it to `ROUTABLE` in `bricolo-providers` and to the `Peers` trait in
//!    `bricolo-router`, so rules.toml can route it.
//!
//! Every argument and result type must implement serde's `Serialize` and
//! `Deserialize`.
//!
//! `speech.transcribe_file` is not in the table. A path names a file on
//! the calling node, so [`GuardedSpeech`] checks it against the calling
//! plugin's grants, reads it there and sends the bytes as `transcribe`.
//! The peer never opens a path for a speech call.

use std::path::PathBuf;

use async_trait::async_trait;
use bricolo_providers::accessibility::{
    AccessibilityProvider, ActionResult, AppState, AxApp, AxWindow, Element,
};
use bricolo_providers::claude_code::{ClaudeCodeProvider, Session};
use bricolo_providers::clipboard::ClipboardProvider;
use bricolo_providers::device::DeviceProvider;
use bricolo_providers::disk::{DiskProvider, authorize};
use bricolo_providers::notify::NotifyProvider;
use bricolo_providers::process::{ProcessInfo, ProcessProvider};
use bricolo_providers::scripting::ScriptingProvider;
use bricolo_providers::speech::SpeechProvider;
use bricolo_providers::tts::{TtsProvider, TtsStatus};
use bricolo_providers::virtual_screen::{VirtualScreenInfo, VirtualScreenProvider};
use bricolo_providers::{CallError, Caller, Providers};

use crate::client::Remote;
use crate::wire::{Codec, Postcard};

macro_rules! remote_capabilities {
    ($(
        $cap:literal => $field:ident: $Trait:ident as $Client:ident {
            $( fn $method:ident($($arg:ident: $ty:ty),*) -> $ret:ty; )*
        }
    )*) => {
        /// Capabilities this node serves to peers, subject to their allow
        /// lists.
        pub const REMOTE: &[&str] = &[$($cap),*];

        /// Every remote method as `(capability, method)`, with the method
        /// named as in Rust (`transcribe`, not the WIT's kebab case).
        pub const METHODS: &[(&str, &str)] = &[$($(($cap, stringify!($method))),*),*];

        $(
            #[doc = concat!("`", $cap, "` on a peer.")]
            pub struct $Client(pub(crate) Remote);

            #[async_trait]
            impl $Trait for $Client {
                $(
                    async fn $method(&self, caller: &Caller, $($arg: $ty),*) -> Result<$ret, CallError> {
                        let args = Postcard::encode(&($($arg,)*));
                        self.0.call($cap, stringify!($method), caller, args).await
                    }
                )*
            }
        )*

        /// Runs one method on `providers`, with arguments and result in
        /// codec `C`. A capability outside [`REMOTE`] is `unavailable`.
        #[allow(clippy::let_unit_value, unused_variables)]
        pub async fn dispatch<C: Codec>(
            providers: &Providers,
            caller: &Caller,
            capability: &str,
            method: &str,
            args: &[u8],
        ) -> Result<Vec<u8>, CallError> {
            match (capability, method) {
                $($(
                    ($cap, stringify!($method)) => {
                        let ($($arg,)*): ($($ty,)*) = C::decode(args)?;
                        let out: $ret = providers.$field.$method(caller, $($arg),*).await?;
                        C::encode(&out)
                    }
                )*)*
                _ if REMOTE.contains(&capability) => Err(CallError::Failed(format!(
                    "{capability} has no method {method}"
                ))),
                _ => Err(CallError::Unavailable),
            }
        }

        /// Runs one method on the peer behind `remote`, with arguments and
        /// result in codec `C`. The peer applies its own allow list and
        /// grants; this node checks nothing but the method's name.
        #[allow(clippy::let_unit_value, unused_variables)]
        pub(crate) async fn dispatch_remote<C: Codec>(
            remote: &Remote,
            caller: &Caller,
            capability: &str,
            method: &str,
            args: &[u8],
        ) -> Result<Vec<u8>, CallError> {
            match (capability, method) {
                $($(
                    ($cap, stringify!($method)) => {
                        let ($($arg,)*): ($($ty,)*) = C::decode(args)?;
                        let out: $ret = $Client(remote.clone()).$method(caller, $($arg),*).await?;
                        C::encode(&out)
                    }
                )*)*
                _ if REMOTE.contains(&capability) => Err(CallError::Failed(format!(
                    "{capability} has no method {method}"
                ))),
                _ => Err(CallError::Unavailable),
            }
        }
    };
}

/// The reserved capability a peer calls to learn what it may call here.
/// Every trusted peer may call it, whatever its allow list says.
pub const MESH: &str = "mesh";

/// `mesh.methods`: the [`METHODS`] that `allows` lets the caller run.
pub fn allowed_methods(allows: impl Fn(&str) -> bool) -> Vec<(String, String)> {
    METHODS
        .iter()
        .filter(|(cap, _)| allows(cap))
        .map(|(cap, method)| (cap.to_string(), method.to_string()))
        .collect()
}

remote_capabilities! {
    "tts" => tts: TtsProvider as RemoteTts {
        fn speak(text: String, voice: Option<String>) -> ();
        fn stop() -> ();
        fn status() -> TtsStatus;
    }
    "speech" => speech: SpeechProvider as RemoteSpeech {
        fn transcribe(audio: Vec<u8>, format: String) -> String;
    }
    "disk" => disk: DiskProvider as RemoteDisk {
        fn read(path: String) -> Vec<u8>;
        fn write(path: String, data: Vec<u8>) -> ();
        fn list(path: String) -> Vec<String>;
    }
    "clipboard" => clipboard: ClipboardProvider as RemoteClipboard {
        fn get() -> Option<String>;
        fn set(text: String) -> ();
        fn formats() -> Vec<String>;
        fn get_image() -> Option<String>;
        fn set_image(path: String) -> ();
        fn get_files() -> Vec<String>;
        fn set_files(paths: Vec<String>) -> ();
    }
    "notify" => notify: NotifyProvider as RemoteNotify {
        fn show(title: String, body: String) -> ();
    }
    "claude-code" => claude_code: ClaudeCodeProvider as RemoteClaudeCode {
        fn start(dir: String, prompt: String) -> Session;
        fn send(id: String, prompt: String) -> Session;
        fn list() -> Vec<Session>;
        fn status(id: String) -> Session;
        fn stop(id: String) -> ();
    }
    "accessibility" => accessibility: AccessibilityProvider as RemoteAccessibility {
        fn list_apps() -> Vec<AxApp>;
        fn list_windows(app: String) -> Vec<AxWindow>;
        fn snapshot(
            app: String,
            window: Option<u32>,
            query: Option<String>,
            element: Option<u32>,
            screenshot: Option<bool>
        ) -> AppState;
        fn find(app: String, text: String) -> Vec<Element>;
        fn click(app: String, element: u32) -> ActionResult;
        fn set_value(app: String, element: u32, value: String) -> ActionResult;
        fn type_text(app: String, element: Option<u32>, text: String) -> ActionResult;
        fn press_key(app: String, key: String) -> ActionResult;
        fn perform(app: String, element: u32, action: String) -> ActionResult;
        fn menu(app: String, path: Vec<String>) -> ActionResult;
        fn wait(app: String, text: String, timeout_ms: Option<u32>) -> Vec<Element>;
    }
    "process" => process: ProcessProvider as RemoteProcess {
        fn list() -> Vec<ProcessInfo>;
        fn get(pid: u32) -> Option<ProcessInfo>;
        fn stop(pid: u32) -> ();
        fn kill(pid: u32) -> ();
    }
    "device" => device: DeviceProvider as RemoteDevice {
        fn volume() -> u8;
        fn set_volume(level: u8) -> ();
        fn muted() -> bool;
        fn set_muted(muted: bool) -> ();
        fn brightness() -> u8;
        fn set_brightness(level: u8) -> ();
        fn wifi() -> bool;
        fn set_wifi(on: bool) -> ();
        fn dark_mode() -> bool;
        fn set_dark_mode(on: bool) -> ();
        fn focus_mode() -> bool;
        fn set_focus_mode(on: bool) -> ();
        fn lock() -> ();
        fn sleep() -> ();
    }
    "scripting" => scripting: ScriptingProvider as RemoteScripting {
        fn run_applescript(source: String) -> String;
        fn run_jxa(source: String) -> String;
        fn list_shortcuts() -> Vec<String>;
        fn run_shortcut(name: String, input: Option<String>) -> String;
        fn run_powershell(source: String) -> String;
    }
    "virtual-screen" => virtual_screen: VirtualScreenProvider as RemoteVirtualScreen {
        fn create(width: u32, height: u32) -> VirtualScreenInfo;
        fn list() -> Vec<VirtualScreenInfo>;
        fn launch(id: u32, program: String, args: Vec<String>) -> u32;
        fn capture(id: u32) -> String;
        fn click(id: u32, x: i32, y: i32) -> ();
        fn type_text(id: u32, text: String) -> ();
        fn press_key(id: u32, key: String) -> ();
        fn destroy(id: u32) -> ();
    }
}

/// Disk calls to a peer stay inside the calling plugin's own grants: this
/// checks the path on the calling node before the call leaves. The peer
/// then applies the grants it gave this node in its `peers.toml`.
pub struct GuardedDisk(pub(crate) RemoteDisk);

fn home() -> PathBuf {
    bricolo_providers::disk::home()
}

#[async_trait]
impl DiskProvider for GuardedDisk {
    async fn read(&self, caller: &Caller, path: String) -> Result<Vec<u8>, CallError> {
        authorize(&path, &caller.disk, &home(), false)?;
        self.0.read(caller, path).await
    }

    async fn write(&self, caller: &Caller, path: String, data: Vec<u8>) -> Result<(), CallError> {
        authorize(&path, &caller.disk, &home(), true)?;
        self.0.write(caller, path, data).await
    }

    async fn list(&self, caller: &Caller, path: String) -> Result<Vec<String>, CallError> {
        authorize(&path, &caller.disk, &home(), false)?;
        self.0.list(caller, path).await
    }
}

/// Speech on a peer. `transcribe_file` reads the file on this node, inside
/// the calling plugin's `disk` grants, and sends its bytes.
pub struct GuardedSpeech(pub(crate) RemoteSpeech);

#[async_trait]
impl SpeechProvider for GuardedSpeech {
    async fn transcribe(
        &self,
        caller: &Caller,
        audio: Vec<u8>,
        format: String,
    ) -> Result<String, CallError> {
        self.0.transcribe(caller, audio, format).await
    }

    async fn transcribe_file(&self, caller: &Caller, path: String) -> Result<String, CallError> {
        let real = authorize(&path, &caller.disk, &home(), false)?;
        let audio = tokio::fs::read(&real)
            .await
            .map_err(|e| CallError::Failed(format!("cannot read {path}: {e}")))?;
        let format = real
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "wav".into());
        self.0.transcribe(caller, audio, format).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bricolo_providers::{LocalOptions, ProviderEvent};

    use super::*;
    use crate::wire::Json;

    fn providers(dir: &std::path::Path) -> Providers {
        let sink: bricolo_providers::EventSink = Arc::new(|_: &str, _: ProviderEvent| {});
        Providers::local(LocalOptions::new(dir.to_path_buf()), sink)
    }

    #[tokio::test]
    async fn dispatch_decodes_calls_and_encodes_results() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.txt"), "hi").unwrap();
        let p = providers(&root);
        let caller = Caller {
            plugin: "t".into(),
            disk: vec![bricolo_providers::disk::DiskGrant {
                root: root.clone(),
                write: false,
            }],
            ..Caller::default()
        };
        let path = root.join("a.txt").to_string_lossy().into_owned();
        let args = Postcard::encode(&(path.clone(),)).unwrap();
        let out = dispatch::<Postcard>(&p, &caller, "disk", "read", &args)
            .await
            .unwrap();
        assert_eq!(Postcard::decode::<Vec<u8>>(&out).unwrap(), b"hi");

        let json = serde_json::to_vec(&[root.to_string_lossy()]).unwrap();
        let out = dispatch::<Json>(&p, &caller, "disk", "list", &json)
            .await
            .unwrap();
        assert_eq!(out, br#"["a.txt"]"#);

        // No speech server here, so the local provider answers unavailable;
        // errors pass through as they are.
        assert_eq!(
            dispatch::<Json>(&p, &caller, "speech", "transcribe", br#"[[], "wav"]"#).await,
            Err(CallError::Unavailable)
        );
        assert_eq!(
            dispatch::<Json>(&p, &caller, "kv", "get", b"[]").await,
            Err(CallError::Unavailable)
        );
        assert!(matches!(
            dispatch::<Json>(&p, &caller, "disk", "format", b"[]").await,
            Err(CallError::Failed(_))
        ));
        assert!(matches!(
            dispatch::<Json>(&p, &caller, "disk", "read", b"[1]").await,
            Err(CallError::Failed(_))
        ));
    }
}
