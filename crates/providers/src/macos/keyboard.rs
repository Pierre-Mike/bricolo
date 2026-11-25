//! `keyboard` on macOS: CGEvents posted at the HID event tap. Posting needs
//! the Accessibility permission, the same grant `window` uses; without it
//! macOS drops the events silently, so each call checks first and answers
//! `denied`.
//!
//! Every event carries its own modifier flags, so keys the user still
//! holds (the chord that triggered the call) don't leak into it. Text goes
//! out one key press per character, as a person types, so it lands where a
//! paste is refused: a character with a key on a US layout presses that
//! key (with Shift when it needs it), because remote desktops and consoles
//! read the key code; any other character goes out as a Unicode string on
//! a key event of its own.

use std::sync::Once;
use std::time::Duration;

use async_trait::async_trait;
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGPreflightPostEventAccess, CGRequestPostEventAccess,
};

use crate::caller::Caller;
use crate::error::CallError;
use crate::hotkey::{Chord, Modifier};
use crate::keyboard::{self, KeyboardProvider, Piece};

/// A pause between events, so slow apps keep up.
const GAP: Duration = Duration::from_millis(4);
/// A further pause after each typed character: with the two `GAP`s, about
/// 12 ms a character, the pace of the Hammerspoon typer.
const CHAR_GAP: Duration = Duration::from_millis(4);

pub struct MacKeyboard;

/// The virtual key code (`kVK_*` in HIToolbox's Events.h) for a W3C code
/// from `Chord`. These are positions on an ANSI keyboard.
pub fn key_code(code: &str) -> Option<u16> {
    let k = match code {
        "KeyA" => 0x00,
        "KeyS" => 0x01,
        "KeyD" => 0x02,
        "KeyF" => 0x03,
        "KeyH" => 0x04,
        "KeyG" => 0x05,
        "KeyZ" => 0x06,
        "KeyX" => 0x07,
        "KeyC" => 0x08,
        "KeyV" => 0x09,
        "KeyB" => 0x0B,
        "KeyQ" => 0x0C,
        "KeyW" => 0x0D,
        "KeyE" => 0x0E,
        "KeyR" => 0x0F,
        "KeyY" => 0x10,
        "KeyT" => 0x11,
        "Digit1" => 0x12,
        "Digit2" => 0x13,
        "Digit3" => 0x14,
        "Digit4" => 0x15,
        "Digit6" => 0x16,
        "Digit5" => 0x17,
        "Equal" => 0x18,
        "Digit9" => 0x19,
        "Digit7" => 0x1A,
        "Minus" => 0x1B,
        "Digit8" => 0x1C,
        "Digit0" => 0x1D,
        "BracketRight" => 0x1E,
        "KeyO" => 0x1F,
        "KeyU" => 0x20,
        "BracketLeft" => 0x21,
        "KeyI" => 0x22,
        "KeyP" => 0x23,
        "Enter" => 0x24,
        "KeyL" => 0x25,
        "KeyJ" => 0x26,
        "Quote" => 0x27,
        "KeyK" => 0x28,
        "Semicolon" => 0x29,
        "Backslash" => 0x2A,
        "Comma" => 0x2B,
        "Slash" => 0x2C,
        "KeyN" => 0x2D,
        "KeyM" => 0x2E,
        "Period" => 0x2F,
        "Tab" => 0x30,
        "Space" => 0x31,
        "Backquote" => 0x32,
        "Backspace" => 0x33,
        "Escape" => 0x35,
        "F17" => 0x40,
        "F18" => 0x4F,
        "F19" => 0x50,
        "F20" => 0x5A,
        "F5" => 0x60,
        "F6" => 0x61,
        "F7" => 0x62,
        "F3" => 0x63,
        "F8" => 0x64,
        "F9" => 0x65,
        "F11" => 0x67,
        "F13" => 0x69,
        "F16" => 0x6A,
        "F14" => 0x6B,
        "F10" => 0x6D,
        "F12" => 0x6F,
        "F15" => 0x71,
        "Home" => 0x73,
        "PageUp" => 0x74,
        "Delete" => 0x75,
        "F4" => 0x76,
        "End" => 0x77,
        "F2" => 0x78,
        "PageDown" => 0x79,
        "F1" => 0x7A,
        "ArrowLeft" => 0x7B,
        "ArrowRight" => 0x7C,
        "ArrowDown" => 0x7D,
        "ArrowUp" => 0x7E,
        _ => return None,
    };
    Some(k)
}

/// The event flags for a chord's modifiers.
pub fn flags(mods: &[Modifier]) -> CGEventFlags {
    mods.iter().fold(CGEventFlags::empty(), |f, m| {
        f | match m {
            Modifier::Cmd => CGEventFlags::MaskCommand,
            Modifier::Ctrl => CGEventFlags::MaskControl,
            Modifier::Alt => CGEventFlags::MaskAlternate,
            Modifier::Shift => CGEventFlags::MaskShift,
        }
    })
}

/// Answers `denied` when macOS would drop posted events. The first refusal
/// asks macOS for access, which lists bricolo under Accessibility and shows the
/// prompt, and logs where to turn it on.
fn check_access() -> Result<(), CallError> {
    static ASKED: Once = Once::new();
    if CGPreflightPostEventAccess() {
        return Ok(());
    }
    ASKED.call_once(|| {
        tracing::warn!(
            "keyboard: macOS refuses key events; turn on bricolo node in \
             System Settings, Privacy & Security, Accessibility (bricolo permissions --request)"
        );
        CGRequestPostEventAccess();
    });
    Err(CallError::Denied)
}

