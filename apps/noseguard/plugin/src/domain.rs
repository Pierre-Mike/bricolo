//! The logic, with no capability calls and no generated bindings, so
//! `cargo test` runs it natively. It includes the parts of lib/sidecar.lua
//! the script used: reading a sidecar's reply and why it failed.

use serde_json::{Value, json};

/// A nose-zone radius in mm at each sensitivity, as in the Hammerspoon menu.
pub struct Zone {
    pub sens: f64,
    pub label: &'static str,
    pub mm: u32,
}

pub const ZONES: &[Zone] = &[
    Zone {
        sens: 35.0,
        label: "Tight",
        mm: 13,
    },
    Zone {
        sens: 55.0,
        label: "Medium",
        mm: 17,
    },
    Zone {
        sens: 75.0,
        label: "Loose",
        mm: 20,
    },
];

/// Seconds of contact before an alert. Hammerspoon kept 0.5 with no menu.
pub const HOLDS: &[f64] = &[0.25, 0.5, 1.0, 2.0];

/// The settings from `init`, with defaults for missing keys.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// The sidecar, without a trailing slash.
    pub url: String,
    pub poll_ms: u32,
    pub timeout_ms: u32,
    pub status_timeout_ms: u32,
    pub cameras_timeout_ms: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            url: "http://127.0.0.1:8802".into(),
            poll_ms: 2_000,
            timeout_ms: 5_000,
            status_timeout_ms: 2_000,
            cameras_timeout_ms: 30_000,
        }
    }
}

impl Settings {
    pub fn from_json(config: &str) -> Self {
        let d = Settings::default();
        let c = serde_json::from_str::<Value>(config).unwrap_or(Value::Null);
        let ms = |key: &str, default: u32, min: u32| {
            c[key]
                .as_u64()
                .and_then(|ms| u32::try_from(ms).ok())
                .unwrap_or(default)
                .max(min)
        };
        Settings {
            url: c["url"]
                .as_str()
                .map(|u| u.trim_end_matches('/').to_string())
                .filter(|u| !u.is_empty())
                .unwrap_or(d.url),
            poll_ms: ms("poll_ms", d.poll_ms, 100),
            timeout_ms: ms("timeout_ms", d.timeout_ms, 100),
            status_timeout_ms: ms("status_timeout_ms", d.status_timeout_ms, 100),
            cameras_timeout_ms: ms("cameras_timeout_ms", d.cameras_timeout_ms, 100),
        }
    }
}

/// The settings kept in kv, as the script kept them: every value a string.
#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    pub camera_id: String,
    pub camera_name: String,
    pub sensitivity: f64,
    pub hold: f64,
    pub debug: bool,
}

impl Prefs {
    /// From the kv values of camera_id, camera_name, sensitivity, hold and
    /// debug, each `None` when unset.
    pub fn read(
        camera_id: Option<String>,
        camera_name: Option<String>,
        sensitivity: Option<String>,
        hold: Option<String>,
        debug: Option<String>,
    ) -> Self {
        let num = |s: Option<String>| s.and_then(|s| s.trim().parse::<f64>().ok());
        Prefs {
            camera_id: camera_id.unwrap_or_default(),
            camera_name: camera_name.unwrap_or_default(),
            sensitivity: num(sensitivity).unwrap_or(55.0),
            hold: num(hold).unwrap_or(0.5),
            debug: debug.as_deref() == Some("1"),
        }
    }

    /// The body of POST /start.
    pub fn start_body(&self) -> String {
        json!({
            "camera_id": self.camera_id,
            "camera_name": self.camera_name,
            "sensitivity": number(self.sensitivity),
            "hold": number(self.hold),
            "debug": self.debug,
        })
        .to_string()
    }

