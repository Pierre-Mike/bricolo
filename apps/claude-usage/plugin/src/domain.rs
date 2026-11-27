//! The logic, with no capability calls and no generated bindings, so
//! `cargo test` runs it natively.

use serde_json::Value;

const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;

/// The settings from `init`, with defaults for missing keys.
#[derive(Debug, PartialEq)]
pub struct Settings {
    pub every_ms: u32,
}

impl Settings {
    /// At least 30 s between calls: the endpoint is rate limited.
    pub fn from_json(config: &str) -> Self {
        let every_ms = serde_json::from_str::<Value>(config)
            .ok()
            .and_then(|c| c["every_ms"].as_u64())
            .and_then(|ms| u32::try_from(ms).ok())
            .unwrap_or(120_000)
            .max(30_000);
        Settings { every_ms }
    }
}

/// The access token in the Keychain item Claude Code writes:
/// `{"claudeAiOauth": {"accessToken": "..."}}`.
pub fn token(keychain: &str) -> Option<String> {
    let v: Value = serde_json::from_str(keychain.trim()).ok()?;
    let t = v["claudeAiOauth"]["accessToken"].as_str()?;
    (!t.is_empty()).then(|| t.to_string())
}

/// When the access token expires, in Unix seconds, from `expiresAt`
/// (milliseconds) in the same item.
pub fn expires_at(keychain: &str) -> Option<i64> {
    let v: Value = serde_json::from_str(keychain.trim()).ok()?;
    v["claudeAiOauth"]["expiresAt"].as_i64().map(|ms| ms / 1000)
}

/// One rate-limit window: percent used and when it resets, in Unix seconds.
/// `resets_at` is none while the window has not started.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub used: f64,
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    pub session: Option<Window>,
    pub week: Option<Window>,
}

/// The body of `GET /api/oauth/usage`.
pub fn usage(body: &str) -> Result<Usage, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    let window = |key: &str| {
        let w = &v[key];
        let used = w["utilization"].as_f64()?;
        Some(Window {
            used: used.clamp(0.0, 100.0),
            resets_at: w["resets_at"].as_str().and_then(rfc3339),
        })
    };
    Ok(Usage {
        session: window("five_hour"),
        week: window("seven_day"),
    })
}

/// Unix seconds from an RFC 3339 time such as `2026-11-26T17:40:00.123+00:00`
/// or `...Z`. Fractions are dropped.
pub fn rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, sec) = (n(11..13)?, n(14..16)?, n(17..19)?);
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        rest = frac.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let oh = rest.get(1..3)?.parse::<i64>().ok()?;
            let om = rest.get(4..6)?.parse::<i64>().ok()?;
            sign * (oh * HOUR + om * 60)
        }
    };
    Some(days_from_civil(y, mo, d) * DAY + h * HOUR + mi * 60 + sec - offset)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "in 2 h 10 min", "in 3 d 4 h", "in 12 min", "now".
pub fn until(secs: i64) -> String {
    if secs <= 0 {
        return "now".into();
    }
    let mins = (secs + 59) / 60;
    let (d, h, m) = (mins / 1440, mins % 1440 / 60, mins % 60);
    match (d, h) {
        (0, 0) => format!("in {m} min"),
        (0, _) if m == 0 => format!("in {h} h"),
        (0, _) => format!("in {h} h {m} min"),
        (_, 0) => format!("in {d} d"),
        _ => format!("in {d} d {h} h"),
    }
}

/// One bar of the widget, as `ui.set-gauge-widget` takes it.
#[derive(Debug, PartialEq)]
pub struct Gauge {
    pub label: String,
    pub value: String,
    pub fill: u8,
    pub marks: u8,
    pub now: Option<u8>,
    pub note: String,
}

/// The gauge for one window: filled to the percent used, one segment per
/// `unit`, a line at the share of the window gone, the time to the reset
/// under it. The hub turns the fill amber when it passes the line.
fn window_gauge(name: &str, w: &Window, unit: i64, units: u8, now: i64) -> Gauge {
    let length = unit * i64::from(units);
    let (gone, note) = match w.resets_at {
        Some(at) => {
            let left = (at - now).clamp(0, length);
            let pct = (100 * (length - left) + length / 2) / length;
            (Some(pct as u8), format!("Resets {}", until(at - now)))
        }
        None => (None, "Starts at your next message".into()),
    };
    Gauge {
        label: name.into(),
        value: format!("{:.0}%", w.used),
        fill: w.used.round().clamp(0.0, 100.0) as u8,
        marks: units,
        now: gone,
        note,
    }
}

