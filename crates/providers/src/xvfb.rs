//! The pure parts of the Xvfb backend (`linux/virtual_screen.rs`): which
//! display number to take, the server's arguments, and the environment a
//! launched program gets. They hold no process or X11 code, so the tests
//! run on every system.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::PathBuf;

/// The first display number tried; the user's own displays are `:0` and
/// `:1`, and Xvfb's own default is `:99`.
pub const FIRST_DISPLAY: u32 = 99;
/// How many numbers from [`FIRST_DISPLAY`] are tried.
pub const DISPLAY_RANGE: u32 = 100;

/// `:99`.
pub fn display_name(n: u32) -> String {
    format!(":{n}")
}

/// The socket a running server listens on.
pub fn socket_path(n: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/.X11-unix/X{n}"))
}

/// The lock file a running server holds.
pub fn lock_path(n: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/.X{n}-lock"))
}

/// The first number from [`FIRST_DISPLAY`] on that `taken` does not claim
/// and is not in `skip`.
pub fn choose_display(taken: impl Fn(u32) -> bool, skip: &[u32]) -> Option<u32> {
    (FIRST_DISPLAY..FIRST_DISPLAY + DISPLAY_RANGE).find(|n| !taken(*n) && !skip.contains(n))
}

/// Whether a socket or lock file exists for display `n`.
pub fn in_use(n: u32) -> bool {
    socket_path(n).exists() || lock_path(n).exists()
}

/// The arguments of `Xvfb`: no TCP socket, one 24-bit screen.
pub fn xvfb_args(n: u32, width: u32, height: u32) -> Vec<String> {
    vec![
        display_name(n),
        "-screen".into(),
        "0".into(),
        format!("{width}x{height}x24"),
        "-nolisten".into(),
        "tcp".into(),
    ]
}

/// What a program launched on the screen gets: variables to set, then
/// variables to remove. It must reach the virtual screen, never the
/// user's own session.
pub fn launch_env(display: &str) -> (Vec<(&'static str, String)>, Vec<&'static str>) {
    (
        vec![
            ("DISPLAY", display.to_string()),
            ("GDK_BACKEND", "x11".into()),
            ("QT_QPA_PLATFORM", "xcb".into()),
            ("SDL_VIDEODRIVER", "x11".into()),
        ],
        vec!["WAYLAND_DISPLAY", "XAUTHORITY"],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_numbers_start_at_99_and_skip_taken_ones() {
        assert_eq!(choose_display(|_| false, &[]), Some(99));
        assert_eq!(choose_display(|n| n == 99 || n == 100, &[]), Some(101));
        assert_eq!(choose_display(|n| n == 99, &[100]), Some(101));
        assert_eq!(choose_display(|_| true, &[]), None);
    }

    #[test]
    fn a_display_is_taken_by_its_socket_or_lock_file() {
        assert_eq!(socket_path(99), PathBuf::from("/tmp/.X11-unix/X99"));
        assert_eq!(lock_path(99), PathBuf::from("/tmp/.X99-lock"));
        assert_eq!(display_name(7), ":7");
        // Nothing runs on display 4242 in a test.
        assert!(!in_use(4242));
    }

    #[test]
    fn xvfb_gets_one_screen_and_no_tcp() {
        assert_eq!(
            xvfb_args(99, 1280, 800),
            [":99", "-screen", "0", "1280x800x24", "-nolisten", "tcp"]
        );
    }

    #[test]
    fn programs_are_pointed_at_the_virtual_screen() {
        let (set, unset) = launch_env(":100");
        assert!(set.contains(&("DISPLAY", ":100".to_string())));
        assert!(set.contains(&("GDK_BACKEND", "x11".to_string())));
        assert!(unset.contains(&"WAYLAND_DISPLAY"));
    }
}