    pub fn zone(&self) -> &'static Zone {
        &ZONES[zone_index(self.sensitivity)]
    }

    /// The kv value of sensitivity after the zone button: the next zone.
    pub fn next_sensitivity(&self) -> String {
        number_text(ZONES[(zone_index(self.sensitivity) + 1) % ZONES.len()].sens)
    }

    /// The kv value of hold after the hold button: the next hold.
    pub fn next_hold(&self) -> String {
        number_text(HOLDS[(hold_index(self.hold) + 1) % HOLDS.len()])
    }

    /// The kv value of debug after the detail log button.
    pub fn next_debug(&self) -> &'static str {
        if self.debug { "0" } else { "1" }
    }
}

/// The zone at `sens`, else Medium.
pub fn zone_index(sens: f64) -> usize {
    ZONES.iter().position(|z| z.sens == sens).unwrap_or(1)
}

/// The hold equal to `hold`, else 0.5 s.
pub fn hold_index(hold: f64) -> usize {
    HOLDS.iter().position(|h| *h == hold).unwrap_or(1)
}

/// `v` as Lua's json.encode wrote it: whole numbers without ".0".
fn number(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        json!(v as i64)
    } else {
        json!(v)
    }
}

/// `v` as Lua's "%g" wrote it for these values: 0.25, 0.5, 1, 2, 55.
fn number_text(v: f64) -> String {
    format!("{v}")
}

pub fn seconds(v: f64) -> String {
    format!("{} s", number_text(v))
}

pub fn touches(n: i64) -> String {
    format!("{n} touch{}", if n == 1 { "" } else { "es" })
}

pub fn camera_label(id: &str, name: &str) -> String {
    if id.is_empty() {
        "Auto (built-in)".into()
    } else if name.is_empty() {
        "Remembered camera".into()
    } else {
        name.into()
    }
}

/// The sidecar's /status (also the answer of /start, /stop and /reset).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Status {
    pub watching: bool,
    pub count: i64,
    pub last_error: Option<String>,
}

impl Status {
    pub fn from_value(v: &Value) -> Self {
        Status {
            watching: v["watching"].as_bool() == Some(true),
            count: v["count"].as_f64().map_or(0, |n| n as i64),
            last_error: v["last_error"].as_str().map(str::to_string),
        }
    }
}

/// The tile's status line: the state and count, or why the sidecar is down.
pub fn status_line(status: Option<&Status>, url: &str) -> String {
    let Some(st) = status else {
        let at = url.strip_prefix("http://").unwrap_or(url);
        return format!("Sidecar not answering on {at}");
    };
    let mut line = format!(
        "{} · {}",
        if st.watching { "Watching" } else { "Off" },
        touches(st.count)
    );
    if let (false, Some(e)) = (st.watching, &st.last_error) {
        line.push_str(" · ");
        line.push_str(e);
    }
    line
}

/// The tile as data, compared to redraw only on a change.
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub title: String,
    pub status: String,
    pub watching: bool,
    /// Ids and labels, in order.
    pub buttons: Vec<(&'static str, String)>,
}

pub fn tile(status: Option<&Status>, prefs: &Prefs, url: &str) -> Tile {
    let z = prefs.zone();
    Tile {
        title: "NoseGuard".into(),
        status: status_line(status, url),
        watching: status.is_some_and(|s| s.watching),
        buttons: vec![
            (
                "camera",
                format!(
                    "Camera: {}…",
                    camera_label(&prefs.camera_id, &prefs.camera_name)
                ),
            ),
            ("zone", format!("Nose zone: {} ({} mm)", z.label, z.mm)),
            ("hold", format!("Hold: {}", seconds(prefs.hold))),
            (
                "debug",
                format!("Detail log: {}", if prefs.debug { "on" } else { "off" }),
            ),
            ("flash", "Test flash".into()),
            ("reset", "Reset count".into()),
        ],
    }
}

/// One entry of the camera choice list: what it shows, and the camera id
/// and name it saves.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub label: String,
    pub id: String,
    pub name: String,
}