/// Posts a key down and a key up for `code`, with `flags` and, when set,
/// the Unicode `text` the key types.
fn press(code: u16, flags: CGEventFlags, text: Option<&[u16]>) -> Result<(), CallError> {
    for down in [true, false] {
        let event = CGEvent::new_keyboard_event(None, code, down)
            .ok_or_else(|| CallError::Failed("cannot create a key event".into()))?;
        CGEvent::set_flags(Some(&event), flags);
        if let Some(units) = text {
            // SAFETY: `units` is a live slice of `units.len()` UTF-16 units.
            unsafe {
                CGEvent::keyboard_set_unicode_string(Some(&event), units.len() as _, units.as_ptr())
            };
        }
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        std::thread::sleep(GAP);
    }
    Ok(())
}

/// Presses the chord in the process `pid` only, whether or not it is in
/// front: `CGEventPostToPid`. Needs the same access as posting events.
pub fn press_to_pid(pid: i32, chord: &Chord) -> Result<(), CallError> {
    let code = key_code(&chord.code)
        .ok_or_else(|| CallError::Failed(format!("no macOS key code for {}", chord.code)))?;
    check_access()?;
    let flags = flags(&chord.mods);
    for down in [true, false] {
        let event = CGEvent::new_keyboard_event(None, code, down)
            .ok_or_else(|| CallError::Failed("cannot create a key event".into()))?;
        CGEvent::set_flags(Some(&event), flags);
        CGEvent::post_to_pid(pid, Some(&event));
        std::thread::sleep(GAP);
    }
    Ok(())
}

fn keystroke(chord: &Chord) -> Result<(), CallError> {
    let code = key_code(&chord.code)
        .ok_or_else(|| CallError::Failed(format!("no macOS key code for {}", chord.code)))?;
    check_access()?;
    press(code, flags(&chord.mods), None)
}

fn type_text(text: &str) -> Result<(), CallError> {
    check_access()?;
    for piece in keyboard::pieces(text) {
        match piece {
            Piece::Key(code) => {
                let code = key_code(code).expect("Enter and Tab have key codes");
                press(code, CGEventFlags::empty(), None)?;
            }
            Piece::Text(run) => {
                let mut buf = [0u16; 2];
                for c in run.chars() {
                    let units = c.encode_utf16(&mut buf);
                    match keyboard::us_key(c)
                        .and_then(|(code, shift)| Some((key_code(code)?, shift)))
                    {
                        // The string too, so local apps type `c` whatever
                        // the layout.
                        Some((code, shift)) => {
                            let flags = if shift {
                                CGEventFlags::MaskShift
                            } else {
                                CGEventFlags::empty()
                            };
                            press(code, flags, Some(units))?;
                        }
                        // Key code 0 with a string: apps insert the string.
                        None => press(0, CGEventFlags::empty(), Some(units))?,
                    }
                    std::thread::sleep(CHAR_GAP);
                }
            }
        }
    }
    Ok(())
}

#[async_trait]
impl KeyboardProvider for MacKeyboard {
    async fn keystroke(&self, _: &Caller, mods: Vec<String>, key: String) -> Result<(), CallError> {
        let chord = keyboard::chord(&mods, &key)?;
        tokio::task::spawn_blocking(move || keystroke(&chord))
            .await
            .map_err(CallError::failed)?
    }

    async fn type_text(&self, _: &Caller, text: String) -> Result<(), CallError> {
        keyboard::check_text(&text)?;
        tokio::task::spawn_blocking(move || type_text(&text))
            .await
            .map_err(CallError::failed)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_chord_key_has_a_key_code() {
        let keys = "abcdefghijklmnopqrstuvwxyz0123456789"
            .chars()
            .map(String::from)
            .chain((1..=20).map(|n| format!("f{n}")))
            .chain(
                [
                    "left",
                    "right",
                    "up",
                    "down",
                    "space",
                    "enter",
                    "tab",
                    "escape",
                    "delete",
                    "forwarddelete",
                    "home",
                    "end",
                    "pageup",
                    "pagedown",
                    "-",
                    "=",
                    "[",
                    "]",
                    "\\",
                    ";",
                    "'",
                    ",",
                    ".",
                    "/",
                    "`",
                ]
                .map(String::from),
            );
        for key in keys {
            let chord = Chord::from_parts(&[], &key).unwrap();
            assert!(key_code(&chord.code).is_some(), "{key} ({})", chord.code);
        }
    }

    #[test]
    fn every_us_character_has_a_key_code() {
        for c in (0x20u8..0x7f).map(char::from) {
            let (code, _) = keyboard::us_key(c).unwrap();
            assert!(key_code(code).is_some(), "{c:?} ({code})");
        }
    }

    #[test]
    fn known_codes_and_flags() {
        let paste = Chord::parse("cmd+v").unwrap();
        assert_eq!(key_code(&paste.code), Some(0x09));
        assert_eq!(flags(&paste.mods), CGEventFlags::MaskCommand);
        assert_eq!(key_code("Enter"), Some(0x24));
        assert_eq!(key_code("ArrowLeft"), Some(0x7B));
        let all = Chord::parse("ctrl+alt+shift+cmd+k").unwrap();
        assert_eq!(
            flags(&all.mods),
            CGEventFlags::MaskCommand
                | CGEventFlags::MaskControl
                | CGEventFlags::MaskAlternate
                | CGEventFlags::MaskShift
        );
        assert_eq!(key_code("KeyAB"), None);
    }
}
