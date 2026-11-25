//! `keyboard`: synthetic key presses. The macOS backend posts CGEvents
//! (`macos/keyboard.rs`), the Linux one sends XTEST fake input
//! (`linux/keyboard.rs`). Key names and modifiers are hotkey's
//! ([`Chord::from_parts`]); the text helpers here are shared.
//!
//! `keyboard` never runs on a peer: a node pressing keys on another
//! device's screen, unseen, is too dangerous to route.

use async_trait::async_trait;

use crate::caller::Caller;
use crate::error::CallError;
use crate::hotkey::Chord;

/// `type-text` refuses longer text: at typing speed, a 2 MB clipboard would
/// hold the keyboard for hours.
pub const MAX_TEXT: usize = 20_000;

#[async_trait]
pub trait KeyboardProvider: Send + Sync {
    /// Presses `key` with `mods` held, then releases them.
    async fn keystroke(
        &self,
        caller: &Caller,
        mods: Vec<String>,
        key: String,
    ) -> Result<(), CallError>;
    /// Types `text` into the focused app.
    async fn type_text(&self, caller: &Caller, text: String) -> Result<(), CallError>;
}

/// No keyboard on this node.
pub struct NoKeyboard;

#[async_trait]
impl KeyboardProvider for NoKeyboard {
    async fn keystroke(&self, _: &Caller, _: Vec<String>, _: String) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn type_text(&self, _: &Caller, _: String) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
}

/// The chord `keystroke` presses, or `failed` with the reason.
pub fn chord(mods: &[String], key: &str) -> Result<Chord, CallError> {
    Chord::from_parts(mods, key).map_err(CallError::Failed)
}

/// Checks `text` against [`MAX_TEXT`], counted in characters.
pub fn check_text(text: &str) -> Result<(), CallError> {
    let n = text.chars().count();
    if n > MAX_TEXT {
        return Err(CallError::Failed(format!(
            "text has {n} characters; type-text takes at most {MAX_TEXT}"
        )));
    }
    Ok(())
}

/// One step of typing: text to insert as it is, or a key that apps treat
/// as an action rather than a character.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    /// "Enter" or "Tab", as W3C codes like [`Chord`]'s.
    Key(&'static str),
}

/// Splits `text` into runs of plain characters and the keys for line
/// breaks and tabs. `\r\n` counts as one line break. Other control
/// characters are dropped.
pub fn pieces(text: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut run = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let key = match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                Some("Enter")
            }
            '\n' => Some("Enter"),
            '\t' => Some("Tab"),
            c if c.is_control() => continue,
            c => {
                run.push(c);
                None
            }
        };
        if let Some(key) = key {
            if !run.is_empty() {
                out.push(Piece::Text(std::mem::take(&mut run)));
            }
            out.push(Piece::Key(key));
        }
    }
    if !run.is_empty() {
        out.push(Piece::Text(run));
    }
    out
}

/// The key that types `c` on a US layout, as a W3C code like [`Chord`]'s,
/// and whether it needs Shift. None for characters that layout has no key
/// for, such as accented letters.
pub fn us_key(c: char) -> Option<(&'static str, bool)> {
    const LETTERS: [&str; 26] = [
        "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK",
        "KeyL", "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV",
        "KeyW", "KeyX", "KeyY", "KeyZ",
    ];
    const DIGITS: [&str; 10] = [
        "Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8",
        "Digit9",
    ];
    // The shifted digits, from 0 to 9.
    const SHIFTED_DIGITS: &str = ")!@#$%^&*(";
    if c.is_ascii_lowercase() {
        return Some((LETTERS[(c as u8 - b'a') as usize], false));
    }
    if c.is_ascii_uppercase() {
        return Some((LETTERS[(c as u8 - b'A') as usize], true));
    }
    if c.is_ascii_digit() {
        return Some((DIGITS[(c as u8 - b'0') as usize], false));
    }
    if let Some(i) = SHIFTED_DIGITS.find(c) {
        return Some((DIGITS[i], true));
    }
    let key = match c {
        ' ' => ("Space", false),
        '-' => ("Minus", false),
        '_' => ("Minus", true),
        '=' => ("Equal", false),
        '+' => ("Equal", true),
        '[' => ("BracketLeft", false),
        '{' => ("BracketLeft", true),
        ']' => ("BracketRight", false),
        '}' => ("BracketRight", true),
        '\\' => ("Backslash", false),
        '|' => ("Backslash", true),
        ';' => ("Semicolon", false),
        ':' => ("Semicolon", true),
        '\'' => ("Quote", false),
        '"' => ("Quote", true),
        ',' => ("Comma", false),
        '<' => ("Comma", true),
        '.' => ("Period", false),
        '>' => ("Period", true),
        '/' => ("Slash", false),
        '?' => ("Slash", true),
        '`' => ("Backquote", false),
        '~' => ("Backquote", true),
        _ => return None,
    };
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keystrokes_parse_like_chords() {
        let c = chord(&["cmd".into()], "v").unwrap();
        assert_eq!(c, Chord::parse("cmd+v").unwrap());
        assert!(matches!(chord(&[], "nope"), Err(CallError::Failed(_))));
    }

    #[test]
    fn long_text_is_refused() {
        assert!(check_text(&"é".repeat(MAX_TEXT)).is_ok());
        assert!(check_text(&"a".repeat(MAX_TEXT + 1)).is_err());
    }

    #[test]
    fn line_breaks_and_tabs_become_keys() {
        assert_eq!(
            pieces("one\ntwo\r\n\tthree\u{7}"),
            [
                Piece::Text("one".into()),
                Piece::Key("Enter"),
                Piece::Text("two".into()),
                Piece::Key("Enter"),
                Piece::Key("Tab"),
                Piece::Text("three".into()),
            ]
        );
        assert_eq!(pieces(""), []);
    }

    #[test]
    fn us_keys_cover_printable_ascii() {
        assert_eq!(us_key('a'), Some(("KeyA", false)));
        assert_eq!(us_key('Z'), Some(("KeyZ", true)));
        assert_eq!(us_key('0'), Some(("Digit0", false)));
        assert_eq!(us_key(')'), Some(("Digit0", true)));
        assert_eq!(us_key('@'), Some(("Digit2", true)));
        assert_eq!(us_key('?'), Some(("Slash", true)));
        assert_eq!(us_key(' '), Some(("Space", false)));
        assert_eq!(us_key('é'), None);
        assert_eq!(us_key('\n'), None);
        for c in (0x20u8..0x7f).map(char::from) {
            assert!(us_key(c).is_some(), "{c:?}");
        }
    }
}
