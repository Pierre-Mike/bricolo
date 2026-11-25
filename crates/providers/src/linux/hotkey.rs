//! `hotkey` on X11: passive key grabs on the root window, on a connection
//! of their own. A thread waits for key presses and sends each to the
//! owning plugin's queue through the event sink, as on macOS.
//!
//! X11 counts Caps Lock and Num Lock as modifiers, so each chord is grabbed
//! four times, with and without them. `cmd` is Super (Mod4) and `alt` is
//! Mod1, the usual layout. Hyprland has its own backend (`hyprland/`); on
//! other Wayland compositors, hotkeys answer `unavailable`: the
//! XDG GlobalShortcuts portal is not supported yet.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, ModMask, Window};
use x11rb::protocol::{ErrorKind, Event};
use x11rb::rust_connection::RustConnection;

use super::{Session, warn_once};
use crate::caller::Caller;
use crate::error::CallError;
use crate::event::{EventSink, ProviderEvent};
use crate::hotkey::{Chord, HotkeyProvider, Modifier};

/// The modifiers a chord can use. Others in a press's state (the locks,
/// Mod3, Mod5) are ignored.
const CHORD_MODS: u16 = 1 | 4 | 8 | 64; // Shift, Control, Mod1, Mod4
const LOCK: u16 = 2;
const NUM_LOCK: u16 = 16; // Mod2
/// Each chord is grabbed with every combination of these.
const LOCKS: [u16; 4] = [0, LOCK, NUM_LOCK, LOCK | NUM_LOCK];

/// The X11 modifier mask for a chord's modifiers.
pub fn mod_mask(mods: &[Modifier]) -> u16 {
    mods.iter()
        .map(|m| match m {
            Modifier::Shift => 1,
            Modifier::Ctrl => 4,
            Modifier::Alt => 8,
            Modifier::Cmd => 64,
        })
        .fold(0, |mask, bit| mask | bit)
}

/// The keysym for a W3C key code from `Chord`, from X11's keysymdef.h.
/// Letters map to their lowercase keysym, which is what the keymap lists
/// first.
pub fn keysym(code: &str) -> Option<u32> {
    let sym = match code {
        "ArrowLeft" => 0xff51,
        "ArrowUp" => 0xff52,
        "ArrowRight" => 0xff53,
        "ArrowDown" => 0xff54,
        "Space" => 0x20,
        "Enter" => 0xff0d,
        "Tab" => 0xff09,
        "Escape" => 0xff1b,
        "Backspace" => 0xff08,
        "Delete" => 0xffff,
        "Home" => 0xff50,
        "End" => 0xff57,
        "PageUp" => 0xff55,
        "PageDown" => 0xff56,
        "Minus" => 0x2d,
        "Equal" => 0x3d,
        "BracketLeft" => 0x5b,
        "BracketRight" => 0x5d,
        "Backslash" => 0x5c,
        "Semicolon" => 0x3b,
        "Quote" => 0x27,
        "Comma" => 0x2c,
        "Period" => 0x2e,
        "Slash" => 0x2f,
        "Backquote" => 0x60,
        _ => {
            if let Some(letter) = code.strip_prefix("Key") {
                let [c] = letter.as_bytes() else { return None };
                return c
                    .is_ascii_uppercase()
                    .then(|| c.to_ascii_lowercase().into());
            }
            if let Some(digit) = code.strip_prefix("Digit") {
                let [c] = digit.as_bytes() else { return None };
                return c.is_ascii_digit().then(|| (*c).into());
            }
            let n: u32 = code.strip_prefix('F')?.parse().ok()?;
            // F1 is 0xffbe and the rest follow in order.
            return (1..=20).contains(&n).then(|| 0xffbe + n - 1);
        }
    };
    Some(sym)
}

/// The keyboard mapping: `per` keysyms for each keycode from `min`.
pub struct Keymap {
    pub min: u8,
    pub per: u8,
    pub syms: Vec<u32>,
}

impl Keymap {
    /// The keycode that types `sym`, preferring one where it needs no
    /// Shift (the first column).
    pub fn keycode(&self, sym: u32) -> Option<u8> {
        let per = usize::from(self.per.max(1));
        let codes = self.syms.chunks(per).enumerate();
        let at = |(i, _)| u8::try_from(usize::from(self.min) + i).ok();
        codes
            .clone()
            .find(|(_, syms)| syms.first() == Some(&sym))
            .or_else(|| codes.clone().find(|(_, syms)| syms.contains(&sym)))
            .and_then(at)
    }
}

/// A grab: the keycode and the chord's modifier mask, locks left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    code: u8,
    mods: u16,
}

struct Binding {
    plugin: String,
    id: String,
    chord: String,
}

type Table = Arc<Mutex<HashMap<Key, Binding>>>;

/// The connection that holds the grabs and receives the presses.
struct Grabber {
    conn: RustConnection,
    root: Window,
    keymap: Keymap,
    alive: AtomicBool,
}

