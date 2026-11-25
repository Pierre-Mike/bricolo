//! The logic, with no capability calls and no generated bindings, so
//! `cargo test` runs it natively.

/// The longest password the plugin types; `keyboard.type-text` stops at
/// 20000 characters, and no password comes near.
pub const MAX_PASSWORD: usize = 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub chord: String,
    /// The touch-id sidecar's endpoint. Its host:port must be in `[http] allow`.
    pub url: String,
    /// The saved passwords, by name, in the order the picker shows them.
    pub names: Vec<String>,
    /// Ask for Touch ID before typing or saving.
    pub touch_id: bool,
    /// How long to wait for the Touch ID prompt, in milliseconds.
    pub timeout_ms: u32,
    /// Names from the settings that `secrets` would refuse.
    pub rejected: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            chord: "cmd+alt+shift+p".into(),
            url: "http://127.0.0.1:8804/authenticate".into(),
            names: vec!["main".into()],
            touch_id: true,
            timeout_ms: 60_000,
            rejected: Vec::new(),
        }
    }
}

impl Settings {
    /// Reads the settings JSON from `init`. Missing or wrong values keep
    /// their defaults.
    pub fn from_json(config: &str) -> Self {
        let mut settings = Self::default();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(config) else {
            return settings;
        };
        if let Some(chord) = value["chord"].as_str() {
            settings.chord = chord.into();
        }
        if let Some(url) = value["url"].as_str() {
            settings.url = url.into();
        }
        if let Some(on) = value["touch_id"].as_bool() {
            settings.touch_id = on;
        }
        if let Some(ms) = value["timeout_ms"].as_u64() {
            settings.timeout_ms = ms.clamp(1_000, 300_000) as u32;
        }
        if let Some(list) = value["names"].as_array() {
            let mut names: Vec<String> = Vec::new();
            for name in list.iter().filter_map(|n| n.as_str()) {
                if !valid_name(name) {
                    settings.rejected.push(name.into());
                } else if !names.iter().any(|n| n == name) {
                    names.push(name.into());
                }
            }
            if !names.is_empty() {
                settings.names = names;
            }
        }
        settings
    }
}

/// A name `secrets` accepts: 1 to 128 letters, digits, `.`, `_` or `-`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// What the picker was opened for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Type,
    Save,
}

impl Action {
    pub fn picker_title(self) -> &'static str {
        match self {
            Action::Type => "Type which password?",
            Action::Save => "Set which password?",
        }
    }

    /// What the Touch ID prompt says after "touch-id is trying to".
    pub fn reason(self, name: &str) -> String {
        match self {
            Action::Type => format!("type the password \"{name}\""),
            Action::Save => format!("save the password \"{name}\""),
        }
    }
}

/// The name the user picked, or none when the list was dismissed.
pub fn picked(names: &[String], index: Option<u32>) -> Option<&str> {
    names.get(index? as usize).map(String::as_str)
}

/// Whether the touch-id sidecar confirmed, from its status and body.
pub fn read_auth(status: u16, body: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(body).trim().to_string();
    match status {
        200 => Ok(()),
        401 => Err(format!("Touch ID {text}")),
        408 => Err("Touch ID timed out".into()),
        409 => Err("a Touch ID prompt is already open".into()),
        _ => Err(format!("Touch ID answered {status}: {text}")),
    }
}

/// Why a password can't be typed, or none when it can.
pub fn check_password(name: &str, value: Option<&str>) -> Result<(), String> {
    match value {
        None | Some("") => Err(format!(
            "No password saved as \"{name}\". Press \"Set password…\" in the hub."
        )),
        Some(v) if v.chars().count() > MAX_PASSWORD => {
            Err(format!("\"{name}\" is longer than {MAX_PASSWORD} characters"))
        }
        Some(_) => Ok(()),
    }
}

/// The program that shows the password dialog.
pub const OSASCRIPT: &str = "/usr/bin/osascript";
/// The dialog gives up before `shell` kills a program at 60 s.
pub const DIALOG_SECS: u32 = 55;

/// osascript arguments for a dialog with a hidden field, the macOS one
/// that shows dots. `prompt` is passed as an argument, never pasted into
/// the script. The script prints `=` and what was typed, or `gave-up`.
pub fn dialog_args(prompt: &str) -> Vec<String> {
    let script = [
        "on run argv",
        "activate",
        "set r to display dialog (item 1 of argv) default answer \"\" with hidden answer \
         with title \"password-typer\" buttons {\"Cancel\", \"Save\"} default button \"Save\" \
         cancel button \"Cancel\" giving up after (item 2 of argv as integer)",
        "if gave up of r then return \"gave-up\"",
        "return \"=\" & text returned of r",
        "end run",
    ];
    let mut args: Vec<String> = script.iter().flat_map(|line| ["-e".to_string(), line.to_string()]).collect();
    args.push(prompt.to_string());
    args.push(DIALOG_SECS.to_string());
    args
}

/// The prompts of the two dialogs: type it, then type it again.
pub fn dialog_prompts(name: &str) -> [String; 2] {
    [
        format!("Type the password to save as \"{name}\"."),
        format!("Type the password for \"{name}\" again."),
    ]
}

