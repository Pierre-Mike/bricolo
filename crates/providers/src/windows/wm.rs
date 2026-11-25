//! Top-level windows and their processes, shared by `window`, `app` and
//! `screen`: the window list as Alt+Tab shows it, titles, owning apps,
//! DWM frames, bringing a window to the front, and DPI awareness.
//!
//! The process is made per-monitor DPI aware (v2) once, before any of
//! these calls, so every coordinate is in physical pixels of the virtual
//! screen, whose origin is the primary monitor's top-left corner.

use std::ffi::c_void;
use std::sync::Once;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, GetExitCodeProcess, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, FindWindowExW, GA_ROOTOWNER, GW_OWNER, GWL_EXSTYLE, GetAncestor,
    GetClassNameW, GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, SW_RESTORE, SetForegroundWindow, ShowWindow, WS_EX_APPWINDOW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, w};

use super::input;
use crate::error::CallError;
use crate::win::{self, Edges, WindowTraits};
use crate::window::{Rect, WindowInfo};

/// `GetExitCodeProcess`'s answer for a process that still runs.
const STILL_ACTIVE: u32 = 0x103;

/// Makes the process per-monitor DPI aware (v2), once. Fails harmlessly
/// when a manifest or an earlier call already set it.
pub fn ensure_dpi_aware() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: a process-wide setting with a predefined context.
        if let Err(e) =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
        {
            tracing::debug!("cannot set per-monitor DPI awareness: {e}");
        }
    });
}

pub fn failed(e: impl std::fmt::Display) -> CallError {
    CallError::Failed(format!("Win32: {e}"))
}

pub fn hwnd(id: u64) -> HWND {
    HWND(id as usize as *mut c_void)
}

pub fn id(hwnd: HWND) -> u64 {
    hwnd.0 as usize as u64
}

fn text(buf: &[u16], len: i32) -> String {
    let len = usize::try_from(len).unwrap_or(0).min(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

pub fn title(hwnd: HWND) -> String {
    // SAFETY: plain reads of another window's title, into our buffer.
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; usize::try_from(len).unwrap_or(0) + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        text(&buf, n)
    }
}

pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: writes at most the buffer's length.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    text(&buf, n)
}

fn cloaked(hwnd: HWND) -> bool {
    let mut value = 0u32;
    // SAFETY: DWMWA_CLOAKED is a DWORD.
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut value).cast(),
            size_of::<u32>() as u32,
        )
    }
    .is_ok_and(|()| value != 0)
}

pub fn traits(hwnd: HWND) -> WindowTraits {
    // SAFETY: reads of a window's state; a closed window reads as empty.
    let (visible, ex, owned) = unsafe {
        (
            IsWindowVisible(hwnd).as_bool(),
            GetWindowLongW(hwnd, GWL_EXSTYLE) as u32,
            GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()),
        )
    };
    WindowTraits {
        visible,
        cloaked: visible && cloaked(hwnd),
        tool_window: ex & WS_EX_TOOLWINDOW.0 != 0,
        app_window: ex & WS_EX_APPWINDOW.0 != 0,
        no_activate: ex & WS_EX_NOACTIVATE.0 != 0,
        owned,
        class: class_name(hwnd),
        title: title(hwnd),
    }
}

pub fn is_app_window(hwnd: HWND) -> bool {
    !hwnd.is_invalid() && win::is_app_window(&traits(hwnd))
}

unsafe extern "system" fn collect(hwnd: HWND, out: LPARAM) -> BOOL {
    // SAFETY: `out` is the Vec that `top_windows` passes, alive for the
    // whole enumeration.
    let list = unsafe { &mut *(out.0 as *mut Vec<HWND>) };
    list.push(hwnd);
    BOOL(1)
}

/// Every top-level window, front to back.
pub fn top_windows() -> Result<Vec<HWND>, CallError> {
    let mut list: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into `list`, which outlives the call.
    unsafe { EnumWindows(Some(collect), LPARAM(&raw mut list as isize)) }.map_err(failed)?;
    Ok(list)
}

/// The windows Alt+Tab shows, front to back.
pub fn app_windows() -> Result<Vec<HWND>, CallError> {
    Ok(top_windows()?
        .into_iter()
        .filter(|w| is_app_window(*w))
        .collect())
}

/// The window `id`, if it is one of the app windows: plugins act on
/// those only, never on the desktop, the taskbar or a hidden window.
pub fn app_window(id: u64) -> Result<HWND, CallError> {
    let w = hwnd(id);
    // SAFETY: IsWindow accepts any value.
    if id != 0 && unsafe { IsWindow(Some(w)) }.as_bool() && is_app_window(w) {
        Ok(w)
    } else {
        Err(CallError::Failed("no such window".into()))
    }
}