/// The widget's gauges: the 5-hour session in hours, the week in days.
pub fn gauges(u: &Usage, now: i64) -> Vec<Gauge> {
    let mut out = Vec::new();
    if let Some(w) = &u.session {
        out.push(window_gauge("Session", w, HOUR, 5, now));
    }
    if let Some(w) = &u.week {
        out.push(window_gauge("Week", w, DAY, 7, now));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_and_floor() {
        assert_eq!(Settings::from_json("{}").every_ms, 120_000);
        assert_eq!(Settings::from_json(r#"{"every_ms":1000}"#).every_ms, 30_000);
        assert_eq!(
            Settings::from_json(r#"{"every_ms":60000}"#).every_ms,
            60_000
        );
    }

    #[test]
    fn reads_the_token() {
        let k = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-x","refreshToken":"r"}}"#;
        assert_eq!(token(k).as_deref(), Some("sk-ant-oat01-x"));
        assert_eq!(token("{}"), None);
        assert_eq!(token("junk"), None);
    }

    #[test]
    fn reads_the_expiry() {
        let k = r#"{"claudeAiOauth":{"accessToken":"t","expiresAt":9000}}"#;
        assert_eq!(expires_at(k), Some(9));
        assert_eq!(expires_at("{}"), None);
    }

    #[test]
    fn parses_times() {
        assert_eq!(rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339("2026-11-26T17:40:00Z"), Some(1_795_714_800));
        assert_eq!(
            rfc3339("2026-11-26T17:40:00.123456+00:00"),
            Some(1_795_714_800)
        );
        assert_eq!(rfc3339("2026-11-26T18:40:00+01:00"), Some(1_795_714_800));
        assert_eq!(rfc3339("yesterday"), None);
    }

    #[test]
    fn parses_usage() {
        let body = r#"{"five_hour":{"utilization":42.0,"resets_at":"2026-11-26T17:40:00Z"},
            "seven_day":{"utilization":18,"resets_at":null},"seven_day_opus":null}"#;
        let u = usage(body).unwrap();
        assert_eq!(
            u.session,
            Some(Window {
                used: 42.0,
                resets_at: Some(1_795_714_800)
            })
        );
        assert_eq!(
            u.week,
            Some(Window {
                used: 18.0,
                resets_at: None
            })
        );
        assert!(usage("<html>").is_err());
    }

    #[test]
    fn time_left() {
        assert_eq!(until(-5), "now");
        assert_eq!(until(30), "in 1 min");
        assert_eq!(until(2 * HOUR + 600), "in 2 h 10 min");
        assert_eq!(until(3 * HOUR), "in 3 h");
        assert_eq!(until(3 * DAY + 4 * HOUR), "in 3 d 4 h");
        assert_eq!(until(2 * DAY), "in 2 d");
    }

    #[test]
    fn gauges_mark_hours_and_days() {
        let now = 1_000_000;
        let u = Usage {
            session: Some(Window {
                used: 5.4,
                resets_at: Some(now + 2 * HOUR + 48 * 60),
            }),
            week: Some(Window {
                used: 25.0,
                resets_at: None,
            }),
        };
        let g = gauges(&u, now);
        assert_eq!(
            g[0],
            Gauge {
                label: "Session".into(),
                value: "5%".into(),
                fill: 5,
                marks: 5,
                // 2 h 12 min of 5 h gone.
                now: Some(44),
                note: "Resets in 2 h 48 min".into(),
            }
        );
        assert_eq!(g[1].marks, 7);
        assert_eq!(g[1].now, None);
        assert_eq!(g[1].note, "Starts at your next message");
        let none = Usage {
            session: None,
            week: None,
        };
        assert!(gauges(&none, now).is_empty());
    }

    #[test]
    fn a_reset_in_the_past_fills_the_window() {
        let w = Window {
            used: 100.0,
            resets_at: Some(10),
        };
        let g = window_gauge("Week", &w, DAY, 7, 1_000);
        assert_eq!((g.fill, g.now), (100, Some(100)));
        assert_eq!(g.note, "Resets now");
    }
}
