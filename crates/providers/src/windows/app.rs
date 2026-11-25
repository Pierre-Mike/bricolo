//! `app` on Windows. An app is a program with windows in the Alt+Tab list,
//! named by its file name without `.exe` ("notepad", "Code"); names match
//! in any case, with or without `.exe`. `bundle-id` is always none.
//!
//! `launch` first brings an app of that name to the front if one runs, as
//! `open -a` does. Otherwise it starts the first of: a Start Menu shortcut
//! of that name (the user's, then everyone's), the program an App Paths
//! entry names (HKCU, then HKLM), or `<name>.exe` on `PATH`, through
//! `ShellExecuteW`. `open-url` hands the URL to `ShellExecuteW`; `file:`
//! answers `denied`, as elsewhere.
//!
//! `quit` posts `WM_CLOSE` to each of the app's windows, as their close
//! buttons do, so the app can save or ask first; it never kills the
//! process. `set-hidden` minimizes every window, or restores the minimized
//! ones and brings the frontmost back. Both answer `failed` with "no such
//! app" when no window matches.
//!
//! `watch` starts one thread with a message loop: a WinEvent hook on
//! `EVENT_SYSTEM_FOREGROUND` reports the app that comes to the front, and a
//! one-second timer compares the apps with windows to report launches and
//! exits (an app counts as launched when its first window shows, and as
//! terminated when its process exits).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GetMessageW, IsIconic, MSG, PostMessageW,
    SW_MINIMIZE, SW_RESTORE, SW_SHOWNORMAL, SetTimer, ShowWindow, TranslateMessage,
    WINEVENT_OUTOFCONTEXT, WM_CLOSE, WM_TIMER,
};
use windows::core::{HSTRING, PCWSTR, w};

use super::window::blocking;
use super::wm::{self, failed};
use crate::app::{AppInfo, AppProvider, check_url};
use crate::caller::Caller;
use crate::error::CallError;
use crate::event::{AppChange, AppEvent, EventSink, ProviderEvent};
use crate::win;

/// How often the watcher compares the apps with windows.
const POLL_MS: u32 = 1000;

/// The plugins that called `watch(true)`.
type Watchers = Arc<Mutex<BTreeSet<String>>>;

pub struct WinApps {
    sink: EventSink,
    watchers: Watchers,
    /// Whether the watcher thread runs; it starts on the first `watch`.
    started: Mutex<bool>,
}

impl WinApps {
    pub fn new(sink: EventSink) -> Self {
        wm::ensure_dpi_aware();
        Self {
            sink,
            watchers: Watchers::default(),
            started: Mutex::new(false),
        }
    }

    fn start_watcher(&self) -> Result<(), CallError> {
        let mut started = self.started.lock().unwrap();
        if *started {
            return Ok(());
        }
        let (sink, watchers) = (self.sink.clone(), self.watchers.clone());
        std::thread::Builder::new()
            .name("bricolo-app-watch".into())
            .spawn(move || watch_loop(sink, watchers))
            .map_err(CallError::failed)?;
        *started = true;
        Ok(())
    }
}

fn info_of(hwnd: HWND) -> AppInfo {
    let pid = wm::pid(hwnd);
    AppInfo {
        name: wm::app_name(pid),
        bundle_id: None,
        pid: i32::try_from(pid).unwrap_or(0),
    }
}

/// The apps with windows in the Alt+Tab list, frontmost first, once each.
fn running() -> Result<Vec<AppInfo>, CallError> {
    let mut apps: Vec<AppInfo> = Vec::new();
    for hwnd in wm::app_windows()? {
        let pid = wm::pid(hwnd);
        if apps.iter().any(|a| a.pid == pid as i32) {
            continue;
        }
        apps.push(AppInfo {
            name: wm::app_name(pid),
            bundle_id: None,
            pid: pid as i32,
        });
    }
    Ok(apps)
}

