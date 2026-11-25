//! The OS permissions this process holds. On macOS, `window` needs
//! Accessibility, `keyboard` and `mouse` need Accessibility to post events,
//! the `fn` hotkey needs Input Monitoring, `mic` needs Microphone, `camera`
//! needs Camera and `screen` needs Screen Recording. Elsewhere none apply,
//! and each answer is `None`.

use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Permissions {
    /// `window`: the Accessibility API.
    pub accessibility: Option<bool>,
    /// `keyboard`: posting key events, also under Accessibility.
    pub post_events: Option<bool>,
    /// The `fn` hotkey: listening to key events, under Input Monitoring.
    pub input_monitoring: Option<bool>,
    /// `mic`: the ffmpeg child records under this process's grant.
    pub microphone: Option<bool>,
    /// `camera`, also through ffmpeg.
    pub camera: Option<bool>,
    /// `screen`, through `screencapture`.
    pub screen_recording: Option<bool>,
}

impl Permissions {
    /// Whether nothing the node needs at start is missing: Accessibility,
    /// posting events and Input Monitoring. Microphone, Camera and Screen
    /// Recording matter only to the apps that use them, so they don't count.
    pub fn complete(&self) -> bool {
        [self.accessibility, self.post_events, self.input_monitoring]
            .iter()
            .all(|p| *p != Some(false))
    }

    /// The answer for one grant, named as in `GRANTS`.
    pub fn get(&self, grant: &str) -> Option<bool> {
        match grant {
            "accessibility" => self.accessibility,
            "post_events" => self.post_events,
            "input_monitoring" => self.input_monitoring,
            "microphone" => self.microphone,
            "camera" => self.camera,
            "screen_recording" => self.screen_recording,
            _ => None,
        }
    }
}

/// One OS grant: its field name in `Permissions`, the name System Settings
/// shows, and its pane under Privacy & Security.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Grant {
    pub name: &'static str,
    pub label: &'static str,
    pub pane: &'static str,
}

/// Every grant, in the order the hub lists them.
pub const GRANTS: &[Grant] = &[
    Grant {
        name: "accessibility",
        label: "Accessibility",
        pane: "Privacy_Accessibility",
    },
    Grant {
        name: "post_events",
        label: "Post key and mouse events",
        pane: "Privacy_Accessibility",
    },
    Grant {
        name: "input_monitoring",
        label: "Input Monitoring",
        pane: "Privacy_ListenEvent",
    },
    Grant {
        name: "microphone",
        label: "Microphone",
        pane: "Privacy_Microphone",
    },
    Grant {
        name: "camera",
        label: "Camera",
        pane: "Privacy_Camera",
    },
    Grant {
        name: "screen_recording",
        label: "Screen Recording",
        pane: "Privacy_ScreenCapture",
    },
];

pub fn grant(name: &str) -> Option<&'static Grant> {
    GRANTS.iter().find(|g| g.name == name)
}

/// The URL that opens a pane of System Settings, such as
/// `Privacy_Microphone`.
pub fn settings_url(pane: &str) -> String {
    format!("x-apple.systempreferences:com.apple.preference.security?{pane}")
}

/// What this process holds now.
pub fn check() -> Permissions {
    #[cfg(target_os = "macos")]
    return crate::macos::permissions();
    #[cfg(not(target_os = "macos"))]
    Permissions::default()
}

/// The grants the OS has never asked about. On macOS only Microphone and
/// Camera can tell; the others answer missing.
pub fn not_asked() -> Vec<&'static str> {
    #[cfg(target_os = "macos")]
    return crate::macos::not_asked();
    #[cfg(not(target_os = "macos"))]
    Vec::new()
}

/// Asks the OS for what is missing of Accessibility, posting events and
/// Input Monitoring, then reports. On macOS this lists the process in
/// System Settings and shows each prompt once; the user still turns the
/// entries on.
pub fn request() -> Permissions {
    #[cfg(target_os = "macos")]
    crate::macos::request_permissions();
    check()
}

/// Asks the OS for one grant, then reports. Mic and camera answer later,
/// so the report may not show the answer yet.
pub fn request_grant(name: &str) -> Permissions {
    #[cfg(target_os = "macos")]
    crate::macos::request_grant(name);
    #[cfg(not(target_os = "macos"))]
    let _ = name;
    check()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_ignores_what_does_not_apply() {
        assert!(Permissions::default().complete());
        let missing = Permissions {
            post_events: Some(false),
            ..Permissions::default()
        };
        assert!(!missing.complete());
        let granted = Permissions {
            accessibility: Some(true),
            post_events: Some(true),
            input_monitoring: Some(true),
            ..Permissions::default()
        };
        assert!(granted.complete());
    }

    #[test]
    fn complete_ignores_the_sensor_grants() {
        let p = Permissions {
            accessibility: Some(true),
            post_events: Some(true),
            input_monitoring: Some(true),
            microphone: Some(false),
            camera: Some(false),
            screen_recording: Some(false),
        };
        assert!(p.complete());
    }

    #[test]
    fn every_grant_is_a_field_with_a_pane() {
        let all = Permissions {
            accessibility: Some(true),
            post_events: Some(true),
            input_monitoring: Some(true),
            microphone: Some(true),
            camera: Some(true),
            screen_recording: Some(true),
        };
        let json = serde_json::to_value(all).unwrap();
        assert_eq!(json.as_object().unwrap().len(), GRANTS.len());
        for g in GRANTS {
            assert_eq!(all.get(g.name), Some(true), "{}", g.name);
            assert!(g.pane.starts_with("Privacy_"), "{}", g.name);
        }
        assert_eq!(
            settings_url(grant("microphone").unwrap().pane),
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
        );
    }
}
