//! The logic, with no capability calls and no generated bindings, so
//! `cargo test` runs it natively.

/// The settings from `init`, with defaults for missing keys.
#[derive(Debug, PartialEq)]
pub struct Settings {
    pub chord: String,
    /// A [tts.profiles] name in node.toml.
    pub selection_voice: String,
    pub poll_ms: u32,
}

impl Settings {
    pub fn from_json(config: &str) -> Self {
        let c = serde_json::from_str::<serde_json::Value>(config).unwrap_or_default();
        let text = |key: &str, default: &str| {
            c[key]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(default)
                .to_string()
        };
        let poll_ms = c["poll_ms"]
            .as_u64()
            .and_then(|ms| u32::try_from(ms).ok())
            .unwrap_or(3_000)
            .max(10);
        Settings {
            chord: text("chord", "ctrl+alt+shift+s"),
            selection_voice: text("selection_voice", "reading"),
            poll_ms,
        }
    }
}

/// The queue as `tts.status` answers it; `None` when it didn't answer.
pub struct Status<'a> {
    pub speaking: bool,
    pub queued: u32,
    pub voice: Option<&'a str>,
}

/// "Speaking · 2 queued · voice alba".
pub fn describe(s: Option<&Status>) -> String {
    let Some(s) = s else {
        return "Speech queue not answering".into();
    };
    let mut line = String::from(if s.speaking { "Speaking" } else { "Idle" });
    if s.queued > 0 {
        line.push_str(&format!(" · {} queued", s.queued));
    }
    if let Some(voice) = s.voice {
        line.push_str(&format!(" · voice {voice}"));
    }
    line
}

/// Whether there is something to read: not none, not only whitespace.
pub fn readable(text: Option<&str>) -> bool {
    text.is_some_and(|t| !t.trim().is_empty())
}

/// What the clipboard holds while the copy is pending.
pub fn marker(unix_secs: u64) -> String {
    format!("bricolo-tts-{unix_secs}")
}

/// Whether the clipboard changed from `marker`, so the copy landed.
pub fn copied(text: Option<&str>, marker: &str) -> bool {
    text.is_some_and(|t| t != marker)
}

/// The label of the tile's selection button.
pub fn selection_label(chord: &str) -> String {
    format!("Speak the selection ({chord})")
}

/// The `err` text scripts see for a failed call: "unavailable",
/// "failed: why".
pub fn error_name(case: &str, message: Option<&str>) -> String {
    match message {
        Some(m) => format!("{case}: {m}"),
        None => case.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_the_queue() {
        let s = Status {
            speaking: true,
            queued: 2,
            voice: Some("alba"),
        };
        assert_eq!(describe(Some(&s)), "Speaking · 2 queued · voice alba");
        let idle = Status {
            speaking: false,
            queued: 0,
            voice: None,
        };
        assert_eq!(describe(Some(&idle)), "Idle");
        assert_eq!(describe(None), "Speech queue not answering");
    }

    #[test]
    fn whitespace_is_not_readable() {
        assert!(readable(Some("hello")));
        assert!(!readable(Some(" \n\t")));
        assert!(!readable(Some("")));
        assert!(!readable(None));
    }

    #[test]
    fn notices_when_the_copy_lands() {
        let m = marker(1_700_000_000);
        assert_eq!(m, "bricolo-tts-1700000000");
        assert!(!copied(Some(&m), &m));
        assert!(!copied(None, &m));
        assert!(copied(Some("the selection"), &m));
    }

    #[test]
    fn labels_and_errors() {
        assert_eq!(
            selection_label("ctrl+alt+shift+s"),
            "Speak the selection (ctrl+alt+shift+s)"
        );
        assert_eq!(error_name("denied", None), "denied");
        assert_eq!(error_name("failed", Some("no voice")), "failed: no voice");
    }

    #[test]
    fn settings_fall_back_to_defaults() {
        let d = Settings::from_json("{}");
        assert_eq!(
            d,
            Settings {
                chord: "ctrl+alt+shift+s".into(),
                selection_voice: "reading".into(),
                poll_ms: 3_000,
            }
        );
        let s = Settings::from_json(
            r#"{"chord":"cmd+alt+r","selection_voice":"alba","poll_ms":1000}"#,
        );
        assert_eq!(s.chord, "cmd+alt+r");
        assert_eq!(s.selection_voice, "alba");
        assert_eq!(s.poll_ms, 1_000);
        assert_eq!(Settings::from_json(r#"{"poll_ms":1}"#).poll_ms, 10);
        assert_eq!(Settings::from_json("junk"), d);
    }
}