/// The password from the dialog's exit code and output, or why not. Never
/// puts the typed text in the error.
pub fn read_dialog(code: Option<i32>, stdout: &str, stderr: &str) -> Result<String, String> {
    if code != Some(0) {
        // osascript exits 1 with error -128 when you press Cancel or Escape.
        if stderr.contains("-128") {
            return Err("Cancelled; nothing saved".into());
        }
        return Err(format!("the password dialog failed: {}", stderr.trim()));
    }
    let Some(text) = stdout.strip_prefix('=') else {
        return Err("Nobody typed the password in time; nothing saved".into());
    };
    // osascript ends its output with one newline.
    let text = text.strip_suffix('\n').unwrap_or(text);
    if text.is_empty() {
        return Err("The password is empty; nothing saved".into());
    }
    if text.contains(['\r', '\n']) {
        return Err("The password holds a line break; nothing saved".into());
    }
    if text.chars().count() > MAX_PASSWORD {
        return Err(format!("The password is longer than {MAX_PASSWORD} characters; nothing saved"));
    }
    Ok(text.to_string())
}

/// The password to save once both dialogs agree.
pub fn matching(first: String, second: &str) -> Result<String, String> {
    if first == second {
        Ok(first)
    } else {
        Err("The two passwords differ; nothing saved".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let s = Settings::from_json("");
        assert_eq!(s.chord, "cmd+alt+shift+p");
        assert_eq!(s.names, vec!["main".to_string()]);
        assert!(s.touch_id);
    }

    #[test]
    fn settings_keep_good_names_once_and_report_bad_ones() {
        let s = Settings::from_json(
            r#"{"names":["work","wifi","work","bad name","", "x/y"],"touch_id":false,"timeout_ms":5}"#,
        );
        assert_eq!(s.names, vec!["work".to_string(), "wifi".to_string()]);
        assert_eq!(s.rejected, vec!["bad name".to_string(), "".into(), "x/y".into()]);
        assert!(!s.touch_id);
        assert_eq!(s.timeout_ms, 1_000);
    }

    #[test]
    fn no_good_names_keeps_main() {
        assert_eq!(Settings::from_json(r#"{"names":["a b"]}"#).names, vec!["main".to_string()]);
    }

    #[test]
    fn picks_by_index() {
        let names = vec!["work".to_string(), "wifi".to_string()];
        assert_eq!(picked(&names, Some(1)), Some("wifi"));
        assert_eq!(picked(&names, Some(2)), None);
        assert_eq!(picked(&names, None), None);
    }

    #[test]
    fn reads_the_sidecar() {
        assert_eq!(read_auth(200, b"ok\n"), Ok(()));
        assert_eq!(read_auth(401, b"cancelled\n"), Err("Touch ID cancelled".into()));
        assert_eq!(read_auth(408, b""), Err("Touch ID timed out".into()));
        assert!(read_auth(503, b"Touch ID can't run: no fingers").unwrap_err().contains("no fingers"));
    }

    #[test]
    fn checks_the_saved_password() {
        assert!(check_password("work", Some("hunter2")).is_ok());
        assert!(check_password("work", None).unwrap_err().contains("Set password"));
        assert!(check_password("work", Some("")).is_err());
        assert!(check_password("work", Some(&"x".repeat(MAX_PASSWORD + 1))).is_err());
    }

    #[test]
    fn reads_what_the_dialog_printed() {
        assert_eq!(read_dialog(Some(0), "=s3cret\n", ""), Ok("s3cret".into()));
        assert_eq!(read_dialog(Some(0), "= spaced \n", ""), Ok(" spaced ".into()));
        assert_eq!(read_dialog(Some(0), "==x\n", ""), Ok("=x".into()));
        assert!(read_dialog(Some(0), "gave-up\n", "").unwrap_err().contains("in time"));
        assert!(read_dialog(Some(0), "=\n", "").unwrap_err().contains("empty"));
        assert!(read_dialog(Some(0), "=a\nb\n", "").is_err());
        let long = format!("={}\n", "x".repeat(MAX_PASSWORD + 1));
        assert!(read_dialog(Some(0), &long, "").is_err());
        let cancel = "execution error: User canceled. (-128)\n";
        assert_eq!(read_dialog(Some(1), "", cancel), Err("Cancelled; nothing saved".into()));
        assert!(read_dialog(None, "=s3cret\n", "").unwrap_err().contains("failed"));
    }

    #[test]
    fn errors_never_hold_the_password() {
        for (code, out) in [(Some(0), "=a\nhunter2\n"), (Some(1), "=hunter2\n")] {
            assert!(!read_dialog(code, out, "").unwrap_err().contains("hunter2"));
        }
        assert!(!matching("hunter2".into(), "hunter3").unwrap_err().contains("hunter"));
    }

    #[test]
    fn both_entries_must_match() {
        assert_eq!(matching("a".into(), "a"), Ok("a".into()));
        assert!(matching("a".into(), "b").is_err());
    }

    #[test]
    fn the_prompt_is_an_argument_not_script() {
        let args = dialog_args("Type \"x\" & quit");
        let n = args.len();
        assert_eq!(args[n - 2], "Type \"x\" & quit");
        assert_eq!(args[n - 1], DIALOG_SECS.to_string());
        assert!(args[..n - 2].chunks(2).all(|pair| pair[0] == "-e"));
        assert!(!args[..n - 2].iter().any(|a| a.contains("quit")));
    }

    #[test]
    fn reasons_name_the_password() {
        assert_eq!(Action::Type.reason("work"), "type the password \"work\"");
        assert_eq!(Action::Save.reason("wifi"), "save the password \"wifi\"");
    }
}