/// The choice list for the cameras in `cams` (from /cameras): Auto first,
/// then each camera, then the chosen one when it isn't connected.
pub fn camera_choices(cams: &Value, id: &str, name: &str) -> Vec<Choice> {
    let mark = |on: bool| if on { "● " } else { "" };
    let mut out = vec![Choice {
        label: format!("{}Auto (built-in)", mark(id.is_empty())),
        id: String::new(),
        name: String::new(),
    }];
    let mut listed = false;
    for c in cams.as_array().into_iter().flatten() {
        let cid = c["id"].as_str().unwrap_or_default();
        let cname = c["name"].as_str().unwrap_or_default();
        listed = listed || cid == id;
        let offline = if c["connected"] == Value::Bool(false) {
            " (offline)"
        } else {
            ""
        };
        out.push(Choice {
            label: format!("{}{cname}{offline}", mark(cid == id)),
            id: cid.into(),
            name: cname.into(),
        });
    }
    if !id.is_empty() && !listed {
        out.push(Choice {
            label: format!("● {} (not connected)", camera_label(id, name)),
            id: id.into(),
            name: name.into(),
        });
    }
    out
}

// From lib/sidecar.lua.

/// What a transport failure reads as, like the script's "sidecar not
/// answering: unavailable".
pub fn not_answering(err: &str) -> String {
    format!("sidecar not answering: {err}")
}

/// Why a reply that isn't a 200 failed: a JSON body's "error", else the
/// body, else the status.
fn reason(status: u16, body: &str) -> String {
    match serde_json::from_str::<Value>(body) {
        Ok(doc @ (Value::Object(_) | Value::Array(_))) => doc["error"]
            .as_str()
            .map_or_else(|| format!("HTTP {status}"), str::to_string),
        _ if !body.is_empty() => body.into(),
        _ => format!("HTTP {status}"),
    }
}

/// A reply's body without trailing space, or why it failed.
pub fn text_of(status: u16, body: &[u8]) -> Result<String, String> {
    let body = String::from_utf8_lossy(body).trim_end().to_string();
    if status != 200 {
        return Err(reason(status, &body));
    }
    Ok(body)
}