/// The process that owns the window. A Store app's frame belongs to
/// ApplicationFrameHost; the app itself owns the CoreWindow inside it.
pub fn pid(hwnd: HWND) -> u32 {
    let mut owner = hwnd;
    if class_name(hwnd) == "ApplicationFrameWindow" {
        // SAFETY: a lookup among the frame's children.
        if let Ok(core) =
            unsafe { FindWindowExW(Some(hwnd), None, w!("Windows.UI.Core.CoreWindow"), None) }
            && !core.is_invalid()
        {
            owner = core;
        }
    }
    let mut pid = 0u32;
    // SAFETY: writes the process id into `pid`.
    unsafe { GetWindowThreadProcessId(owner, Some(&raw mut pid)) };
    pid
}

/// The full path of the process's program. Protected processes refuse
/// even the limited query.
pub fn process_path(pid: u32) -> Option<String> {
    // SAFETY: the handle is closed below; the buffer outlives the call.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &raw mut len,
        );
        let _ = CloseHandle(process);
        result.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// The app's name: its program's file name without `.exe`, or "unknown"
/// when the process can't be read.
pub fn app_name(pid: u32) -> String {
    process_path(pid).map_or_else(|| "unknown".into(), |p| win::app_name(&p))
}

/// Whether the process still runs.
pub fn alive(pid: u32) -> bool {
    // SAFETY: the handle is closed below.
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(process, &raw mut code).is_ok();
        let _ = CloseHandle(process);
        ok && code == STILL_ACTIVE
    }
}

pub fn edges(r: RECT) -> Edges {
    Edges {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

/// `GetWindowRect`: the frame with the invisible resize borders.
pub fn outer(hwnd: HWND) -> Result<Edges, CallError> {
    let mut r = RECT::default();
    // SAFETY: writes into `r`.
    unsafe { GetWindowRect(hwnd, &raw mut r) }.map_err(failed)?;
    Ok(edges(r))
}

/// The frame as the user sees it, from DWM, or the window rectangle when
/// DWM has no answer.
pub fn visible(hwnd: HWND) -> Result<Edges, CallError> {
    let mut r = RECT::default();
    // SAFETY: DWMWA_EXTENDED_FRAME_BOUNDS is a RECT.
    let dwm = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut r).cast(),
            size_of::<RECT>() as u32,
        )
    };
    match dwm {
        Ok(()) => Ok(edges(r)),
        Err(_) => outer(hwnd),
    }
}

pub fn frame(hwnd: HWND) -> Result<Rect, CallError> {
    Ok(visible(hwnd)?.to_rect())
}

pub fn info(hwnd: HWND) -> Result<WindowInfo, CallError> {
    Ok(WindowInfo {
        id: id(hwnd),
        app: app_name(pid(hwnd)),
        title: title(hwnd),
        frame: frame(hwnd)?,
    })
}

/// The foreground window as the window list knows it: itself, or the
/// app window that owns it (a dialog's main window). None for the desktop
/// or the taskbar.
pub fn foreground() -> Option<HWND> {
    // SAFETY: plain reads.
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return None;
    }
    if is_app_window(fg) {
        return Some(fg);
    }
    // SAFETY: a plain read.
    let root = unsafe { GetAncestor(fg, GA_ROOTOWNER) };
    is_app_window(root).then_some(root)
}

/// Restores the window if it is minimized and brings it to the front.
/// Windows lets a process take the foreground only when it got the last
/// input event, so when the plain call is refused this taps Alt (which
/// counts as our input) and attaches to the foreground thread's input
/// first, the usual workaround.
pub fn focus(hwnd: HWND) -> Result<(), CallError> {
    // SAFETY: calls on a window handle that `app_window` checked; the
    // input attachment is undone before returning.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
            return Ok(());
        }
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        let attached =
            fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, true).as_bool();
        // VK_MENU: a tap of Alt.
        let _ = input::send(&[
            input::virtual_key(0x12, false),
            input::virtual_key(0x12, true),
        ]);
        let _ = BringWindowToTop(hwnd);
        let ok = SetForegroundWindow(hwnd).as_bool();
        if attached {
            let _ = AttachThreadInput(me, fg_thread, false);
        }
        if !ok {
            tracing::debug!("window: Windows kept another window in front");
        }
    }
    Ok(())
}
