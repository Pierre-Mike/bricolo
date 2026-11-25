//! `system` on Windows.
//!
//! - `idle-seconds`: `GetLastInputInfo` against `GetTickCount`, for the
//!   session the node runs in.
//! - `power`: `GetSystemPowerStatus`.
//! - `keep-awake`: `SetThreadExecutionState(ES_CONTINUOUS |
//!   ES_SYSTEM_REQUIRED)`. The state belongs to the thread that set it, so
//!   one thread, `bricolo-keep-awake`, holds it while any plugin asks for it and
//!   clears it when the last one lets go. Like `caffeinate -i`, it stops
//!   idle sleep only: the display may still turn off, and a sleep the user
//!   asks for still happens.
//! - `watch`: a hidden top-level window on the thread `bricolo-system-watch`,
//!   with its own message loop. `WM_POWERBROADCAST` gives `will-sleep` and
//!   `did-wake`, `WM_WTSSESSION_CHANGE` (after
//!   `WTSRegisterSessionNotification`) `screen-locked` and
//!   `screen-unlocked`, and `WM_DISPLAYCHANGE` `screens-changed`. The
//!   mapping is [`windows_message_event`], tested on every system.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use ::windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use ::windows::Win32::System::LibraryLoader::GetModuleHandleW;
use ::windows::Win32::System::Power::{
    ES_CONTINUOUS, ES_SYSTEM_REQUIRED, EXECUTION_STATE, GetSystemPowerStatus, SYSTEM_POWER_STATUS,
    SetThreadExecutionState,
};
use ::windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use ::windows::Win32::System::SystemInformation::GetTickCount;
use ::windows::Win32::System::Threading::GetCurrentThreadId;
use ::windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use ::windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG,
    PostThreadMessageW, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_QUIT,
    WNDCLASSW,
};
use ::windows::core::w;
use async_trait::async_trait;

use crate::caller::Caller;
use crate::error::CallError;
use crate::event::{EventSink, ProviderEvent, SystemEvent};
use crate::system::{
    Power, SystemProvider, idle_from_ticks, power_from_status, win_msg, windows_message_event,
};

type Watchers = Arc<Mutex<BTreeSet<String>>>;

pub struct WinSystem {
    sink: EventSink,
    watchers: Watchers,
    /// The message thread's id once it runs, or why it could not start.
    watch_thread: Mutex<Option<Result<u32, String>>>,
    /// Plugins that asked to stay awake.
    awake: Mutex<BTreeSet<String>>,
    /// Tells `bricolo-keep-awake` whether to hold the state.
    keeper: Mutex<Option<mpsc::Sender<bool>>>,
}

impl WinSystem {
    pub fn new(sink: EventSink) -> Self {
        Self {
            sink,
            watchers: Arc::default(),
            watch_thread: Mutex::new(None),
            awake: Mutex::default(),
            keeper: Mutex::new(None),
        }
    }

    /// Sends the wanted state to the keep-awake thread, starting it first.
    fn hold(&self, on: bool) -> Result<(), CallError> {
        let mut keeper = self.keeper.lock().unwrap();
        if let Some(tx) = &*keeper
            && tx.send(on).is_ok()
        {
            return Ok(());
        }
        if !on {
            // No thread, nothing held.
            *keeper = None;
            return Ok(());
        }
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("bricolo-keep-awake".into())
            .spawn(move || keep_awake_thread(rx))
            .map_err(CallError::failed)?;
        tx.send(on).map_err(CallError::failed)?;
        *keeper = Some(tx);
        Ok(())
    }

    /// Starts the message thread once; later calls give the same answer.
    fn ensure_watching(&self) -> Result<(), CallError> {
        let mut slot = self.watch_thread.lock().unwrap();
        if slot.is_none() {
            let (sink, watchers) = (self.sink.clone(), self.watchers.clone());
            let forward = move |event: SystemEvent| {
                let names: Vec<String> = watchers.lock().unwrap().iter().cloned().collect();
                for plugin in names {
                    sink(&plugin, ProviderEvent::System(event));
                }
            };
            *slot = Some(start_watch_thread(Box::new(forward)));
        }
        match slot.as_ref() {
            Some(Ok(_)) => Ok(()),
            Some(Err(e)) => Err(CallError::Failed(e.clone())),
            None => Err(CallError::Unavailable),
        }
    }
}

fn keep_awake_thread(rx: mpsc::Receiver<bool>) {
    let set = |flags: EXECUTION_STATE| {
        // SAFETY: changes this thread's execution state only.
        if unsafe { SetThreadExecutionState(flags) }.0 == 0 {
            tracing::warn!("system: SetThreadExecutionState({:#x}) failed", flags.0);
        }
    };
    for on in rx {
        if on {
            set(EXECUTION_STATE(ES_CONTINUOUS.0 | ES_SYSTEM_REQUIRED.0));
        } else {
            set(ES_CONTINUOUS);
        }
    }
    // The provider is gone: let the system sleep again.
    set(ES_CONTINUOUS);
}

