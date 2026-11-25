//! `keyboard` on Hyprland.
//!
//! - `keystroke` asks Hyprland to send the chord to the focused window
//!   (`send_key_state` down and up, or legacy `sendshortcut`). Keys the
//!   user still holds do not mix in. Hyprland resolves the key name in the
//!   keymap of the last keyboard used. After a `wtype` run (our `type-text`
//!   or anyone's) that is `wtype`'s own keymap, which holds only the keys it
//!   just typed, so Hyprland answers "key not found" until the user presses
//!   a real key. Worse, while the focused app still holds `wtype`'s keymap,
//!   a `send_key_state` that Hyprland accepts can arrive as the wrong key or
//!   out of order. So after any `wtype` run of ours ([`WTYPE_KEYMAP`]), and
//!   whenever Hyprland answers "key not found", `keystroke` uses
//!   `wtype -M <mods> -k <key>` instead, after the same wait for released
//!   modifiers as `type-text`. A hotkey press, a real key, clears the flag.
//!   `wtype`'s one-level keymap delivers Shift as a modifier but does not
//!   change the character (shift+1 is not "!"), which is fine for shortcuts.
//! - `type-text` runs `wtype`, which types through the virtual keyboard
//!   protocol. Hyprland merges a virtual keyboard's keys with the modifiers
//!   held on the real one, so with the Lua config the node first waits (up
//!   to 2 s) until no modifier is down: after the chord that triggered the
//!   call, typing starts once the user lets go. Without `wtype`,
//!   `type-text` answers `unavailable`.

use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;

use super::{Dialect, Hyprland, Keys, cmd};
use crate::error::CallError;
use crate::keyboard::{Piece, check_text, chord, pieces};
use crate::linux::{warn_once, which};

/// The modifier keys `type-text` waits for.
const MODIFIERS: [&str; 8] = [
    "Super_L",
    "Super_R",
    "Alt_L",
    "Alt_R",
    "Control_L",
    "Control_R",
    "Shift_L",
    "Shift_R",
];
const RELEASE_WAIT: Duration = Duration::from_secs(2);

/// Set after we run `wtype`: the focused app may still read keys with
/// `wtype`'s keymap. The Hyprland hotkey thread clears it on a key press.
pub static WTYPE_KEYMAP: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
pub struct HyprKeyboard {
    warned: AtomicBool,
}

