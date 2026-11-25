//! `hotkey` on Windows: `RegisterHotKey` on one thread of its own. A hotkey
//! belongs to the thread that registers it and arrives as `WM_HOTKEY` in
//! that thread's queue, so the thread runs a `GetMessageW` loop and takes
//! its orders (register, unregister) through a channel and a posted
//! message, answering each registration on a oneshot channel.
//!
//! `cmd` is the Windows key (`MOD_WIN`); every chord is registered with
//! `MOD_NOREPEAT`, so holding it sends one press. Letters, digits and
//! punctuation are physical positions on a US keyboard, as for
//! `keyboard`: their virtual key comes from the scan code through the
//! current layout. Windows has no release message for a hotkey, so after a
//! press the thread polls the key every 15 ms and sends
//! `hotkey-released` when it goes up. A chord another program holds, or
//! one Windows keeps for itself (Win+L), answers `failed`. Fn is the
//! keyboard's own business and answers `unavailable`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::oneshot;
use windows::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MAPVK_VSC_TO_VK, MapVirtualKeyW, RegisterHotKey, UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, KillTimer, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetTimer, WM_APP,
    WM_HOTKEY, WM_TIMER,
};
use windows::core::HRESULT;

use super::input;
use crate::caller::Caller;
use crate::error::CallError;
use crate::event::{EventSink, ProviderEvent};
use crate::hotkey::{Chord, HotkeyProvider};
use crate::win;

/// Posted to the thread when orders wait in its channel.
const WM_ORDERS: u32 = WM_APP + 1;
/// How often a pressed hotkey's key is checked for its release.
const RELEASE_POLL_MS: u32 = 15;
/// Applications may use hotkey ids 1 to 0xBFFF.
const MAX_ID: i32 = 0xbfff;

enum Order {
    Register {
        id: i32,
        mods: u32,
        vk: u32,
        reply: oneshot::Sender<Result<(), Refused>>,
    },
    Unregister(i32),
}

enum Refused {
    /// Another program, or Windows, holds the chord.
    Taken,
    Failed(String),
}

struct Binding {
    plugin: String,
    id: String,
    chord: String,
    vk: u16,
}

/// Bindings by their `RegisterHotKey` id.
type Table = Arc<Mutex<HashMap<i32, Binding>>>;

/// The hotkey thread: its id, for posting, and its order channel.
struct Thread {
    tid: u32,
    orders: mpsc::Sender<Order>,
}

impl Thread {
    fn start(table: Table, sink: EventSink) -> Result<Self, CallError> {
        let (orders, inbox) = mpsc::channel();
        let (ready, started) = mpsc::channel();
        std::thread::Builder::new()
            .name("bricolo-hotkeys".into())
            .spawn(move || run(&inbox, &ready, &table, &sink))
            .map_err(CallError::failed)?;
        let tid = started
            .recv()
            .map_err(|_| CallError::Failed("the hotkey thread did not start".into()))?;
        Ok(Self { tid, orders })
    }

    fn send(&self, order: Order) -> Result<(), CallError> {
        self.orders
            .send(order)
            .map_err(|_| CallError::Failed("the hotkey thread stopped".into()))?;
        // SAFETY: posts a message to our own thread's queue.
        unsafe { PostThreadMessageW(self.tid, WM_ORDERS, WPARAM(0), LPARAM(0)) }
            .map_err(|e| CallError::Failed(format!("the hotkey thread stopped: {e}")))
    }
}

