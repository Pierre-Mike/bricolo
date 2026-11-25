//! `window` on Windows. The list is what Alt+Tab shows (see
//! `win::is_app_window`), front to back; a window's id is its HWND and
//! its app the program's file name without `.exe`. Frames are DWM's
//! visible bounds, in physical pixels (the process is per-monitor DPI
//! aware), so they leave out the invisible resize borders; `set-frame`
//! adds those back. Screens come from `EnumDisplayMonitors`: the work
//! area is the visible frame and the primary monitor is `main`.

use std::time::Duration;

use async_trait::async_trait;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, IsZoomed, MONITORINFOF_PRIMARY, PostMessageW, SW_MINIMIZE, SW_RESTORE,
    SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, ShowWindow, WM_CLOSE,
};
use windows::core::BOOL;

use super::wm::{self, failed};
use crate::caller::Caller;
use crate::error::CallError;
use crate::win;
use crate::window::{Rect, Screen, WindowInfo, WindowProvider};

/// A window that moves to a monitor of another scale resizes itself; it
/// gets this long before the frame is checked and set again.
const SETTLE: Duration = Duration::from_millis(50);

pub struct WinWindows;

impl WinWindows {
    pub fn new() -> Self {
        wm::ensure_dpi_aware();
        Self
    }
}

/// Runs Win32 work on the blocking pool.
pub async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, CallError> + Send + 'static,
) -> Result<T, CallError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(CallError::failed)?
}

/// Moves and resizes so the visible frame is `frame`.
fn place(hwnd: HWND, frame: Rect) -> Result<(), CallError> {
    let (x, y, w, h) = win::compensate(frame, wm::outer(hwnd)?, wm::visible(hwnd)?);
    // SAFETY: a window `app_window` checked.
    unsafe { SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE) }.map_err(failed)
}

fn set_frame(hwnd: HWND, frame: Rect) -> Result<(), CallError> {
    // SAFETY: a window `app_window` checked.
    unsafe {
        if IsZoomed(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
    }
    place(hwnd, frame)?;
    // The borders differ between scales, and a per-monitor aware app
    // resizes itself on the new monitor: check once and set again.
    std::thread::sleep(SETTLE);
    if wm::frame(hwnd)? != frame {
        place(hwnd, frame)?;
    }
    Ok(())
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    out: LPARAM,
) -> BOOL {
    // SAFETY: `out` is the Vec that `screens` passes, alive for the call.
    let list = unsafe { &mut *(out.0 as *mut Vec<HMONITOR>) };
    list.push(monitor);
    BOOL(1)
}

fn screens() -> Result<Vec<Screen>, CallError> {
    let mut monitors: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only pushes into `monitors`, which outlives it.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&raw mut monitors as isize),
        )
    };
    if !ok.as_bool() {
        return Err(CallError::Failed("cannot list the monitors".into()));
    }
    let mut out = Vec::new();
    for (i, monitor) in monitors.into_iter().enumerate() {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: MONITORINFOEXW starts with MONITORINFO, and cbSize says
        // which one it is.
        let ok = unsafe { GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>()) };
        if !ok.as_bool() {
            continue;
        }
        let len = info
            .szDevice
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(info.szDevice.len());
        let device = String::from_utf16_lossy(&info.szDevice[..len]);
        out.push(Screen {
            id: win::monitor_id(&device, i),
            frame: wm::edges(info.monitorInfo.rcMonitor).to_rect(),
            visible_frame: wm::edges(info.monitorInfo.rcWork).to_rect(),
            main: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        });
    }
    if !out.is_empty() && !out.iter().any(|s| s.main) {
        out[0].main = true;
    }
    Ok(out)
}

#[async_trait]
impl WindowProvider for WinWindows {
    async fn list(&self, _: &Caller) -> Result<Vec<WindowInfo>, CallError> {
        blocking(|| {
            let mut out = Vec::new();
            for hwnd in wm::app_windows()? {
                // A window can close while we read it.
                if let Ok(info) = wm::info(hwnd) {
                    out.push(info);
                }
            }
            Ok(out)
        })
        .await
    }

    async fn focused(&self, _: &Caller) -> Result<Option<WindowInfo>, CallError> {
        blocking(|| Ok(wm::foreground().and_then(|w| wm::info(w).ok()))).await
    }

    async fn focus(&self, _: &Caller, id: u64) -> Result<(), CallError> {
        blocking(move || wm::focus(wm::app_window(id)?)).await
    }

    async fn set_frame(&self, _: &Caller, id: u64, frame: Rect) -> Result<(), CallError> {
        blocking(move || set_frame(wm::app_window(id)?, frame)).await
    }

    async fn close(&self, _: &Caller, id: u64) -> Result<(), CallError> {
        blocking(move || {
            let hwnd = wm::app_window(id)?;
            // SAFETY: posts a message; the window handles it on its thread.
            unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.map_err(failed)
        })
        .await
    }

    async fn set_minimized(&self, _: &Caller, id: u64, minimized: bool) -> Result<(), CallError> {
        blocking(move || {
            let hwnd = wm::app_window(id)?;
            if minimized {
                // SAFETY: a window `app_window` checked.
                unsafe {
                    let _ = ShowWindow(hwnd, SW_MINIMIZE);
                }
                Ok(())
            } else {
                // Restores it and brings it to the front.
                wm::focus(hwnd)
            }
        })
        .await
    }

    async fn screens(&self, _: &Caller) -> Result<Vec<Screen>, CallError> {
        blocking(screens).await
    }
}