/// Like `text_of`, with the body decoded from JSON.
pub fn json_of(status: u16, body: &[u8]) -> Result<Value, String> {
    match serde_json::from_str::<Value>(&text_of(status, body)?) {
        Ok(Value::Null) | Err(_) => Err("sidecar answered no JSON".into()),
        Ok(doc) => Ok(doc),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs() -> Prefs {
        Prefs::read(None, None, None, None, None)
    }

    #[test]
    fn settings_fall_back_to_defaults() {
        assert_eq!(Settings::from_json("{}"), Settings::default());
        assert_eq!(Settings::from_json("junk"), Settings::default());
        let s = Settings::from_json(r#"{"url":"http://127.0.0.1:9000/","poll_ms":5}"#);
        assert_eq!(s.url, "http://127.0.0.1:9000");
        assert_eq!(s.poll_ms, 100);
    }

    #[test]
    fn reads_prefs_with_the_script_defaults() {
        let p = prefs();
        assert_eq!((p.sensitivity, p.hold, p.debug), (55.0, 0.5, false));
        assert_eq!(
            camera_label(&p.camera_id, &p.camera_name),
            "Auto (built-in)"
        );
        let p = Prefs::read(
            Some("abc".into()),
            Some("".into()),
            Some("junk".into()),
            Some("2".into()),
            Some("1".into()),
        );
        assert_eq!((p.sensitivity, p.hold, p.debug), (55.0, 2.0, true));
        assert_eq!(camera_label("abc", ""), "Remembered camera");
        assert_eq!(camera_label("abc", "FaceTime"), "FaceTime");
    }

    #[test]
    fn buttons_cycle_and_write_values_as_the_script_did() {
        let mut p = prefs();
        assert_eq!(p.next_sensitivity(), "75");
        p.sensitivity = 75.0;
        assert_eq!(p.next_sensitivity(), "35");
        p.sensitivity = 42.0;
        assert_eq!(p.zone().label, "Medium");
        assert_eq!(p.next_hold(), "1");
        p.hold = 2.0;
        assert_eq!(p.next_hold(), "0.25");
        p.hold = 0.25;
        assert_eq!(p.next_hold(), "0.5");
        assert_eq!(p.next_debug(), "1");
    }

    #[test]
    fn start_body_writes_whole_numbers_without_a_point() {
        let mut p = prefs();
        p.hold = 1.0;
        let body: Value = serde_json::from_str(&p.start_body()).unwrap();
        assert_eq!(
            body,
            json!({"camera_id":"","camera_name":"","sensitivity":55,"hold":1,"debug":false})
        );
        assert!(p.start_body().contains("\"sensitivity\":55"));
        assert!(!p.start_body().contains(".0"));
        p.hold = 0.25;
        assert!(p.start_body().contains("\"hold\":0.25"));
    }

    #[test]
    fn status_line_shows_state_count_and_error() {
        let url = "http://127.0.0.1:8802";
        assert_eq!(
            status_line(None, url),
            "Sidecar not answering on 127.0.0.1:8802"
        );
        let st = Status::from_value(&json!({"watching":true,"count":1,"last_error":"x"}));
        assert_eq!(status_line(Some(&st), url), "Watching · 1 touch");
        let st = Status::from_value(&json!({"watching":false,"count":3,"last_error":"no camera"}));
        assert_eq!(status_line(Some(&st), url), "Off · 3 touches · no camera");
        let st = Status::from_value(&json!({"last_error":null}));
        assert_eq!(status_line(Some(&st), url), "Off · 0 touches");
    }

    #[test]
    fn tile_labels_match_the_script() {
        let st = Status {
            watching: true,
            ..Status::default()
        };
        let t = tile(Some(&st), &prefs(), "http://127.0.0.1:8802");
        assert_eq!(t.title, "NoseGuard");
        assert!(t.watching);
        let labels: Vec<_> = t.buttons.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Camera: Auto (built-in)…",
                "Nose zone: Medium (17 mm)",
                "Hold: 0.5 s",
                "Detail log: off",
                "Test flash",
                "Reset count",
            ]
        );
        assert!(!tile(None, &prefs(), "").watching);
    }

    #[test]
    fn camera_choices_mark_the_pick_and_keep_a_missing_one() {
        let cams = json!([
            {"id":"a","name":"FaceTime"},
            {"id":"b","name":"Desk","connected":false},
        ]);
        let labels = |c: Vec<Choice>| c.into_iter().map(|c| c.label).collect::<Vec<_>>();
        assert_eq!(
            labels(camera_choices(&cams, "", "")),
            ["● Auto (built-in)", "FaceTime", "Desk (offline)"]
        );
        assert_eq!(
            labels(camera_choices(&cams, "b", "Desk")),
            ["Auto (built-in)", "FaceTime", "● Desk (offline)"]
        );
        let c = camera_choices(&cams, "z", "Old");
        assert_eq!(c[3].label, "● Old (not connected)");
        assert_eq!((c[3].id.as_str(), c[3].name.as_str()), ("z", "Old"));
        assert_eq!(camera_choices(&json!({}), "", "").len(), 1);
    }

    #[test]
    fn reads_replies_like_lib_sidecar() {
        assert_eq!(text_of(200, b"ok \n"), Ok("ok".into()));
        assert_eq!(json_of(200, br#"{"count":2}"#).unwrap()["count"], json!(2));
        assert_eq!(json_of(200, b"hi"), Err("sidecar answered no JSON".into()));
        assert_eq!(
            json_of(200, b"null"),
            Err("sidecar answered no JSON".into())
        );
        assert_eq!(json_of(500, br#"{"error":"busy"}"#), Err("busy".into()));
        assert_eq!(json_of(500, br#"{"oops":1}"#), Err("HTTP 500".into()));
        assert_eq!(json_of(404, b"not found\n"), Err("not found".into()));
        assert_eq!(json_of(502, b""), Err("HTTP 502".into()));
        assert_eq!(
            not_answering("unavailable"),
            "sidecar not answering: unavailable"
        );
    }
}
