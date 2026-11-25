//! Linux backends. Windows, screens, hotkeys, the keyboard and the app
//! list work on X11 (`x11.rs`, `ewmh.rs`) and on Hyprland (`hyprland/`,
//! over its IPC socket); `desktop.rs` picks one on each call. The clipboard
//! works on X11 and on Wayland compositors with the data-control protocol;
//! notifications go over D-Bus; apps launch through desktop files and
//! `xdg-open`. On other Wayland compositors, and with no display at all,
//! those capabilities answer `unavailable` and log why once. See
//! docs/linux.md.

pub mod accessibility;
pub mod app;
pub mod clipboard;
pub mod desktop;
pub mod ewmh;
pub mod hotkey;
pub mod hyprland;
pub mod keyboard;
pub mod mouse;
pub mod notify;
pub mod screen;
pub mod secret_service;
pub mod system;
pub mod tts;
pub mod virtual_screen;
pub mod window;
pub mod x11;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The kind of graphical session the node runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    X11,
    Wayland,
    /// No display: a server, a container, an SSH login.
    Headless,
}

impl Session {
    /// Reads `DISPLAY`, `WAYLAND_DISPLAY` and `XDG_SESSION_TYPE`.
    pub fn current() -> Self {
        Self::from_env(
            std::env::var_os("DISPLAY").as_deref(),
            std::env::var_os("WAYLAND_DISPLAY").as_deref(),
            std::env::var_os("XDG_SESSION_TYPE").as_deref(),
        )
    }

    /// A Wayland session wins over `DISPLAY`: XWayland only shows the X11
    /// clients, and key grabs there only fire while one of them has focus.
    pub fn from_env(
        display: Option<&OsStr>,
        wayland: Option<&OsStr>,
        session_type: Option<&OsStr>,
    ) -> Self {
        let set = |v: Option<&OsStr>| v.is_some_and(|v| !v.is_empty());
        if set(wayland) || session_type == Some(OsStr::new("wayland")) {
            Self::Wayland
        } else if set(display) {
            Self::X11
        } else {
            Self::Headless
        }
    }
}

/// Logs `message` the first time it is called for `flag`.
pub fn warn_once(flag: &AtomicBool, message: &str) {
    if !flag.swap(true, Ordering::Relaxed) {
        tracing::warn!("{message}");
    }
}

/// The first executable file named `name` in the `PATH` list `path`.
pub fn which_in(name: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

/// [`which_in`] on the node's `PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    which_in(name, &std::env::var_os("PATH")?)
}

/// The first of `names` found by `find`.
pub fn first_found(names: &[&str], find: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    names.iter().find_map(|n| find(n))
}

/// Whether `path` names a file a user could run.
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<&OsStr> {
        Some(OsStr::new(s))
    }

    #[test]
    fn wayland_wins_over_display() {
        assert_eq!(Session::from_env(os(":0"), None, None), Session::X11);
        assert_eq!(
            Session::from_env(os(":0"), os("wayland-0"), None),
            Session::Wayland
        );
        assert_eq!(
            Session::from_env(os(":0"), None, os("wayland")),
            Session::Wayland
        );
        assert_eq!(Session::from_env(os(":0"), None, os("x11")), Session::X11);
        assert_eq!(Session::from_env(None, None, None), Session::Headless);
        assert_eq!(Session::from_env(os(""), os(""), None), Session::Headless);
    }

    #[test]
    fn which_finds_the_first_executable() {
        use std::os::unix::fs::PermissionsExt;
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let plain = a.path().join("tool");
        std::fs::write(&plain, "").unwrap();
        let exe = b.path().join("tool");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths([a.path(), b.path()]).unwrap();
        // The file in `a` is not executable, so `b` wins.
        assert_eq!(which_in("tool", &path), Some(exe));
        assert_eq!(which_in("other", &path), None);
    }
}
