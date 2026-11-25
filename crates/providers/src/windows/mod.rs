//! Windows backends, on Win32 through the `windows` crate. Each module
//! is one capability; `platform()` in registry.rs picks them up. See
//! docs/windows.md.

/// Child processes: process groups, Ctrl+Break and job objects, for the
/// sidecars and ffmpeg.
pub(crate) mod process;

pub mod accessibility;
pub mod app;
pub mod clipboard;
pub mod credentials;
pub mod dshow;
pub mod hotkey;
mod input;
pub mod keyboard;
pub mod mouse;
pub mod notify;
pub mod screen;
pub mod system;
pub mod tts;
pub mod virtual_screen;
pub mod window;
mod wm;
