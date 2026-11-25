//! `keyboard` on X11: fake key presses through the XTEST extension, on a
//! connection opened for each call. On Wayland and without a display it
//! answers `unavailable`.
//!
//! Keys go through the server's keyboard map, so `keystroke` presses the
//! keycode that types the key's keysym, with the modifier keys held. Text
//! is typed one character at a time: a character the map has gets its key,
//! with Shift when it sits in the second column; one it lacks (most of
//! Unicode on a US map) is mapped onto an unused keycode first, as xdotool
//! does. Those keycodes keep their new keysyms afterwards.
//!
//! Modifiers the user still holds, such as the chord that triggered the
//! call, are released first, so they don't change what gets typed.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use async_trait::async_trait;
use x11rb::connection::{Connection, RequestConnection as _};
use x11rb::protocol::xproto::{ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
use x11rb::protocol::xtest::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::hotkey::{Keymap, keysym};
use super::{Session, warn_once};
use crate::caller::Caller;
use crate::error::CallError;
use crate::hotkey::{Chord, Modifier};
use crate::keyboard::{self, KeyboardProvider, Piece};

const SHIFT_L: u32 = 0xffe1;
const CONTROL_L: u32 = 0xffe3;
const ALT_L: u32 = 0xffe9;
const SUPER_L: u32 = 0xffeb;
/// Before a spare keycode is mapped to another keysym, the clients get
/// this long to read the key it typed last.
const SETTLE: Duration = Duration::from_millis(30);

#[derive(Default)]
pub struct X11Keyboard {
    warned: AtomicBool,
}

/// The keysym that types `c`, from keysymdef.h: Latin-1 keysyms are the
/// code point, the rest of Unicode is `0x01000000` plus it.
pub fn char_keysym(c: char) -> u32 {
    let cp = u32::from(c);
    match cp {
        0x20..=0x7e | 0xa0..=0xff => cp,
        _ => 0x0100_0000 + cp,
    }
}

/// Where `sym` sits in the map: its keycode, and whether it needs Shift
/// (second column). Only the first two columns count; others need
/// modifiers such as AltGr that the map doesn't name.
pub fn locate(map: &Keymap, sym: u32) -> Option<(u8, bool)> {
    let per = usize::from(map.per.max(1));
    for column in 0..per.min(2) {
        let found = map
            .syms
            .chunks(per)
            .position(|syms| syms.get(column) == Some(&sym));
        if let Some(i) = found {
            let code = u8::try_from(usize::from(map.min) + i).ok()?;
            return Some((code, column == 1));
        }
    }
    None
}

/// Keycodes with no keysym at all, free to map a character onto.
pub fn spare_keycodes(map: &Keymap) -> Vec<u8> {
    let per = usize::from(map.per.max(1));
    map.syms
        .chunks(per)
        .enumerate()
        .filter(|(_, syms)| syms.iter().all(|s| *s == 0))
        .filter_map(|(i, _)| u8::try_from(usize::from(map.min) + i).ok())
        .collect()
}

/// The keysym of the key that holds each modifier.
fn modifier_keysym(m: Modifier) -> u32 {
    match m {
        Modifier::Shift => SHIFT_L,
        Modifier::Ctrl => CONTROL_L,
        Modifier::Alt => ALT_L,
        Modifier::Cmd => SUPER_L,
    }
}

fn failed(e: impl std::fmt::Display) -> CallError {
    CallError::Failed(format!("X11: {e}"))
}

/// One call's connection and the keyboard map read at its start.
pub(super) struct Typist {
    conn: RustConnection,
    map: Keymap,
    spare: Vec<u8>,
    /// The next spare keycode to use.
    next_spare: usize,
    /// How many spare keycodes this call has mapped so far.
    used_spares: usize,
}

impl Typist {
    fn open() -> Result<Self, CallError> {
        Self::open_on(None)
    }

    /// Opens `display` (`:99`), or the default display.
    pub(super) fn open_on(display: Option<&str>) -> Result<Self, CallError> {
        let (conn, _) = x11rb::connect(display).map_err(failed)?;
        if conn
            .extension_information(xtest::X11_EXTENSION_NAME)
            .map_err(failed)?
            .is_none()
        {
            return Err(CallError::Failed(
                "the X server has no XTEST extension".into(),
            ));
        }
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let mapping = conn
            .get_keyboard_mapping(min, max - min + 1)
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        let map = Keymap {
            min,
            per: mapping.keysyms_per_keycode,
            syms: mapping.keysyms,
        };
        let spare = spare_keycodes(&map);
        Ok(Self {
            conn,
            map,
            spare,
            next_spare: 0,
            used_spares: 0,
        })
    }

    fn fake(&self, press: bool, code: u8) -> Result<(), CallError> {
        let kind = if press {
            KEY_PRESS_EVENT
        } else {
            KEY_RELEASE_EVENT
        };
        self.conn
            .xtest_fake_input(kind, code, 0, x11rb::NONE, 0, 0, 0)
            .map_err(failed)?;
        Ok(())
    }

    /// Releases the modifier keys that are down now.
    pub(super) fn release_held_modifiers(&self) -> Result<(), CallError> {
        let held = self
            .conn
            .query_keymap()
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        let mods = self
            .conn
            .get_modifier_mapping()
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        for &code in mods.keycodes.iter().filter(|c| **c != 0) {
            let byte = held.keys[usize::from(code / 8)];
            if byte & (1 << (code % 8)) != 0 {
                self.fake(false, code)?;
            }
        }
        Ok(())
    }

    fn keycode(&self, sym: u32) -> Result<u8, CallError> {
        locate(&self.map, sym)
            .map(|(code, _)| code)
            .ok_or_else(|| CallError::Failed(format!("no key for keysym {sym:#x} in the map")))
    }

    /// Presses `code` with the keys in `held` down, then releases all.
    fn press(&self, held: &[u8], code: u8) -> Result<(), CallError> {
        for &m in held {
            self.fake(true, m)?;
        }
        self.fake(true, code)?;
        self.fake(false, code)?;
        for &m in held.iter().rev() {
            self.fake(false, m)?;
        }
        Ok(())
    }

    pub(super) fn keystroke(&self, chord: &Chord) -> Result<(), CallError> {
        let sym = keysym(&chord.code)
            .ok_or_else(|| CallError::Failed(format!("no X11 keysym for {}", chord.code)))?;
        let code = self.keycode(sym)?;
        let held = chord
            .mods
            .iter()
            .map(|m| self.keycode(modifier_keysym(*m)))
            .collect::<Result<Vec<_>, _>>()?;
        self.press(&held, code)
    }

    /// Maps `sym` onto the next spare keycode and returns it.
    fn borrow_spare(&mut self, sym: u32) -> Result<u8, CallError> {
        if self.spare.is_empty() {
            return Err(CallError::Failed(format!(
                "no key types keysym {sym:#x}, and the map has no unused keycode"
            )));
        }
        if self.used_spares >= self.spare.len() {
            // Every spare was mapped in this call: let the clients read
            // the last characters before one changes under them.
            self.conn.sync().map_err(failed)?;
            std::thread::sleep(SETTLE);
        }
        let code = self.spare[self.next_spare % self.spare.len()];
        self.next_spare += 1;
        self.used_spares += 1;
        let per = usize::from(self.map.per.max(1));
        let mut syms = vec![0; per];
        syms[0] = sym;
        if per > 1 {
            syms[1] = sym;
        }
        self.conn
            .change_keyboard_mapping(1, code, self.map.per, &syms)
            .map_err(failed)?
            .check()
            .map_err(failed)?;
        // Keep our copy in step, so the same character finds this key.
        let at = usize::from(code - self.map.min) * per;
        self.map.syms[at..at + per].copy_from_slice(&syms);
        Ok(code)
    }

    fn type_char(&mut self, c: char) -> Result<(), CallError> {
        let sym = char_keysym(c);
        match locate(&self.map, sym) {
            Some((code, false)) => self.press(&[], code),
            Some((code, true)) => {
                let shift = self.keycode(SHIFT_L)?;
                self.press(&[shift], code)
            }
            None => {
                let code = self.borrow_spare(sym)?;
                self.press(&[], code)
            }
        }
    }

    pub(super) fn type_text(&mut self, text: &str) -> Result<(), CallError> {
        for piece in keyboard::pieces(text) {
            match piece {
                Piece::Key(code) => {
                    let sym = keysym(code).expect("Enter and Tab have keysyms");
                    let code = self.keycode(sym)?;
                    self.press(&[], code)?;
                }
                Piece::Text(run) => {
                    for c in run.chars() {
                        self.type_char(c)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Waits until the server has processed every request.
    pub(super) fn finish(&self) -> Result<(), CallError> {
        self.conn.sync().map_err(failed)
    }
}

/// Presses `chord` on the default display, as `keystroke` does, for
/// callers that already know the session is X11 (`accessibility`).
pub(super) fn press_chord(chord: &Chord) -> Result<(), CallError> {
    let typist = Typist::open()?;
    typist.release_held_modifiers()?;
    typist.keystroke(chord)?;
    typist.finish()
}

impl X11Keyboard {
    fn check_session(&self) -> Result<(), CallError> {
        match Session::current() {
            Session::X11 => {}
            Session::Wayland => {
                warn_once(
                    &self.warned,
                    "keyboard: this is a Wayland session; synthetic key presses need X11 or Hyprland, so \
                     keyboard answers unavailable (see docs/linux.md)",
                );
                return Err(CallError::Unavailable);
            }
            Session::Headless => {
                warn_once(
                    &self.warned,
                    "keyboard: no DISPLAY, so keyboard answers unavailable",
                );
                return Err(CallError::Unavailable);
            }
        }
        Ok(())
    }

    async fn run(
        &self,
        work: impl FnOnce(&mut Typist) -> Result<(), CallError> + Send + 'static,
    ) -> Result<(), CallError> {
        self.check_session()?;
        tokio::task::spawn_blocking(move || {
            let mut typist = Typist::open()?;
            typist.release_held_modifiers()?;
            work(&mut typist)?;
            typist.finish()
        })
        .await
        .map_err(CallError::failed)?
    }
}

#[async_trait]
impl KeyboardProvider for X11Keyboard {
    async fn keystroke(&self, _: &Caller, mods: Vec<String>, key: String) -> Result<(), CallError> {
        let chord = keyboard::chord(&mods, &key)?;
        self.run(move |t| t.keystroke(&chord)).await
    }

    async fn type_text(&self, _: &Caller, text: String) -> Result<(), CallError> {
        keyboard::check_text(&text)?;
        self.run(move |t| t.type_text(&text)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> Keymap {
        // Keycodes 8 to 12, two columns: a/A, 1/!, Shift_L, nothing, é.
        Keymap {
            min: 8,
            per: 2,
            syms: vec![0x61, 0x41, 0x31, 0x21, SHIFT_L, 0, 0, 0, 0xe9, 0xc9],
        }
    }

    #[test]
    fn characters_map_to_keysyms() {
        assert_eq!(char_keysym('a'), 0x61);
        assert_eq!(char_keysym('~'), 0x7e);
        assert_eq!(char_keysym('é'), 0xe9);
        assert_eq!(char_keysym('€'), 0x0100_20ac);
        assert_eq!(char_keysym('😀'), 0x0101_f600);
    }

    #[test]
    fn locate_reports_the_shift_column() {
        let m = map();
        assert_eq!(locate(&m, 0x61), Some((8, false)));
        assert_eq!(locate(&m, 0x41), Some((8, true)));
        assert_eq!(locate(&m, 0x21), Some((9, true)));
        assert_eq!(locate(&m, 0xc9), Some((12, true)));
        assert_eq!(locate(&m, 0x0100_20ac), None);
    }

    #[test]
    fn spare_keycodes_have_no_keysym() {
        assert_eq!(spare_keycodes(&map()), [11]);
    }

    #[test]
    fn modifiers_press_their_left_keys() {
        assert_eq!(modifier_keysym(Modifier::Cmd), SUPER_L);
        assert_eq!(modifier_keysym(Modifier::Ctrl), CONTROL_L);
        let chord = keyboard::chord(&["ctrl".into()], "v").unwrap();
        assert_eq!(keysym(&chord.code), Some(0x76));
    }
}
