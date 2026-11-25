//! The standard library as the hub's Library view lists it: each
//! capability with the first sentence of its `wit/` doc comment, and the OS
//! grant it needs on macOS. The descriptions are read from `wit/` at
//! compile time, so they match what `bricolo wit` prints.

use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Capability {
    pub name: &'static str,
    /// The first sentence of the interface's doc comment.
    pub summary: String,
    /// The grant in `permissions::GRANTS` it needs on macOS, if any.
    pub needs: Option<&'static str>,
    /// When the grant applies to only part of the capability.
    pub note: Option<&'static str>,
    /// A second grant some of its calls need, like Screen Recording for
    /// `accessibility` screenshots.
    pub also: Option<&'static str>,
}

/// Each capability: its name, its WIT file, and the grant it needs.
const ENTRIES: &[(&str, &str, Option<&str>, Option<&str>)] = &[
    ("log", include_str!("../../../wit/log.wit"), None, None),
    ("timer", include_str!("../../../wit/timer.wit"), None, None),
    (
        "hotkey",
        include_str!("../../../wit/hotkey.wit"),
        Some("input_monitoring"),
        Some("only the fn key"),
    ),
    ("tts", include_str!("../../../wit/tts.wit"), None, None),
    (
        "speech",
        include_str!("../../../wit/speech.wit"),
        None,
        None,
    ),
    ("disk", include_str!("../../../wit/disk.wit"), None, None),
    (
        "clipboard",
        include_str!("../../../wit/clipboard.wit"),
        None,
        None,
    ),
    ("kv", include_str!("../../../wit/kv.wit"), None, None),
    (
        "window",
        include_str!("../../../wit/window.wit"),
        Some("accessibility"),
        None,
    ),
    ("app", include_str!("../../../wit/app.wit"), None, None),
    (
        "notify",
        include_str!("../../../wit/notify.wit"),
        None,
        None,
    ),
    ("shell", include_str!("../../../wit/shell.wit"), None, None),
    ("http", include_str!("../../../wit/http.wit"), None, None),
    ("ui", include_str!("../../../wit/ui.wit"), None, None),
    (
        "keyboard",
        include_str!("../../../wit/keyboard.wit"),
        Some("post_events"),
        None,
    ),
    (
        "mic",
        include_str!("../../../wit/mic.wit"),
        Some("microphone"),
        None,
    ),
    ("audio", include_str!("../../../wit/audio.wit"), None, None),
    (
        "system",
        include_str!("../../../wit/system.wit"),
        None,
        None,
    ),
    ("files", include_str!("../../../wit/files.wit"), None, None),
    (
        "mouse",
        include_str!("../../../wit/mouse.wit"),
        Some("post_events"),
        Some("moving and clicking"),
    ),
    (
        "screen",
        include_str!("../../../wit/screen.wit"),
        Some("screen_recording"),
        None,
    ),
    (
        "selection",
        include_str!("../../../wit/selection.wit"),
        Some("accessibility"),
        None,
    ),
    (
        "camera",
        include_str!("../../../wit/camera.wit"),
        Some("camera"),
        None,
    ),
    (
        "secrets",
        include_str!("../../../wit/secrets.wit"),
        None,
        None,
    ),
    (
        "claude-code",
        include_str!("../../../wit/claude-code.wit"),
        None,
        None,
    ),
    (
        "accessibility",
        include_str!("../../../wit/accessibility.wit"),
        Some("accessibility"),
        Some("screenshots also need Screen Recording"),
    ),
    (
        "process",
        include_str!("../../../wit/process.wit"),
        None,
        None,
    ),
    (
        "device",
        include_str!("../../../wit/device.wit"),
        None,
        None,
    ),
    (
        "scripting",
        include_str!("../../../wit/scripting.wit"),
        None,
        Some("each app a script drives asks for Automation access once"),
    ),
    (
        "virtual-screen",
        include_str!("../../../wit/virtual-screen.wit"),
        None,
        None,
    ),
];

/// Grants a capability needs for part of its calls, besides `needs`:
/// `accessibility` takes screenshots with Screen Recording.
const ALSO: &[(&str, &str)] = &[("accessibility", "screen_recording")];

/// The capabilities that need `grant`, fully or for some calls, in world
/// order: what `bricolo permissions` lists under each grant.
pub fn needing(grant: &str) -> Vec<&'static str> {
    ENTRIES
        .iter()
        .filter(|(name, _, needs, _)| *needs == Some(grant) || ALSO.contains(&(*name, grant)))
        .map(|(name, ..)| *name)
        .collect()
}

/// Every capability, in the order `wit/world.wit` imports them.
pub fn capabilities() -> Vec<Capability> {
    ENTRIES
        .iter()
        .map(|(name, wit, needs, note)| Capability {
            name,
            summary: summary(wit),
            needs: *needs,
            note: *note,
            also: ALSO.iter().find(|(c, _)| c == name).map(|(_, g)| *g),
        })
        .collect()
}

/// The first sentence of the `///` comment right above `interface`.
fn summary(wit: &str) -> String {
    let mut doc: Vec<&str> = Vec::new();
    for line in wit.lines() {
        let line = line.trim();
        if let Some(text) = line.strip_prefix("///") {
            doc.push(text.trim());
        } else if line.starts_with("interface ") {
            break;
        } else {
            doc.clear();
        }
    }
    // Code marks read as noise in the hub, which shows plain text.
    let text = doc.join(" ").replace('`', "");
    match text.find(". ") {
        Some(end) => text[..=end].to_string(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::grant;

    #[test]
    fn matches_the_world_imports() {
        let world = include_str!("../../../wit/world.wit");
        let imports: Vec<&str> = world
            .lines()
            .filter_map(|l| l.trim().strip_prefix("import "))
            .map(|l| l.trim_end_matches(';'))
            .collect();
        let names: Vec<&str> = capabilities().iter().map(|c| c.name).collect();
        assert_eq!(names, imports);
    }

    #[test]
    fn every_capability_has_a_summary_and_a_known_grant() {
        for c in capabilities() {
            assert!(c.summary.ends_with('.'), "{}: {:?}", c.name, c.summary);
            assert!(!c.summary.contains('`'), "{}: {:?}", c.name, c.summary);
            if let Some(g) = c.needs {
                assert!(grant(g).is_some(), "{}: unknown grant {g}", c.name);
            }
        }
    }

    #[test]
    fn lists_the_capabilities_each_grant_serves() {
        assert_eq!(
            needing("accessibility"),
            ["window", "selection", "accessibility"]
        );
        assert_eq!(needing("screen_recording"), ["screen", "accessibility"]);
        assert!(needing("post_events").contains(&"keyboard"));
        for (cap, g) in ALSO {
            assert!(grant(g).is_some() && capabilities().iter().any(|c| c.name == *cap));
        }
    }

    #[test]
    fn takes_the_first_sentence() {
        let mic = capabilities()
            .into_iter()
            .find(|c| c.name == "mic")
            .unwrap();
        assert_eq!(
            mic.summary,
            "Records the microphone of this device into the caller's data folder, through ffmpeg."
        );
        assert_eq!(
            summary("/// Not this.\n\n/// Timers. More.\ninterface timer {}"),
            "Timers."
        );
    }
}
