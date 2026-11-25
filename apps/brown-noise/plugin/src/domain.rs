//! The logic, with no capability calls and no generated bindings, so
//! `cargo test` runs it natively.

/// Ids and labels, in the order the tile shows them.
pub const COLORS: &[(&str, &str)] = &[
    ("brown", "Brown"),
    ("pink", "Pink"),
    ("white", "White"),
    ("blue", "Blue"),
    ("violet", "Violet"),
];

pub const DEFAULT_COLOR: &str = "brown";
pub const DEFAULT_VOLUME: u8 = 40;

/// The settings from `init`, with defaults for missing keys.
#[derive(Debug, PartialEq)]
pub struct Settings {
    /// The folder holding noise_<color>.wav.
    pub sounds: String,
    /// How much a volume button changes the volume.
    pub step: u8,
}

impl Settings {
    pub fn from_json(config: &str) -> Self {
        let c = serde_json::from_str::<serde_json::Value>(config).unwrap_or_default();
        let sounds = c["sounds"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("~/.config/bricolo/sounds/noise")
            .trim_end_matches('/')
            .to_string();
        let step = c["step"]
            .as_u64()
            .map(|s| s.clamp(1, 100) as u8)
            .unwrap_or(10);
        Settings { sounds, step }
    }
}

/// The label of color `id`, or `id` itself when it is not a known color.
pub fn label_of(id: &str) -> &str {
    COLORS
        .iter()
        .find(|(c, _)| *c == id)
        .map_or(id, |(_, label)| label)
}

/// The color kept in kv, brown when there is none.
pub fn color(kept: Option<String>) -> String {
    kept.unwrap_or_else(|| DEFAULT_COLOR.into())
}

/// The volume kept in kv ("40"), 40 when there is none or it is not a
/// number.
pub fn volume(kept: Option<&str>) -> u8 {
    kept.and_then(|v| v.trim().parse::<i64>().ok())
        .map_or(DEFAULT_VOLUME, |v| v.clamp(0, 100) as u8)
}

/// The volume after pressing "up" or "down", kept between 0 and 100.
pub fn stepped(volume: u8, up: bool, step: u8) -> u8 {
    if up {
        volume.saturating_add(step).min(100)
    } else {
        volume.saturating_sub(step)
    }
}

/// The file that plays color `id`.
pub fn wav(sounds: &str, id: &str) -> String {
    format!("{sounds}/noise_{id}.wav")
}

/// "Playing · Brown · 40%".
pub fn status(playing: bool, color: &str, volume: u8) -> String {
    let state = if playing { "Playing" } else { "Stopped" };
    format!("{state} · {} · {volume}%", label_of(color))
}

/// The tile's buttons as (id, label): the volume pair, then one per color
/// with a dot before the current one.
pub fn buttons(color: &str) -> Vec<(String, String)> {
    let mut out = vec![
        ("down".to_string(), "Volume −".to_string()),
        ("up".to_string(), "Volume +".to_string()),
    ];
    for (id, label) in COLORS {
        let dot = if *id == color { "● " } else { "" };
        out.push((id.to_string(), format!("{dot}{label}")));
    }
    out
}

/// The "playing" value kept in kv.
pub fn playing_flag(on: bool) -> &'static str {
    if on { "1" } else { "0" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_known_colors_and_passes_others_through() {
        assert_eq!(label_of("pink"), "Pink");
        assert_eq!(label_of("green"), "green");
    }

    #[test]
    fn reads_kept_values_with_defaults() {
        assert_eq!(color(None), "brown");
        assert_eq!(color(Some("blue".into())), "blue");
        assert_eq!(volume(None), 40);
        assert_eq!(volume(Some("")), 40);
        assert_eq!(volume(Some("junk")), 40);
        assert_eq!(volume(Some("10")), 10);
        assert_eq!(volume(Some("250")), 100);
    }

    #[test]
    fn steps_the_volume_within_bounds() {
        assert_eq!(stepped(40, true, 10), 50);
        assert_eq!(stepped(40, false, 10), 30);
        assert_eq!(stepped(95, true, 10), 100);
        assert_eq!(stepped(5, false, 10), 0);
        assert_eq!(stepped(250, true, 10), 100);
    }

    #[test]
    fn names_the_file_and_the_status() {
        assert_eq!(wav("~/n", "pink"), "~/n/noise_pink.wav");
        assert_eq!(status(true, "brown", 40), "Playing · Brown · 40%");
        assert_eq!(status(false, "violet", 0), "Stopped · Violet · 0%");
    }

    #[test]
    fn marks_the_current_color() {
        let b = buttons("white");
        assert_eq!(b.len(), 7);
        assert_eq!(b[0], ("down".into(), "Volume −".into()));
        assert_eq!(b[1], ("up".into(), "Volume +".into()));
        assert_eq!(b[2].1, "Brown");
        assert_eq!(b[4], ("white".into(), "● White".into()));
    }

    #[test]
    fn keeps_playing_as_one_or_zero() {
        assert_eq!(playing_flag(true), "1");
        assert_eq!(playing_flag(false), "0");
    }

    #[test]
    fn settings_fall_back_to_defaults() {
        let d = Settings::from_json("{}");
        assert_eq!(d.sounds, "~/.config/bricolo/sounds/noise");
        assert_eq!(d.step, 10);
        assert_eq!(Settings::from_json("junk"), d);
        let s = Settings::from_json(r#"{"sounds":"/tmp/n/","step":5}"#);
        assert_eq!(s.sounds, "/tmp/n");
        assert_eq!(s.step, 5);
        assert_eq!(Settings::from_json(r#"{"step":0}"#).step, 1);
    }
}
