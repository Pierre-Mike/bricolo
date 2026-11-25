//! One trait per capability, plus the local implementations the router
//! draws from. A remote peer implements the same traits in `mesh`, so
//! callers never know where a call runs.
//!
//! Portable: `log`, `timer`, `kv`, `disk`, `shell`, `http`, the sidecars,
//! `speech` and the `tts` queue (both need a server in `node.toml`),
//! `mic` and `camera` (ffmpeg), `audio` (ffmpeg, AVAudioPlayer on macOS,
//! `unavailable` on Windows), and `claude-code` (the `claude` CLI, with
//! `[claude-code] dirs` in `node.toml`). Per system, under `macos/`,
//! `linux/` and `windows/` (with its portable parts in `win.rs`): `hotkey`,
//! `clipboard`, `window`, `app`, `notify`, `keyboard`, `mouse`, `screen`,
//! `system`, `secrets`, and the TTS engine when no server is set (`say`;
//! espeak-ng or spd-say; SAPI). Elsewhere those answer
//! `CallError::Unavailable`. `ui` holds the tiles and panels the UI
//! process draws, and `sensors` the plugins using the microphone, camera or
//! screen.

pub mod accessibility;
pub mod app;
#[cfg(any(test, target_os = "linux"))]
mod atspi;
pub mod audio;
mod caller;
pub mod camera;
pub mod catalog;
pub mod claude_code;
pub mod clipboard;
pub mod cmd;
pub mod desktop;
pub mod device;
pub mod disk;
pub mod echo;
mod error;
mod event;
pub mod files;
pub mod hotkey;
pub mod http;
pub mod keyboard;
pub mod kv;
#[cfg(target_os = "linux")]
mod linux;
pub mod log;
#[cfg(target_os = "macos")]
mod macos;
pub mod mic;
pub mod mouse;
pub mod notify;
pub mod permissions;
pub mod process;
#[cfg(any(test, target_os = "windows"))]
mod rdp;
mod registry;
pub mod screen;
pub mod scripting;
pub mod secrets;
pub mod selection;
pub mod sensors;
pub mod shell;
pub mod sidecar;
pub mod speech;
pub mod system;
pub mod timer;
pub mod tts;
pub mod ui;
pub mod virtual_screen;
#[cfg(any(test, target_os = "windows"))]
mod win;
pub mod window;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(any(test, target_os = "linux"))]
mod xvfb;

pub use caller::Caller;
pub use desktop::Desktop;
pub use error::CallError;
pub use event::{
    AppChange, AppEvent, EventSink, FileChange, FileEvent, ProviderEvent, SystemEvent,
};
pub use registry::{CAPABILITIES, LocalOptions, Providers, ROUTABLE};