impl HyprKeyboard {
    pub async fn keystroke(
        &self,
        hypr: Hyprland,
        mods: Vec<String>,
        key: String,
    ) -> Result<(), CallError> {
        let parsed = chord(&mods, &key)?;
        let keys = Keys::from_chord(&parsed)
            .ok_or_else(|| CallError::Failed(format!("no Hyprland key name for {key}")))?;
        let wtype = match which("wtype") {
            Some(w) if WTYPE_KEYMAP.load(Ordering::SeqCst) => w,
            found => {
                let sent = super::window::blocking(hypr.clone(), move |h| {
                    h.run(&cmd::keystroke(h.dialect(), &keys))
                })
                .await;
                match (sent, found) {
                    (Err(CallError::Failed(e)), Some(w)) if e.contains("key not found") => {
                        tracing::debug!("keyboard: Hyprland has no key {key}; using wtype");
                        w
                    }
                    (sent, _) => return sent,
                }
            }
        };
        let keys = Keys::from_chord(&parsed).expect("parsed above");
        if hypr.dialect() == Dialect::Lua {
            tokio::task::spawn_blocking(move || wait_for_release(&hypr))
                .await
                .map_err(CallError::failed)?;
        }
        WTYPE_KEYMAP.store(true, Ordering::SeqCst);
        let out = tokio::process::Command::new(&wtype)
            .args(wtype_chord(&keys))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(CallError::failed)?;
        if !out.status.success() {
            return Err(CallError::Failed(format!(
                "wtype failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }

    pub async fn type_text(&self, hypr: Hyprland, text: String) -> Result<(), CallError> {
        check_text(&text)?;
        let Some(wtype) = which("wtype") else {
            warn_once(
                &self.warned,
                "keyboard: type-text on Hyprland needs wtype; install it to type text",
            );
            return Err(CallError::Unavailable);
        };
        if hypr.dialect() == Dialect::Lua {
            tokio::task::spawn_blocking(move || wait_for_release(&hypr))
                .await
                .map_err(CallError::failed)?;
        }
        WTYPE_KEYMAP.store(true, Ordering::SeqCst);
        for piece in pieces(&text) {
            let mut command = tokio::process::Command::new(&wtype);
            command.stdout(Stdio::null()).stderr(Stdio::piped());
            let input = match piece {
                Piece::Text(t) => {
                    command.arg("-").stdin(Stdio::piped());
                    Some(t)
                }
                Piece::Key(k) => {
                    command.args(["-k", wtype_key(k)]).stdin(Stdio::null());
                    None
                }
            };
            let mut child = command.spawn().map_err(CallError::failed)?;
            if let Some(t) = input
                && let Some(mut stdin) = child.stdin.take()
            {
                stdin
                    .write_all(t.as_bytes())
                    .await
                    .map_err(CallError::failed)?;
            }
            let out = child.wait_with_output().await.map_err(CallError::failed)?;
            if !out.status.success() {
                return Err(CallError::Failed(format!(
                    "wtype failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
        }
        Ok(())
    }
}

/// The keysym `wtype -k` takes for a key from [`pieces`].
fn wtype_key(code: &str) -> &'static str {
    match code {
        "Tab" => "Tab",
        _ => "Return",
    }
}

/// `wtype` arguments that press `keys`: modifiers down, the key, modifiers
/// up. Letters go lowercase, the keysym on an unshifted key.
fn wtype_chord(keys: &Keys) -> Vec<String> {
    let mods: Vec<&str> = [
        (super::SHIFT, "shift"),
        (super::CTRL, "ctrl"),
        (super::ALT, "alt"),
        (super::SUPER, "logo"),
    ]
    .into_iter()
    .filter(|(bit, _)| keys.mods & bit != 0)
    .map(|(_, name)| name)
    .collect();
    let key = if keys.key.len() == 1 {
        keys.key.to_ascii_lowercase()
    } else {
        keys.key.to_string()
    };
    let mut args = Vec::new();
    for m in &mods {
        args.extend(["-M".to_string(), m.to_string()]);
    }
    args.extend(["-k".to_string(), key]);
    for m in mods.iter().rev() {
        args.extend(["-m".to_string(), m.to_string()]);
    }
    args
}

/// Lua that is true while any modifier is down.
pub fn held_expr() -> String {
    let checks: Vec<String> = MODIFIERS
        .iter()
        .map(|k| format!("hl.is_key_down({})", super::lua_quote(k)))
        .collect();
    format!("({})", checks.join(" or "))
}

fn wait_for_release(hypr: &Hyprland) {
    let expr = held_expr();
    let deadline = Instant::now() + RELEASE_WAIT;
    while Instant::now() < deadline {
        match hypr.ask(&expr) {
            Ok(v) if v == "false" => return,
            Ok(_) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => {
                tracing::debug!("keyboard: cannot read the modifiers: {e}");
                return;
            }
        }
    }
    tracing::debug!("keyboard: modifiers still held after 2 s; typing anyway");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_for_every_modifier() {
        let expr = held_expr();
        assert!(expr.starts_with("(hl.is_key_down(\"Super_L\") or "));
        assert_eq!(expr.matches("is_key_down").count(), MODIFIERS.len());
        assert_eq!(wtype_key("Enter"), "Return");
        assert_eq!(wtype_key("Tab"), "Tab");
    }

    fn keys(chord: &str) -> Keys {
        Keys::from_chord(&crate::hotkey::Chord::parse(chord).unwrap()).unwrap()
    }

    #[test]
    fn builds_wtype_chords() {
        assert_eq!(
            wtype_chord(&keys("cmd+v")),
            ["-M", "logo", "-k", "v", "-m", "logo"]
        );
        assert_eq!(
            wtype_chord(&keys("ctrl+shift+a")),
            [
                "-M", "shift", "-M", "ctrl", "-k", "a", "-m", "ctrl", "-m", "shift"
            ]
        );
        assert_eq!(wtype_chord(&keys("enter")), ["-k", "Return"]);
        assert_eq!(
            wtype_chord(&keys("alt+left")),
            ["-M", "alt", "-k", "Left", "-m", "alt"]
        );
    }
}