/// The app windows of the app named `app`, front to back.
fn windows_of(app: &str) -> Result<Vec<HWND>, CallError> {
    let app = win::check_app_name(app)?;
    let mut names: BTreeMap<u32, String> = BTreeMap::new();
    let mut out = Vec::new();
    for hwnd in wm::app_windows()? {
        let pid = wm::pid(hwnd);
        let name = names.entry(pid).or_insert_with(|| wm::app_name(pid));
        if win::same_app(name, app) {
            out.push(hwnd);
        }
    }
    Ok(out)
}

fn windows_or_fail(app: &str) -> Result<Vec<HWND>, CallError> {
    let windows = windows_of(app)?;
    if windows.is_empty() {
        return Err(CallError::Failed("no such app".into()));
    }
    Ok(windows)
}

/// The default value of an App Paths key, under HKCU then HKLM.
fn app_path(name: &str) -> Option<String> {
    let key = HSTRING::from(win::app_paths_key(name));
    [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
        .into_iter()
        .find_map(|root| read_default(root, &key))
}

fn read_default(root: HKEY, key: &HSTRING) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: `size` is the buffer's size in bytes; REG_EXPAND_SZ values
    // come back expanded.
    let result = unsafe {
        RegGetValueW(
            root,
            key,
            PCWSTR::null(),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&raw mut size),
        )
    };
    if result.is_err() {
        return None;
    }
    let len = (size as usize / 2).min(buf.len());
    let value = String::from_utf16_lossy(&buf[..len]);
    let value = win::unquote(value.trim_end_matches('\0'));
    (!value.is_empty()).then(|| value.to_string())
}

/// What `launch` starts for `app`: a shortcut, an App Paths program or a
/// program on `PATH`.
fn resolve(app: &str) -> Option<String> {
    let dirs = win::start_menu_dirs(
        std::env::var_os("APPDATA").as_deref(),
        std::env::var_os("ProgramData").as_deref(),
    );
    if let Some(link) = win::find_shortcut(app, &dirs) {
        return Some(link.display().to_string());
    }
    if let Some(path) = app_path(app) {
        return Some(path);
    }
    let path = std::env::var_os("PATH")?;
    win::find_on_path(app, &path).map(|p| p.display().to_string())
}

/// Opens `target` (a program, a shortcut or a URL) as Explorer would.
/// Shell extensions may need COM, so the thread joins an apartment for
/// the call.
fn shell_open(target: &str) -> Result<(), CallError> {
    let file = HSTRING::from(target);
    // SAFETY: COM is set up and torn down on this thread around the call;
    // the strings outlive it.
    let code = unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).is_ok();
        let result = ShellExecuteW(None, w!("open"), &file, None, None, SW_SHOWNORMAL);
        if com {
            CoUninitialize();
        }
        result.0 as isize
    };
    // Values above 32 mean success.
    if code > 32 {
        Ok(())
    } else {
        Err(CallError::Failed(format!(
            "cannot open {target}: ShellExecute error {code}"
        )))
    }
}

/// The state the watcher thread's callbacks read.
struct WatchState {
    sink: EventSink,
    watchers: Watchers,
    last_active: Option<i32>,
}

thread_local! {
    static WATCH: RefCell<Option<WatchState>> = const { RefCell::new(None) };
}

fn emit(state: &WatchState, change: AppChange, app: &AppInfo) {
    let plugins: Vec<String> = state.watchers.lock().unwrap().iter().cloned().collect();
    for plugin in plugins {
        let event = AppEvent {
            change,
            app: app.clone(),
        };
        (state.sink)(&plugin, ProviderEvent::App(event));
    }
}

unsafe extern "system" fn on_foreground(
    _: HWINEVENTHOOK,
    _: u32,
    _: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    // The event's window may be a dialog; report the app window's app.
    let Some(hwnd) = wm::foreground() else { return };
    let app = info_of(hwnd);
    WATCH.with_borrow_mut(|state| {
        let Some(state) = state else { return };
        if state.last_active == Some(app.pid) {
            return;
        }
        state.last_active = Some(app.pid);
        emit(state, AppChange::Activated, &app);
    });
}

