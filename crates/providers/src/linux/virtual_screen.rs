//! `virtual-screen` on Linux: one Xvfb server for each screen. The server
//! starts on the first free display number from `:99` on, with no TCP
//! socket; programs launch on it with `DISPLAY` set, in their own process
//! group, so the user's session never sees them. Pictures read the root
//! window with `GetImage` and clicks and keys go through XTEST, both on a
//! connection to that display; no window manager runs, so keys reach the
//! window under the pointer.
//!
//! Ending a screen sends SIGTERM to each launched group, then to Xvfb, and
//! SIGKILL to what has not left a moment later. The display numbers, the
//! arguments and the environment are in `xvfb.rs`.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use x11rb::connection::{Connection, RequestConnection as _};
use x11rb::protocol::xproto::MOTION_NOTIFY_EVENT;
use x11rb::protocol::xtest::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::keyboard::Typist;
use super::mouse::button_press;
use super::screen::x11_capture_on;
use super::which;
use crate::error::CallError;
use crate::hotkey::Chord;
use crate::virtual_screen::ScreenBackend;
use crate::xvfb;

/// How long a new server has to accept connections.
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a process gets to leave after SIGTERM.
const TERM_GRACE: Duration = Duration::from_millis(1500);
/// Servers start one at a time, so two calls never pick the same number.
static START: Mutex<()> = Mutex::new(());

#[derive(Default)]
pub struct Xvfb;

/// A running server and what runs on it.
pub struct Screen {
    display: String,
    width: u32,
    height: u32,
    xvfb: Child,
    children: Vec<Child>,
}

fn failed(e: impl std::fmt::Display) -> CallError {
    CallError::Failed(format!("virtual screen: {e}"))
}

/// Connects to the screen's display.
fn open(screen: &Screen) -> Result<(RustConnection, usize), CallError> {
    x11rb::connect(Some(&screen.display)).map_err(|e| {
        failed(format!(
            "cannot open display {} ({e}); its Xvfb may have exited",
            screen.display
        ))
    })
}

enum Ready {
    Up,
    Exited,
    TimedOut,
}

