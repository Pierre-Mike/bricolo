//! `mouse` on Windows: `GetCursorPos` and `SetCursorPos` in physical
//! pixels of the virtual screen (the process is per-monitor DPI aware),
//! clicks and wheel notches through `SendInput`. One wheel notch scrolls
//! what the user set in Mouse settings, three lines by default.

use std::time::Duration;

use async_trait::async_trait;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOUSE_EVENT_FLAGS, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_WHEEL,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

use super::input;
use super::window::blocking;
use super::wm::{self, failed};
use crate::caller::Caller;
use crate::error::CallError;
use crate::mouse::{self, MouseButton, MouseProvider, Point};
use crate::win;

/// A pause between the clicks of a double or triple click.
const GAP: Duration = Duration::from_millis(30);

pub struct WinMouse;

impl WinMouse {
    pub fn new() -> Self {
        wm::ensure_dpi_aware();
        Self
    }
}

fn button_flags(button: MouseButton) -> (MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS) {
    match button {
        MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
    }
}

#[async_trait]
impl MouseProvider for WinMouse {
    async fn position(&self, _: &Caller) -> Result<Point, CallError> {
        blocking(|| {
            let mut p = POINT::default();
            // SAFETY: writes into `p`.
            unsafe { GetCursorPos(&raw mut p) }.map_err(failed)?;
            Ok(Point { x: p.x, y: p.y })
        })
        .await
    }

    async fn move_to(&self, _: &Caller, to: Point) -> Result<(), CallError> {
        // SAFETY: moves the pointer; Windows clips it to the screens.
        blocking(move || unsafe { SetCursorPos(to.x, to.y) }.map_err(failed)).await
    }

    async fn click(&self, _: &Caller, button: MouseButton, count: u8) -> Result<(), CallError> {
        mouse::check_click_count(count)?;
        let (down, up) = button_flags(button);
        blocking(move || {
            for n in 0..count {
                if n > 0 {
                    std::thread::sleep(GAP);
                }
                input::send(&[input::mouse(down, 0), input::mouse(up, 0)])?;
            }
            Ok(())
        })
        .await
    }

    async fn scroll(&self, _: &Caller, dx: i32, dy: i32) -> Result<(), CallError> {
        blocking(move || {
            let axes = [
                (MOUSEEVENTF_WHEEL, win::wheel(dy, true)),
                (MOUSEEVENTF_HWHEEL, win::wheel(dx, false)),
            ];
            for (flag, notches) in axes {
                if let Some((data, times)) = notches {
                    let inputs: Vec<_> = (0..times).map(|_| input::mouse(flag, data)).collect();
                    input::send(&inputs)?;
                }
            }
            Ok(())
        })
        .await
    }
}
