//! `mouse` on X11: fake pointer input through the XTEST extension, on a
//! connection opened for each call. The position comes from `QueryPointer`.
//! Scrolling presses buttons 4 and 5 (up and down) and 6 and 7 (left and
//! right). On Wayland and without a display it answers `unavailable`.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use async_trait::async_trait;
use x11rb::connection::{Connection, RequestConnection as _};
use x11rb::protocol::xproto::{
    BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, MOTION_NOTIFY_EVENT,
};
use x11rb::protocol::xtest::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::{Session, warn_once};
use crate::caller::Caller;
use crate::error::CallError;
use crate::mouse::{self, MouseButton, MouseProvider, Point};

/// A pause between the clicks of a double or triple click.
const GAP: Duration = Duration::from_millis(30);
/// The most wheel notches one call sends, so a typo cannot spin for long.
const MAX_NOTCHES: u32 = 1000;

#[derive(Default)]
pub struct X11Mouse {
    warned: AtomicBool,
}

/// The X button number of `button`.
pub fn button_number(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 3,
    }
}

/// The wheel button and how many times to press it for a scroll of `delta`
/// lines: `negative` for a negative delta, `positive` for a positive one.
pub fn wheel(delta: i32, negative: u8, positive: u8) -> Option<(u8, u32)> {
    match delta {
        0 => None,
        d if d < 0 => Some((negative, d.unsigned_abs().min(MAX_NOTCHES))),
        d => Some((positive, d.unsigned_abs().min(MAX_NOTCHES))),
    }
}

fn failed(e: impl std::fmt::Display) -> CallError {
    CallError::Failed(format!("X11: {e}"))
}

fn open() -> Result<(RustConnection, usize), CallError> {
    let (conn, screen) = x11rb::connect(None).map_err(failed)?;
    if conn
        .extension_information(xtest::X11_EXTENSION_NAME)
        .map_err(failed)?
        .is_none()
    {
        return Err(CallError::Failed(
            "the X server has no XTEST extension".into(),
        ));
    }
    Ok((conn, screen))
}

pub(super) fn button_press(conn: &RustConnection, button: u8) -> Result<(), CallError> {
    for kind in [BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT] {
        conn.xtest_fake_input(kind, button, 0, x11rb::NONE, 0, 0, 0)
            .map_err(failed)?;
    }
    Ok(())
}

impl X11Mouse {
    fn check_session(&self) -> Result<(), CallError> {
        match Session::current() {
            Session::X11 => Ok(()),
            Session::Wayland => {
                warn_once(
                    &self.warned,
                    "mouse: this is a Wayland session; synthetic pointer input needs X11, so \
                     mouse answers unavailable (see docs/linux.md)",
                );
                Err(CallError::Unavailable)
            }
            Session::Headless => {
                warn_once(
                    &self.warned,
                    "mouse: no DISPLAY, so mouse answers unavailable",
                );
                Err(CallError::Unavailable)
            }
        }
    }

    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&RustConnection, usize) -> Result<T, CallError> + Send + 'static,
    ) -> Result<T, CallError> {
        self.check_session()?;
        tokio::task::spawn_blocking(move || {
            let (conn, screen) = open()?;
            let out = work(&conn, screen)?;
            conn.sync().map_err(failed)?;
            Ok(out)
        })
        .await
        .map_err(CallError::failed)?
    }
}

#[async_trait]
impl MouseProvider for X11Mouse {
    async fn position(&self, _: &Caller) -> Result<Point, CallError> {
        self.run(|conn, screen| {
            let root = conn.setup().roots[screen].root;
            let p = conn
                .query_pointer(root)
                .map_err(failed)?
                .reply()
                .map_err(failed)?;
            Ok(Point {
                x: i32::from(p.root_x),
                y: i32::from(p.root_y),
            })
        })
        .await
    }

    async fn move_to(&self, _: &Caller, to: Point) -> Result<(), CallError> {
        let x = i16::try_from(to.x).map_err(|_| CallError::Failed("x is out of range".into()))?;
        let y = i16::try_from(to.y).map_err(|_| CallError::Failed("y is out of range".into()))?;
        self.run(move |conn, screen| {
            let root = conn.setup().roots[screen].root;
            // detail 0 with an absolute move: `x` and `y` are root coordinates.
            conn.xtest_fake_input(MOTION_NOTIFY_EVENT, 0, 0, root, x, y, 0)
                .map_err(failed)?;
            Ok(())
        })
        .await
    }

    async fn click(&self, _: &Caller, button: MouseButton, count: u8) -> Result<(), CallError> {
        mouse::check_click_count(count)?;
        self.run(move |conn, _| {
            for n in 0..count {
                if n > 0 {
                    conn.sync().map_err(failed)?;
                    std::thread::sleep(GAP);
                }
                button_press(conn, button_number(button))?;
            }
            Ok(())
        })
        .await
    }

    async fn scroll(&self, _: &Caller, dx: i32, dy: i32) -> Result<(), CallError> {
        self.run(move |conn, _| {
            // Button 4 scrolls up and 5 down; 6 left and 7 right.
            for (button, times) in [wheel(dy, 4, 5), wheel(dx, 6, 7)].into_iter().flatten() {
                for _ in 0..times {
                    button_press(conn, button)?;
                }
            }
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_use_x_numbers() {
        assert_eq!(button_number(MouseButton::Left), 1);
        assert_eq!(button_number(MouseButton::Middle), 2);
        assert_eq!(button_number(MouseButton::Right), 3);
    }

    #[test]
    fn scrolling_picks_the_wheel_button() {
        assert_eq!(wheel(0, 4, 5), None);
        assert_eq!(wheel(-3, 4, 5), Some((4, 3)));
        assert_eq!(wheel(2, 4, 5), Some((5, 2)));
        assert_eq!(wheel(i32::MIN, 6, 7), Some((6, MAX_NOTCHES)));
    }
}