/// What the watch window does with each event.
type Handler = Box<dyn Fn(SystemEvent)>;

thread_local! {
    /// Where the watch window's procedure sends events, on its own thread.
    static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if let Some(event) = windows_message_event(msg, wparam.0) {
        tracing::debug!(?event, "system event");
        HANDLER.with(|h| {
            if let Some(handler) = &*h.borrow() {
                handler(event);
            }
        });
    }
    if msg == win_msg::WM_POWERBROADCAST {
        // TRUE: we do not object to the change.
        return LRESULT(1);
    }
    // SAFETY: the default procedure for our own window.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Starts `bricolo-system-watch` and waits for its window. Returns the thread's
/// id, for `WM_QUIT`.
fn start_watch_thread(handler: Box<dyn Fn(SystemEvent) + Send>) -> Result<u32, String> {
    let (ready, started) = mpsc::channel();
    std::thread::Builder::new()
        .name("bricolo-system-watch".into())
        .spawn(move || {
            HANDLER.with(|h| *h.borrow_mut() = Some(handler));
            // SAFETY: plain Win32 calls on this thread's own window.
            unsafe {
                let hwnd = match create_window() {
                    Ok(hwnd) => hwnd,
                    Err(e) => {
                        let _ = ready.send(Err(e));
                        return;
                    }
                };
                let _ = ready.send(Ok(GetCurrentThreadId()));
                register_session(hwnd);
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                let _ = WTSUnRegisterSessionNotification(hwnd);
                let _ = DestroyWindow(hwnd);
            }
        })
        .map_err(|e| format!("cannot start the system watch thread: {e}"))?;
    started
        .recv()
        .map_err(|_| "the system watch thread stopped".to_string())?
}

/// A hidden top-level window: broadcasts such as `WM_POWERBROADCAST` and
/// `WM_DISPLAYCHANGE` reach top-level windows, not message-only ones.
unsafe fn create_window() -> Result<HWND, String> {
    // SAFETY: the caller is the thread that will own the window.
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("bricolo-system-watch");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        // Zero when the class exists already, as after a restart of the
        // provider; CreateWindowExW reports a real failure.
        RegisterClassW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("bricolo"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| format!("cannot create the system watch window: {e}"))
    }
}

/// Asks for lock and unlock messages. Right after login the session
/// service may not answer yet, so this tries for a few seconds.
unsafe fn register_session(hwnd: HWND) {
    for attempt in 1..=10 {
        // SAFETY: `hwnd` is this thread's live window.
        match unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) } {
            Ok(()) => return,
            Err(e) if attempt == 10 => {
                tracing::warn!("system: no lock and unlock events: {e}");
            }
            Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    }
}

#[async_trait]
impl SystemProvider for WinSystem {
    async fn idle_seconds(&self, _: &Caller) -> Result<u64, CallError> {
        let mut info = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: `info` is a valid LASTINPUTINFO with its size set.
        if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
            return Err(CallError::Failed("GetLastInputInfo failed".into()));
        }
        // SAFETY: no arguments.
        let now = unsafe { GetTickCount() };
        Ok(idle_from_ticks(now, info.dwTime))
    }

    async fn power(&self, _: &Caller) -> Result<Power, CallError> {
        let mut status = SYSTEM_POWER_STATUS::default();
        // SAFETY: `status` is a valid out pointer.
        unsafe { GetSystemPowerStatus(&mut status) }.map_err(CallError::failed)?;
        Ok(power_from_status(
            status.ACLineStatus,
            status.BatteryFlag,
            status.BatteryLifePercent,
        ))
    }

    async fn keep_awake(&self, caller: &Caller, on: bool) -> Result<(), CallError> {
        let mut awake = self.awake.lock().unwrap();
        let before = !awake.is_empty();
        if on {
            awake.insert(caller.plugin.clone());
        } else {
            awake.remove(&caller.plugin);
        }
        let after = !awake.is_empty();
        if before != after
            && let Err(e) = self.hold(after)
        {
            if on {
                awake.remove(&caller.plugin);
            }
            return Err(e);
        }
        Ok(())
    }

    async fn watch(&self, caller: &Caller, on: bool) -> Result<(), CallError> {
        if on {
            self.ensure_watching()?;
            self.watchers.lock().unwrap().insert(caller.plugin.clone());
        } else {
            self.watchers.lock().unwrap().remove(&caller.plugin);
        }
        Ok(())
    }

    fn release(&self, plugin: &str) {
        self.watchers.lock().unwrap().remove(plugin);
        let mut awake = self.awake.lock().unwrap();
        if awake.remove(plugin) && awake.is_empty() {
            let _ = self.hold(false);
        }
    }
}

impl Drop for WinSystem {
    fn drop(&mut self) {
        // Closing the channel ends `bricolo-keep-awake`, which clears the state.
        self.keeper.lock().unwrap().take();
        if let Some(Ok(thread)) = *self.watch_thread.lock().unwrap() {
            // SAFETY: posts WM_QUIT to our own message thread.
            let _ = unsafe { PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
    }
}
