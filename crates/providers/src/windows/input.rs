//! `SendInput` for `keyboard`, `mouse` and the focus workaround: building
//! key events and sending them.
//!
//! Windows drops synthetic input aimed at a window of a higher integrity
//! level (an app run as administrator) when the node is not elevated:
//! `SendInput` then inserts nothing, and the call answers `denied`.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE,
    MOUSE_EVENT_FLAGS, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};

use crate::error::CallError;
use crate::win;

fn keyboard(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn up_flag(up: bool) -> KEYBD_EVENT_FLAGS {
    if up {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS(0)
    }
}

/// A key by its scan code: a physical position, whatever the layout.
pub fn scan(code: u16, extended: bool, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_SCANCODE | up_flag(up);
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    keyboard(0, code, flags)
}

/// A key by its virtual-key code. The right-hand modifiers and the
/// Windows keys are extended keys.
pub fn virtual_key(vk: u16, up: bool) -> INPUT {
    let mut flags = up_flag(up);
    if matches!(vk, 0xa3 | 0xa5 | 0x5b | 0x5c) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    keyboard(vk, 0, flags)
}

/// One UTF-16 unit typed as itself, whatever the layout.
pub fn unicode(unit: u16, up: bool) -> INPUT {
    keyboard(0, unit, KEYEVENTF_UNICODE | up_flag(up))
}

pub fn mouse(flags: MOUSE_EVENT_FLAGS, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                // The wheel's signed delta, as the field's DWORD.
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Sends `inputs` as one uninterrupted sequence.
pub fn send(inputs: &[INPUT]) -> Result<(), CallError> {
    if inputs.is_empty() {
        return Ok(());
    }
    // SAFETY: a slice of initialized INPUTs and their size.
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) } as usize;
    match sent {
        n if n == inputs.len() => Ok(()),
        0 => {
            tracing::debug!("SendInput inserted nothing: the focused app may run as administrator");
            Err(CallError::Denied)
        }
        n => Err(CallError::Failed(format!(
            "SendInput inserted {n} of {} events",
            inputs.len()
        ))),
    }
}

/// Whether the key is down now.
pub fn is_down(vk: u16) -> bool {
    // SAFETY: a plain read; the high bit says the key is down.
    let state = unsafe { GetAsyncKeyState(i32::from(vk)) };
    state < 0
}

/// Releases the modifier keys the user still holds, such as the chord
/// that triggered the call, so they don't change what gets typed. A held
/// Windows or Alt key gets an unassigned key pressed first, so that its
/// release opens neither the Start menu nor the menu bar.
pub fn release_held_modifiers() -> Result<(), CallError> {
    let held: Vec<u16> = win::MODIFIER_KEYS
        .into_iter()
        .filter(|vk| is_down(*vk))
        .collect();
    let mut inputs = Vec::new();
    if held.iter().any(|vk| win::needs_mask(*vk)) {
        inputs.push(virtual_key(win::MASK_KEY, false));
        inputs.push(virtual_key(win::MASK_KEY, true));
    }
    inputs.extend(held.iter().map(|vk| virtual_key(*vk, true)));
    send(&inputs)
}