fn watch_loop(sink: EventSink, watchers: Watchers) {
    WATCH.with_borrow_mut(|state| {
        *state = Some(WatchState {
            sink,
            watchers,
            last_active: None,
        });
    });
    // The first look sets what runs already; it reports nothing.
    let mut known: BTreeMap<i32, AppInfo> = BTreeMap::new();
    if let Ok(apps) = running() {
        win::diff_apps(&mut known, &apps, |_| true);
    }
    // SAFETY: an out-of-context hook delivers its callbacks through this
    // thread's message loop below; the timer posts WM_TIMER to it.
    unsafe {
        let hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
        if hook.is_invalid() {
            tracing::warn!("app: cannot watch the foreground window; no activate events");
        }
        if SetTimer(None, 0, POLL_MS, None) == 0 {
            tracing::warn!("app: cannot start the timer; no launch or quit events");
        }
        let mut msg = MSG::default();
        while GetMessageW(&raw mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_TIMER {
                poll(&mut known);
                continue;
            }
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
}

fn poll(known: &mut BTreeMap<i32, AppInfo>) {
    let Ok(apps) = running() else { return };
    let changes = win::diff_apps(known, &apps, |pid| u32::try_from(pid).is_ok_and(wm::alive));
    if changes.is_empty() {
        return;
    }
    WATCH.with_borrow(|state| {
        if let Some(state) = state {
            for (change, app) in &changes {
                emit(state, *change, app);
            }
        }
    });
}

#[async_trait]
impl AppProvider for WinApps {
    async fn launch(&self, _: &Caller, app: String) -> Result<(), CallError> {
        blocking(move || {
            let app = win::check_app_name(&app)?.to_string();
            if let Some(front) = windows_of(&app)?.first() {
                return wm::focus(*front);
            }
            let target = resolve(&app).ok_or_else(|| {
                CallError::Failed(format!(
                    "no Start Menu shortcut, App Paths entry or program on PATH named {app}"
                ))
            })?;
            shell_open(&target)
        })
        .await
    }

    async fn open_url(&self, _: &Caller, url: String) -> Result<(), CallError> {
        check_url(&url)?;
        blocking(move || shell_open(&url)).await
    }

    async fn running(&self, _: &Caller) -> Result<Vec<AppInfo>, CallError> {
        blocking(running).await
    }

    async fn frontmost(&self, _: &Caller) -> Result<Option<AppInfo>, CallError> {
        blocking(|| Ok(wm::foreground().map(info_of))).await
    }

    async fn watch(&self, caller: &Caller, on: bool) -> Result<(), CallError> {
        if on {
            self.start_watcher()?;
            self.watchers.lock().unwrap().insert(caller.plugin.clone());
        } else {
            self.watchers.lock().unwrap().remove(&caller.plugin);
        }
        Ok(())
    }

    fn release(&self, plugin: &str) {
        self.watchers.lock().unwrap().remove(plugin);
    }

    async fn quit(&self, _: &Caller, app: String) -> Result<(), CallError> {
        blocking(move || {
            for hwnd in windows_or_fail(&app)? {
                // SAFETY: posts a message; the window handles it on its
                // own thread.
                unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }
                    .map_err(failed)?;
            }
            Ok(())
        })
        .await
    }

    async fn set_hidden(&self, _: &Caller, app: String, hidden: bool) -> Result<(), CallError> {
        blocking(move || {
            let windows = windows_or_fail(&app)?;
            if hidden {
                for hwnd in &windows {
                    // SAFETY: a window from the app window list.
                    unsafe {
                        let _ = ShowWindow(*hwnd, SW_MINIMIZE);
                    }
                }
                return Ok(());
            }
            // Back to front, so the frontmost ends on top.
            for hwnd in windows.iter().rev() {
                // SAFETY: a window from the app window list. Only
                // minimized ones: SW_RESTORE also un-maximizes.
                unsafe {
                    if IsIconic(*hwnd).as_bool() {
                        let _ = ShowWindow(*hwnd, SW_RESTORE);
                    }
                }
            }
            wm::focus(windows[0])
        })
        .await
    }
}
