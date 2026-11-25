//! The logic, with no capability calls and no generated bindings, so
//! `cargo test` runs it natively. serde_json stands in for the script's
//! lib/json.lua.

use serde_json::{Value, json};

/// The tile's buttons, as (id, label), in the order it shows them.
pub const BUTTONS: &[(&str, &str)] = &[
    ("region", "Read a screen region"),
    ("clipboard", "Read the clipboard image"),
    ("page", "Open the drop page"),
    ("unload", "Unload the model"),
];

/// The settings from `init`, with defaults for missing keys.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub chord: String,
    /// The sidecar, with no trailing slash.
    pub base: String,
    /// Where the region or the clipboard image goes for the sidecar to read.
    pub shot: String,
    pub poll_ms: u32,
    pub ocr_timeout_ms: u32,
    pub health_timeout_ms: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            chord: "ctrl+alt+shift+o".into(),
            base: "http://127.0.0.1:8797".into(),
            shot: "/tmp/bricolo-ocr-shot.png".into(),
            poll_ms: 15_000,
            ocr_timeout_ms: 300_000,
            health_timeout_ms: 3_000,
        }
    }
}

impl Settings {
    pub fn from_json(config: &str) -> Self {
        let d = Settings::default();
        let c = serde_json::from_str::<Value>(config).unwrap_or(Value::Null);
        let text = |key: &str, default: String| {
            c[key]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or(default)
        };
        let ms = |key: &str, default: u32| {
            c[key]
                .as_u64()
                .and_then(|ms| u32::try_from(ms).ok())
                .unwrap_or(default)
        };
        Settings {
            chord: text("chord", d.chord),
            base: text("base", d.base).trim_end_matches('/').to_string(),
            shot: text("shot", d.shot),
            poll_ms: ms("poll_ms", d.poll_ms).max(1_000),
            // http.send takes at most 300 s.
            ocr_timeout_ms: ms("ocr_timeout_ms", d.ocr_timeout_ms).clamp(1_000, 300_000),
            health_timeout_ms: ms("health_timeout_ms", d.health_timeout_ms).clamp(100, 300_000),
        }
    }
}

/// Lua truthiness: anything but a missing value, null or false.
fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false))
}

/// Lua's `tonumber`, truncated as `%d` prints it; 0 when it isn't a number.
fn count(v: &Value) -> i64 {
    let n = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    n.filter(|n| n.is_finite()).map_or(0, |n| n as i64)
}

/// "Model loaded · 3 pages read" from the body of /health, or None when
/// the server did not answer 200.
pub fn describe(health: Option<&str>) -> String {
    let health = health.and_then(|b| serde_json::from_str::<Value>(b).ok());
    let Some(h) = health.filter(|h| h.is_object() || h.is_array()) else {
        return "Server not answering".into();
    };
    if truthy(&h["busy"]) {
        return "Reading…".into();
    }
    let pages = count(&h["pages_done"]);
    format!(
        "{} · {pages} page{} read",
        if truthy(&h["loaded"]) {
            "Model loaded"
        } else {
            "Model idle"
        },
        if pages == 1 { "" } else { "s" },
    )
}

/// The JSON body of `POST /ocr` for the file at `path`.
pub fn ocr_body(path: &str) -> String {
    json!({ "path": path, "format": "markdown" }).to_string()
}

/// What to do with the answer of `POST /ocr`.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Put `text` on the clipboard.
    Copied {
        text: String,
    },
    NoText,
    /// A status other than 200.
    Refused {
        status: u16,
        body: String,
    },
}

pub fn outcome(status: u16, body: &[u8]) -> Outcome {
    let body = String::from_utf8_lossy(body);
    if status != 200 {
        return Outcome::Refused {
            status,
            body: body.into_owned(),
        };
    }
    // Lua's `%s+$`: ASCII white space only.
    let text = body.trim_end_matches(|c: char| c.is_ascii_whitespace());
    if text.is_empty() {
        Outcome::NoText
    } else {
        Outcome::Copied { text: text.into() }
    }
}