/// The thread's loop. It makes its message queue, says it is ready, then
/// handles orders, presses and the release timer until the process ends.
fn run(inbox: &mpsc::Receiver<Order>, ready: &mpsc::Sender<u32>, table: &Table, sink: &EventSink) {
    let mut msg = MSG::default();
    // SAFETY: the first PeekMessage creates this thread's queue, so posts
    // made after `ready` arrive.
    unsafe {
        let _ = PeekMessageW(&raw mut msg, None, 0, 0, PM_NOREMOVE);
    }
    // SAFETY: a plain read.
    if ready.send(unsafe { GetCurrentThreadId() }).is_err() {
        return;
    }
    // Pressed hotkeys waiting for their release: (plugin, id, key).
    let mut held: Vec<(String, String, u16)> = Vec::new();
    let mut timer = 0usize;
    // SAFETY: a standard message loop on this thread; the hotkeys and the
    // timer belong to it.
    while unsafe { GetMessageW(&raw mut msg, None, 0, 0) }.as_bool() {
        match msg.message {
            WM_ORDERS => {
                for order in inbox.try_iter() {
                    handle(order);
                }
            }
            WM_HOTKEY => {
                let os_id = msg.wParam.0 as i32;
                let target = table
                    .lock()
                    .unwrap()
                    .get(&os_id)
                    .map(|b| (b.plugin.clone(), b.id.clone(), b.vk));
                if let Some((plugin, id, vk)) = target {
                    sink(&plugin, ProviderEvent::Hotkey(id.clone()));
                    if !held.iter().any(|(p, i, _)| *p == plugin && *i == id) {
                        held.push((plugin, id, vk));
                    }
                    if timer == 0 {
                        // SAFETY: a thread timer: WM_TIMER in this queue.
                        timer = unsafe { SetTimer(None, 0, RELEASE_POLL_MS, None) };
                    }
                }
            }
            WM_TIMER if msg.wParam.0 == timer && timer != 0 => {
                held.retain(|(plugin, id, vk)| {
                    let down = input::is_down(*vk);
                    if !down {
                        sink(plugin, ProviderEvent::HotkeyReleased(id.clone()));
                    }
                    down
                });
                if held.is_empty() {
                    // SAFETY: the timer this thread set.
                    let _ = unsafe { KillTimer(None, timer) };
                    timer = 0;
                }
            }
            _ => {}
        }
    }
}

fn handle(order: Order) {
    match order {
        Order::Register {
            id,
            mods,
            vk,
            reply,
        } => {
            // SAFETY: a thread hotkey (no window), owned by this thread.
            let result = unsafe { RegisterHotKey(None, id, HOT_KEY_MODIFIERS(mods), vk) };
            let answer = result.map_err(|e| {
                if e.code() == HRESULT::from_win32(ERROR_HOTKEY_ALREADY_REGISTERED.0) {
                    Refused::Taken
                } else {
                    Refused::Failed(e.message())
                }
            });
            let _ = reply.send(answer);
        }
        Order::Unregister(id) => {
            // SAFETY: a hotkey this thread registered.
            let _ = unsafe { UnregisterHotKey(None, id) };
        }
    }
}

/// The virtual key for a chord's key. A character key takes its code from
/// its scan code through the current layout, so "cmd+q" is the key in the
/// Q position on an AZERTY keyboard too.
fn chord_vk(code: &str) -> Option<u16> {
    if win::is_character_key(code)
        && let Some((scan, _)) = win::scan_code(code)
    {
        // SAFETY: a pure lookup in the keyboard layout.
        let vk = unsafe { MapVirtualKeyW(u32::from(scan), MAPVK_VSC_TO_VK) };
        if let Ok(vk) = u16::try_from(vk)
            && vk != 0
        {
            return Some(vk);
        }
    }
    win::virtual_key(code)
}

pub struct WinHotkeys {
    sink: EventSink,
    table: Table,
    thread: Mutex<Option<Arc<Thread>>>,
    next_id: AtomicI32,
}

impl WinHotkeys {
    pub fn new(sink: EventSink) -> Self {
        Self {
            sink,
            table: Table::default(),
            thread: Mutex::default(),
            next_id: AtomicI32::new(1),
        }
    }

    fn thread(&self) -> Result<Arc<Thread>, CallError> {
        let mut slot = self.thread.lock().unwrap();
        if let Some(t) = &*slot {
            return Ok(t.clone());
        }
        let t = Arc::new(Thread::start(self.table.clone(), self.sink.clone())?);
        *slot = Some(t.clone());
        Ok(t)
    }

