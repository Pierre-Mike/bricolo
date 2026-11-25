//! `keyboard` on Windows, through `SendInput`. `keystroke` presses keys
//! by their scan codes, so names are physical positions on a US keyboard
//! whatever the layout; `cmd` is the Windows key. `type-text` sends each
//! character as itself (`KEYEVENTF_UNICODE`), so any text types on any
//! layout; line breaks and tabs press Enter and Tab. Modifiers the user
//! still holds are released first.

use std::time::Duration;

use async_trait::async_trait;
use windows::Win32::UI::Input::KeyboardAndMouse::INPUT;

use super::input;
use super::window::blocking;
use crate::caller::Caller;
use crate::error::CallError;
use crate::hotkey::Chord;
use crate::keyboard::{self, KeyboardProvider, Piece};
use crate::win;

/// Characters sent in one `SendInput` call, with a pause between calls so
/// slow apps keep up.
const BATCH: usize = 100;
const PAUSE: Duration = Duration::from_millis(10);

pub struct WinKeyboard;

fn key_scan(code: &str) -> Result<(u16, bool), CallError> {
    win::scan_code(code).ok_or_else(|| CallError::Failed(format!("no Windows key for {code}")))
}

fn press(inputs: &mut Vec<INPUT>, (code, extended): (u16, bool)) {
    inputs.push(input::scan(code, extended, false));
    inputs.push(input::scan(code, extended, true));
}

pub(super) fn keystroke(chord: &Chord) -> Result<(), CallError> {
    let key = key_scan(&chord.code)?;
    let mods: Vec<(u16, bool)> = chord.mods.iter().map(|m| win::modifier_scan(*m)).collect();
    let mut inputs = Vec::new();
    for (code, extended) in &mods {
        inputs.push(input::scan(*code, *extended, false));
    }
    press(&mut inputs, key);
    for (code, extended) in mods.iter().rev() {
        inputs.push(input::scan(*code, *extended, true));
    }
    input::release_held_modifiers()?;
    input::send(&inputs)
}

pub(super) fn type_text(text: &str) -> Result<(), CallError> {
    fn flush(inputs: &mut Vec<INPUT>, chars: &mut usize) -> Result<(), CallError> {
        input::send(inputs)?;
        inputs.clear();
        *chars = 0;
        std::thread::sleep(PAUSE);
        Ok(())
    }
    input::release_held_modifiers()?;
    let mut inputs = Vec::new();
    let mut chars = 0;
    for piece in keyboard::pieces(text) {
        match piece {
            Piece::Key(code) => {
                press(&mut inputs, key_scan(code)?);
                chars += 1;
            }
            Piece::Text(run) => {
                for c in run.chars() {
                    let mut units = [0u16; 2];
                    for unit in c.encode_utf16(&mut units) {
                        inputs.push(input::unicode(*unit, false));
                        inputs.push(input::unicode(*unit, true));
                    }
                    chars += 1;
                    if chars >= BATCH {
                        flush(&mut inputs, &mut chars)?;
                    }
                }
            }
        }
        if chars >= BATCH {
            flush(&mut inputs, &mut chars)?;
        }
    }
    input::send(&inputs)
}

#[async_trait]
impl KeyboardProvider for WinKeyboard {
    async fn keystroke(&self, _: &Caller, mods: Vec<String>, key: String) -> Result<(), CallError> {
        let chord = keyboard::chord(&mods, &key)?;
        blocking(move || keystroke(&chord)).await
    }

    async fn type_text(&self, _: &Caller, text: String) -> Result<(), CallError> {
        keyboard::check_text(&text)?;
        blocking(move || type_text(&text)).await
    }
}