/// Waits until `display` takes connections, the server exits, or time runs out.
fn wait_ready(child: &mut Child, number: u32, display: &str) -> Ready {
    let start = Instant::now();
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            return Ready::Exited;
        }
        if xvfb::socket_path(number).exists() && x11rb::connect(Some(display)).is_ok() {
            return Ready::Up;
        }
        if start.elapsed() >= START_TIMEOUT {
            return Ready::TimedOut;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Sends `sig` to `child`'s process group, if the child still runs: a
/// reaped leader's number may name another group by now.
fn signal_group(child: &mut Child, sig: i32) {
    if !matches!(child.try_wait(), Ok(None)) {
        return;
    }
    if let Ok(pgid) = i32::try_from(child.id()) {
        // SAFETY: killpg only sends a signal.
        unsafe { libc::killpg(pgid, sig) };
    }
}

/// Ends `child` and its group: SIGTERM, then SIGKILL after the grace
/// period, and reaps it.
fn terminate(child: &mut Child) {
    signal_group(child, libc::SIGTERM);
    let start = Instant::now();
    while start.elapsed() < TERM_GRACE {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    signal_group(child, libc::SIGKILL);
    let _ = child.wait();
}

impl Drop for Screen {
    fn drop(&mut self) {
        for child in &mut self.children {
            terminate(child);
        }
        terminate(&mut self.xvfb);
    }
}

impl ScreenBackend for Xvfb {
    type Screen = Screen;

    fn start(&self, width: u32, height: u32) -> Result<(Screen, String), CallError> {
        let program = which("Xvfb").ok_or_else(|| {
            CallError::Failed(
                "Xvfb is not installed; install the xvfb package (for example `apt install xvfb`) \
                 to use virtual screens"
                    .into(),
            )
        })?;
        let _one_at_a_time = START.lock().unwrap_or_else(|e| e.into_inner());
        let mut skip = Vec::new();
        for _ in 0..3 {
            let number = xvfb::choose_display(xvfb::in_use, &skip).ok_or_else(|| {
                failed("no free display number from :99 on; stop some virtual screens")
            })?;
            let display = xvfb::display_name(number);
            let mut child = Command::new(&program)
                .args(xvfb::xvfb_args(number, width, height))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .map_err(|e| failed(format!("cannot run {}: {e}", program.display())))?;
            match wait_ready(&mut child, number, &display) {
                Ready::Up => {
                    let screen = Screen {
                        display: display.clone(),
                        width,
                        height,
                        xvfb: child,
                        children: Vec::new(),
                    };
                    return Ok((screen, display));
                }
                // Someone else took the number between the check and the
                // start: try the next.
                Ready::Exited => skip.push(number),
                Ready::TimedOut => {
                    terminate(&mut child);
                    return Err(failed(format!(
                        "Xvfb did not accept connections on {display} within {} s",
                        START_TIMEOUT.as_secs()
                    )));
                }
            }
        }
        Err(failed("Xvfb exited at each display number it was given"))
    }

    fn launch(
        &self,
        screen: &mut Screen,
        program: &str,
        args: &[String],
    ) -> Result<u32, CallError> {
        let (set, unset) = xvfb::launch_env(&screen.display);
        let mut cmd = Command::new(program);
        cmd.args(args)
            .envs(set)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        for name in unset {
            cmd.env_remove(name);
        }
        let child = cmd
            .spawn()
            .map_err(|e| CallError::Failed(format!("cannot run {program}: {e}")))?;
        let pid = child.id();
        screen.children.push(child);
        Ok(pid)
    }

    fn alive(&self, screen: &mut Screen) -> Vec<u32> {
        // try_wait reaps the ones that exited.
        screen
            .children
            .retain_mut(|c| matches!(c.try_wait(), Ok(None)));
        screen.children.iter().map(Child::id).collect()
    }

    fn capture(&self, screen: &Screen, path: &Path) -> Result<(), CallError> {
        // A server that is gone answers `unavailable` below; say why.
        open(screen)?;
        x11_capture_on(Some(&screen.display), None, path)
    }

    fn click(&self, screen: &Screen, x: i32, y: i32) -> Result<(), CallError> {
        let inside = |v: i32, side: u32| v >= 0 && i64::from(v) < i64::from(side);
        if !inside(x, screen.width) || !inside(y, screen.height) {
            return Err(CallError::Failed(format!(
                "invalid_argument: ({x}, {y}) is outside the {}x{} screen",
                screen.width, screen.height
            )));
        }
        let (conn, number) = open(screen)?;
        if conn
            .extension_information(xtest::X11_EXTENSION_NAME)
            .map_err(failed)?
            .is_none()
        {
            return Err(failed("the X server has no XTEST extension"));
        }
        let root = conn.setup().roots[number].root;
        // The screen is at most 8192 pixels on a side: the coordinates fit.
        conn.xtest_fake_input(MOTION_NOTIFY_EVENT, 0, 0, root, x as i16, y as i16, 0)
            .map_err(failed)?;
        button_press(&conn, 1)?;
        conn.sync().map_err(failed)
    }

    fn type_text(&self, screen: &Screen, text: &str) -> Result<(), CallError> {
        let mut typist = Typist::open_on(Some(&screen.display))?;
        typist.type_text(text)?;
        typist.finish()
    }

    fn press_key(&self, screen: &Screen, chord: &Chord) -> Result<(), CallError> {
        let typist = Typist::open_on(Some(&screen.display))?;
        typist.keystroke(chord)?;
        typist.finish()
    }

    fn stop(&self, screen: Screen) {
        drop(screen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caller::Caller;
    use crate::shell::ShellGrant;
    use crate::virtual_screen::{VirtualScreenProvider, VirtualScreens};

    /// The RGBA pixels and size of a PNG.
    fn read_png(path: &str) -> (Vec<u8>, u32, u32) {
        let decoder =
            png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut buf).unwrap();
        buf.truncate(frame.buffer_size());
        (buf, frame.width, frame.height)
    }

    fn pixel(png: &(Vec<u8>, u32, u32), x: u32, y: u32) -> [u8; 3] {
        let at = ((y * png.1 + x) * 4) as usize;
        [png.0[at], png.0[at + 1], png.0[at + 2]]
    }

    /// Needs Xvfb and xterm, as `just linux-test` provides: starts a
    /// virtual screen next to the desktop's own, runs xterm on it, takes a
    /// picture, clicks and types, and takes another that differs.
    #[ignore = "needs Xvfb and xterm; run by just linux-test"]
    #[tokio::test(flavor = "multi_thread")]
    async fn x11_virtual_screen_runs_xterm_captures_clicks_and_types() {
        let (Some(_), Some(xterm)) = (which("Xvfb"), which("xterm")) else {
            eprintln!("Xvfb or xterm is not installed; skipping");
            return;
        };
        let xterm = xterm.display().to_string();
        let dir = tempfile::tempdir().unwrap();
        let screens = VirtualScreens::new(Xvfb, dir.path().to_path_buf());
        let c = Caller {
            plugin: "t".into(),
            shell: ShellGrant {
                programs: vec![xterm.clone()],
                env: Vec::new(),
            },
            ..Caller::default()
        };
        let info = screens.create(&c, 800, 600).await.unwrap();
        assert!(info.name.starts_with(':'), "{info:?}");
        let number: u32 = info.name[1..].parse().unwrap();
        assert!(xvfb::in_use(number));
        // The desktop's own display is not the screen's.
        assert_ne!(
            std::env::var("DISPLAY").ok().as_deref(),
            Some(info.name.as_str())
        );

        let pid = screens
            .launch(
                &c,
                info.id,
                xterm,
                ["-geometry", "60x15+0+0", "-bg", "white", "-fg", "black"]
                    .map(String::from)
                    .to_vec(),
            )
            .await
            .unwrap();
        assert_eq!(screens.list(&c).await.unwrap()[0].pids, [pid]);
        tokio::time::sleep(Duration::from_millis(1500)).await;

        let first = read_png(&screens.capture(&c, info.id).await.unwrap());
        assert_eq!((first.1, first.2), (800, 600));
        assert_eq!(pixel(&first, 8, 8), [255, 255, 255], "xterm's white corner");

        screens.click(&c, info.id, 100, 100).await.unwrap();
        screens
            .type_text(&c, info.id, "echo hello from bricolo\n".into())
            .await
            .unwrap();
        screens
            .press_key(&c, info.id, "ctrl+l".into())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;
        let second = read_png(&screens.capture(&c, info.id).await.unwrap());
        assert_ne!(first.0, second.0, "typing changed nothing on the screen");
        assert!(
            screens.click(&c, info.id, 800, 10).await.is_err(),
            "outside the screen"
        );

        screens.destroy(&c, info.id).await.unwrap();
        // SAFETY: signal 0 only checks that the process exists.
        let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
        assert!(!alive, "xterm outlived its screen");
        let start = Instant::now();
        while xvfb::in_use(number) && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!xvfb::in_use(number), "Xvfb left its socket behind");
    }
}