    /// An id no binding uses.
    fn new_id(&self) -> i32 {
        let table = self.table.lock().unwrap();
        loop {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            if !(1..=MAX_ID).contains(&id) {
                self.next_id.store(1, Ordering::Relaxed);
                continue;
            }
            if !table.contains_key(&id) {
                return id;
            }
        }
    }

    fn find(&self, plugin: &str, id: &str) -> Option<i32> {
        self.table
            .lock()
            .unwrap()
            .iter()
            .find(|(_, b)| b.plugin == plugin && b.id == id)
            .map(|(os, _)| *os)
    }

    fn unregister(&self, os_id: i32) {
        if let Some(t) = &*self.thread.lock().unwrap()
            && let Err(e) = t.send(Order::Unregister(os_id))
        {
            tracing::warn!("hotkey: {e}");
        }
    }
}

#[async_trait]
impl HotkeyProvider for WinHotkeys {
    async fn bind(&self, caller: &Caller, id: String, chord: String) -> Result<(), CallError> {
        let parsed = Chord::parse(&chord).map_err(CallError::Failed)?;
        if parsed.is_fn() {
            return Err(CallError::Unavailable);
        }
        let vk = chord_vk(&parsed.code)
            .ok_or_else(|| CallError::Failed(format!("no Windows key for {}", parsed.code)))?;
        let mods = win::hotkey_modifiers(&parsed.mods);
        let owner = self
            .table
            .lock()
            .unwrap()
            .iter()
            .find(|(_, b)| Chord::parse(&b.chord).is_ok_and(|c| c == parsed))
            .map(|(_, b)| (b.plugin.clone(), b.id.clone()));
        match owner {
            Some((plugin, bound)) if plugin == caller.plugin && bound == id => return Ok(()),
            Some((plugin, bound)) => {
                return Err(CallError::Failed(format!(
                    "{chord} is already bound by {plugin} as {bound}"
                )));
            }
            None => {}
        }
        // Rebinding an id to a new chord drops the old chord.
        self.unbind(caller, id.clone()).await?;
        let thread = self.thread()?;
        let os_id = self.new_id();
        let (reply, answer) = oneshot::channel();
        thread.send(Order::Register {
            id: os_id,
            mods,
            vk: u32::from(vk),
            reply,
        })?;
        match answer.await {
            Ok(Ok(())) => {}
            Ok(Err(Refused::Taken)) => {
                return Err(CallError::Failed(format!(
                    "cannot bind {chord}: another program holds it"
                )));
            }
            Ok(Err(Refused::Failed(e))) => {
                return Err(CallError::Failed(format!("cannot bind {chord}: {e}")));
            }
            Err(_) => return Err(CallError::Failed("the hotkey thread stopped".into())),
        }
        self.table.lock().unwrap().insert(
            os_id,
            Binding {
                plugin: caller.plugin.clone(),
                id,
                chord,
                vk,
            },
        );
        Ok(())
    }

    async fn unbind(&self, caller: &Caller, id: String) -> Result<(), CallError> {
        let Some(os_id) = self.find(&caller.plugin, &id) else {
            return Ok(());
        };
        self.table.lock().unwrap().remove(&os_id);
        self.unregister(os_id);
        Ok(())
    }

    fn release(&self, plugin: &str) {
        let mut dropped = Vec::new();
        self.table.lock().unwrap().retain(|os_id, b| {
            let keep = b.plugin != plugin;
            if !keep {
                dropped.push(*os_id);
            }
            keep
        });
        for os_id in dropped {
            self.unregister(os_id);
        }
    }

    fn bound(&self, plugin: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .table
            .lock()
            .unwrap()
            .values()
            .filter(|b| b.plugin == plugin)
            .map(|b| (b.id.clone(), b.chord.clone()))
            .collect();
        out.sort();
        out
    }
}