enum GrabError {
    /// Another program holds the grab.
    Taken,
    Failed(String),
}

impl Grabber {
    fn open() -> Result<Self, String> {
        let (conn, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
        let setup = conn.setup();
        let root = setup.roots[screen].root;
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let mapping = conn
            .get_keyboard_mapping(min, max - min + 1)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        let keymap = Keymap {
            min,
            per: mapping.keysyms_per_keycode,
            syms: mapping.keysyms,
        };
        Ok(Self {
            conn,
            root,
            keymap,
            alive: AtomicBool::new(true),
        })
    }

    fn grab(&self, key: Key) -> Result<(), GrabError> {
        for (i, lock) in LOCKS.into_iter().enumerate() {
            let result = self
                .conn
                .grab_key(
                    false,
                    self.root,
                    ModMask::from(key.mods | lock),
                    key.code,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )
                .map_err(ReplyError::from)
                .and_then(|cookie| cookie.check());
            if let Err(e) = result {
                for lock in &LOCKS[..i] {
                    self.ungrab_one(key, *lock);
                }
                let _ = self.conn.flush();
                return Err(match e {
                    ReplyError::X11Error(e) if e.error_kind == ErrorKind::Access => {
                        GrabError::Taken
                    }
                    e => GrabError::Failed(e.to_string()),
                });
            }
        }
        Ok(())
    }

    fn ungrab_one(&self, key: Key, lock: u16) {
        let _ = self
            .conn
            .ungrab_key(key.code, self.root, ModMask::from(key.mods | lock));
    }

    /// Sends the ungrab requests without waiting for an answer.
    fn ungrab(&self, key: Key) {
        for lock in LOCKS {
            self.ungrab_one(key, lock);
        }
        let _ = self.conn.flush();
    }

    /// Delivers presses until the connection breaks.
    fn listen(&self, table: &Table, sink: &EventSink) {
        loop {
            match self.conn.wait_for_event() {
                Ok(Event::KeyPress(press)) => {
                    let key = Key {
                        code: press.detail,
                        mods: u16::from(press.state) & CHORD_MODS,
                    };
                    let target = table
                        .lock()
                        .unwrap()
                        .get(&key)
                        .map(|b| (b.plugin.clone(), b.id.clone()));
                    if let Some((plugin, id)) = target {
                        sink(&plugin, ProviderEvent::Hotkey(id));
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("hotkey: lost the X server: {e}");
                    self.alive.store(false, Ordering::Relaxed);
                    return;
                }
            }
        }
    }
}

pub struct X11Hotkeys {
    sink: EventSink,
    table: Table,
    grabber: Mutex<Option<Arc<Grabber>>>,
    warned: AtomicBool,
}

impl X11Hotkeys {
    pub fn new(sink: EventSink) -> Self {
        Self {
            sink,
            table: Arc::default(),
            grabber: Mutex::default(),
            warned: AtomicBool::new(false),
        }
    }

    /// The live grabber, connecting first if needed. A new connection
    /// grabs the bindings again that the old one held.
    fn grabber(&self) -> Result<Arc<Grabber>, CallError> {
        match Session::current() {
            Session::X11 => {}
            Session::Wayland => {
                warn_once(
                    &self.warned,
                    "hotkey: this is a Wayland session; global hotkeys need X11 or Hyprland, so binding \
                     answers unavailable (see docs/linux.md)",
                );
                return Err(CallError::Unavailable);
            }
            Session::Headless => {
                warn_once(
                    &self.warned,
                    "hotkey: no DISPLAY, so binding answers unavailable",
                );
                return Err(CallError::Unavailable);
            }
        }
        let mut slot = self.grabber.lock().unwrap();
        if let Some(g) = &*slot
            && g.alive.load(Ordering::Relaxed)
        {
            return Ok(g.clone());
        }
        let grabber = Arc::new(Grabber::open().map_err(|e| {
            let message = format!("hotkey: cannot open the X display: {e}");
            warn_once(&self.warned, &message);
            tracing::debug!("{message}");
            CallError::Unavailable
        })?);
        for (key, b) in self.table.lock().unwrap().iter() {
            if grabber.grab(*key).is_err() {
                tracing::warn!("hotkey: could not grab {} again for {}", b.chord, b.plugin);
            }
        }
        let (listener, table, sink) = (grabber.clone(), self.table.clone(), self.sink.clone());
        std::thread::Builder::new()
            .name("bricolo-hotkeys".into())
            .spawn(move || listener.listen(&table, &sink))
            .map_err(CallError::failed)?;
        *slot = Some(grabber.clone());
        Ok(grabber)
    }

    fn find(&self, plugin: &str, id: &str) -> Option<Key> {
        self.table
            .lock()
            .unwrap()
            .iter()
            .find(|(_, b)| b.plugin == plugin && b.id == id)
            .map(|(key, _)| *key)
    }

    fn ungrab(&self, key: Key) {
        if let Some(g) = &*self.grabber.lock().unwrap() {
            g.ungrab(key);
        }
    }
}

#[async_trait]
impl HotkeyProvider for X11Hotkeys {
    async fn bind(&self, caller: &Caller, id: String, chord: String) -> Result<(), CallError> {
        let parsed = Chord::parse(&chord).map_err(CallError::Failed)?;
        // X11 has no keysym for Fn: the keyboard handles it itself.
        if parsed.is_fn() {
            return Err(CallError::Unavailable);
        }
        let sym = keysym(&parsed.code)
            .ok_or_else(|| CallError::Failed(format!("no X11 keysym for {}", parsed.code)))?;
        let grabber = self.grabber()?;
        let code = grabber
            .keymap
            .keycode(sym)
            .ok_or_else(|| CallError::Failed(format!("no key on this keyboard types {chord}")))?;
        let key = Key {
            code,
            mods: mod_mask(&parsed.mods),
        };
        let owner = self
            .table
            .lock()
            .unwrap()
            .get(&key)
            .map(|b| (b.plugin.clone(), b.id.clone()));
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
        let g = grabber.clone();
        match tokio::task::spawn_blocking(move || g.grab(key))
            .await
            .map_err(CallError::failed)?
        {
            Ok(()) => {}
            Err(GrabError::Taken) => {
                return Err(CallError::Failed(format!(
                    "cannot bind {chord}: another program holds it"
                )));
            }
            Err(GrabError::Failed(e)) => {
                return Err(CallError::Failed(format!("cannot bind {chord}: {e}")));
            }
        }
        self.table.lock().unwrap().insert(
            key,
            Binding {
                plugin: caller.plugin.clone(),
                id,
                chord,
            },
        );
        Ok(())
    }

    async fn unbind(&self, caller: &Caller, id: String) -> Result<(), CallError> {
        let Some(key) = self.find(&caller.plugin, &id) else {
            return Ok(());
        };
        self.table.lock().unwrap().remove(&key);
        self.ungrab(key);
        Ok(())
    }

    fn release(&self, plugin: &str) {
        let mut dropped = Vec::new();
        self.table.lock().unwrap().retain(|key, b| {
            let keep = b.plugin != plugin;
            if !keep {
                dropped.push(*key);
            }
            keep
        });
        for key in dropped {
            self.ungrab(key);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(chord: &str) -> Option<u32> {
        keysym(&Chord::parse(chord).unwrap().code)
    }

    #[test]
    fn chords_map_to_keysyms() {
        assert_eq!(sym("cmd+alt+left"), Some(0xff51));
        assert_eq!(sym("ctrl+k"), Some(u32::from(b'k')));
        assert_eq!(sym("ctrl+7"), Some(u32::from(b'7')));
        assert_eq!(sym("f1"), Some(0xffbe));
        assert_eq!(sym("f12"), Some(0xffc9));
        assert_eq!(sym("f20"), Some(0xffd1));
        assert_eq!(sym("return"), Some(0xff0d));
        assert_eq!(sym("forwarddelete"), Some(0xffff));
        assert_eq!(sym("pagedown"), Some(0xff56));
        assert_eq!(sym("cmd++"), Some(u32::from(b'=')));
        assert_eq!(sym("`"), Some(0x60));
        assert_eq!(keysym("KeyAB"), None);
        assert_eq!(keysym("F21"), None);
        // Every key `Chord` accepts has a keysym.
        let keys = "abcdefghijklmnopqrstuvwxyz0123456789-=[]\\;',./`";
        for key in keys.chars() {
            assert!(sym(&format!("ctrl+{key}")).is_some(), "{key}");
        }
        for key in [
            "left", "right", "up", "down", "space", "enter", "tab", "escape", "delete", "home",
            "end", "pageup", "pagedown", "f5",
        ] {
            assert!(sym(key).is_some(), "{key}");
        }
    }

    #[test]
    fn modifiers_map_to_the_usual_masks() {
        let mask = |c: &str| mod_mask(&Chord::parse(c).unwrap().mods);
        assert_eq!(mask("ctrl+alt+shift+cmd+k"), CHORD_MODS);
        assert_eq!(mask("cmd+alt+left"), 64 | 8);
        assert_eq!(mask("ctrl+a"), 4);
        assert_eq!(mask("f1"), 0);
    }

    #[test]
    fn keymap_prefers_the_unshifted_column() {
        // Keycode 10 types 1 and !, keycode 11 types ! only (made up), and
        // keycode 12 types k and K.
        let map = Keymap {
            min: 10,
            per: 2,
            syms: vec![0x31, 0x21, 0x21, 0, 0x6b, 0x4b],
        };
        assert_eq!(map.keycode(0x31), Some(10));
        assert_eq!(map.keycode(0x21), Some(11));
        assert_eq!(map.keycode(0x6b), Some(12));
        assert_eq!(map.keycode(0x4b), Some(12));
        assert_eq!(map.keycode(0xff51), None);
    }
}