/// The toast after a copy, counting characters, not bytes.
pub fn copied_toast(text: &str) -> String {
    format!("Copied {} characters of Markdown", text.chars().count())
}

/// The log line after a copy, counting bytes as the script's `#text` did.
pub fn copied_log(text: &str, path: &str) -> String {
    format!("ocr: {} characters from {path}", text.len())
}

/// osascript arguments that write the clipboard's PNG to `shot` and print
/// "ok", or print "none" when the clipboard holds no image.
pub fn clipboard_script(shot: &str) -> Vec<String> {
    let lines = [
        "try".to_string(),
        "set png to the clipboard as «class PNGf»".into(),
        "on error".into(),
        "return \"none\"".into(),
        "end try".into(),
        format!("set f to open for access POSIX file \"{shot}\" with write permission"),
        "set eof f to 0".into(),
        "write png to f".into(),
        "close access f".into(),
        "return \"ok\"".into(),
    ];
    lines
        .into_iter()
        .flat_map(|l| ["-e".to_string(), l])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_the_health_answer() {
        assert_eq!(describe(None), "Server not answering");
        assert_eq!(describe(Some("not json")), "Server not answering");
        assert_eq!(describe(Some("42")), "Server not answering");
        assert_eq!(describe(Some(r#"{"busy":true,"loaded":true}"#)), "Reading…");
        assert_eq!(
            describe(Some(r#"{"loaded":true,"pages_done":3}"#)),
            "Model loaded · 3 pages read"
        );
        assert_eq!(
            describe(Some(r#"{"loaded":false,"pages_done":1,"busy":false}"#)),
            "Model idle · 1 page read"
        );
        assert_eq!(describe(Some("{}")), "Model idle · 0 pages read");
        assert_eq!(
            describe(Some(r#"{"loaded":1,"pages_done":"2"}"#)),
            "Model loaded · 2 pages read"
        );
    }

    #[test]
    fn builds_the_ocr_request() {
        let v: Value = serde_json::from_str(&ocr_body("/tmp/a \"b\".png")).unwrap();
        assert_eq!(v["path"], "/tmp/a \"b\".png");
        assert_eq!(v["format"], "markdown");
    }

    #[test]
    fn reads_the_ocr_answer() {
        assert_eq!(
            outcome(200, "# Title\n\nText \n\n".as_bytes()),
            Outcome::Copied {
                text: "# Title\n\nText".into()
            }
        );
        assert_eq!(outcome(200, b" \n\t"), Outcome::NoText);
        assert_eq!(
            outcome(500, b"model failed"),
            Outcome::Refused {
                status: 500,
                body: "model failed".into()
            }
        );
    }

    #[test]
    fn counts_characters_for_the_toast_and_bytes_for_the_log() {
        assert_eq!(copied_toast("é1"), "Copied 2 characters of Markdown");
        assert_eq!(
            copied_log("é1", "/tmp/x.png"),
            "ocr: 3 characters from /tmp/x.png"
        );
    }

    #[test]
    fn writes_the_clipboard_script() {
        let args = clipboard_script("/tmp/s.png");
        assert_eq!(args.len(), 20);
        assert!(args.iter().step_by(2).all(|a| a == "-e"));
        assert_eq!(args[1], "try");
        assert_eq!(
            args[11],
            "set f to open for access POSIX file \"/tmp/s.png\" with write permission"
        );
        assert_eq!(args[19], "return \"ok\"");
    }

    #[test]
    fn settings_fall_back_to_defaults() {
        assert_eq!(Settings::from_json("junk"), Settings::default());
        assert_eq!(Settings::from_json("{}"), Settings::default());
        let s = Settings::from_json(
            r#"{"chord":"cmd+o","base":"http://127.0.0.1:9000/","poll_ms":5,"ocr_timeout_ms":999999}"#,
        );
        assert_eq!(s.chord, "cmd+o");
        assert_eq!(s.base, "http://127.0.0.1:9000");
        assert_eq!(s.poll_ms, 1_000);
        assert_eq!(s.ocr_timeout_ms, 300_000);
        assert_eq!(s.shot, "/tmp/bricolo-ocr-shot.png");
    }
}
