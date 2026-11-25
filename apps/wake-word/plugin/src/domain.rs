//! The logic, with no capability calls and no generated bindings, so
//! `cargo test` runs it natively.

use serde_json::Value;

/// The settings from `init`, with defaults for missing keys.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The sidecar, without a trailing slash.
    pub url: String,
    pub poll_ms: u32,
    pub timeout_ms: u32,
    pub status_timeout_ms: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            url: "http://127.0.0.1:8803".into(),
            poll_ms: 5_000,
            timeout_ms: 5_000,
            status_timeout_ms: 3_000,
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
        }
    }
}

/// Whether the kv value of "enabled" turns listening on: only "on" does.
pub fn enabled(kv: Option<&str>) -> bool {
    kv == Some("on")
}

/// What the switch saves under "enabled".
pub fn kv_value(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// A status field as Lua would concatenate it: strings and numbers.
fn field(s: &Value, key: &str) -> Option<String> {
    match &s[key] {
        Value::String(t) => Some(t.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Whether a reply is what Lua calls a table.
fn is_table(s: &Value) -> bool {
    s.is_object() || s.is_array()
}

/// The tile's line from the sidecar's /status, or `None` when it didn't
/// answer.
pub fn describe(s: Option<&Value>) -> String {
    let Some(s) = s.filter(|s| is_table(s)) else {
        return "Wake word sidecar not answering".into();
    };
    let mut line = format!(
        "Wake word \"Codec\": {}",
        field(s, "why").unwrap_or_else(|| "?".into())
    );
    if let Some(heard) = field(s, "last_heard") {
        line.push_str(&format!(" · last heard: {heard}"));
    }
    line
}

/// The path to POST so the sidecar matches the switch, or `None` when it
/// already does or didn't answer. After a restart the sidecar comes back
/// disabled.
pub fn sync_path(s: Option<&Value>, enabled: bool) -> Option<&'static str> {
    let s = s.filter(|s| is_table(s))?;
    if s["enabled"].as_bool() == Some(enabled) {
        return None;
    }
    Some(if enabled { "/enable" } else { "/disable" })
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
    use serde_json::json;

    #[test]
    fn only_on_is_enabled() {
        assert!(enabled(Some("on")));
        assert!(!enabled(Some("off")));
        assert!(!enabled(None));
        assert_eq!(kv_value(true), "on");
        assert_eq!(kv_value(false), "off");
    }

    #[test]
    fn describes_the_status() {
        assert_eq!(describe(None), "Wake word sidecar not answering");
        assert_eq!(
            describe(Some(&json!("hi"))),
            "Wake word sidecar not answering"
        );
        assert_eq!(
            describe(Some(&json!({"why": "listening"}))),
            "Wake word \"Codec\": listening"
        );
        assert_eq!(
            describe(Some(&json!({"why": "listening", "last_heard": "12:04"}))),
            "Wake word \"Codec\": listening · last heard: 12:04"
        );
        assert_eq!(
            describe(Some(&json!({"last_heard": null}))),
            "Wake word \"Codec\": ?"
        );
    }

    #[test]
    fn syncs_only_a_sidecar_that_disagrees() {
        let off = json!({"enabled": false});
        assert_eq!(sync_path(Some(&off), true), Some("/enable"));
        assert_eq!(sync_path(Some(&off), false), None);
        let on = json!({"enabled": true});
        assert_eq!(sync_path(Some(&on), false), Some("/disable"));
        assert_eq!(sync_path(Some(&json!({})), false), Some("/disable"));
        assert_eq!(sync_path(None, true), None);
    }

    #[test]
    fn reads_sidecar_replies() {
        assert_eq!(text_of(200, b"ok \n"), Ok("ok".into()));
        assert_eq!(json_of(200, br#"{"why":"x"}"#).unwrap()["why"], json!("x"));
        assert_eq!(json_of(200, b"hi"), Err("sidecar answered no JSON".into()));
        assert_eq!(
            json_of(200, b"null"),
            Err("sidecar answered no JSON".into())
        );
        assert_eq!(json_of(500, br#"{"error":"busy"}"#), Err("busy".into()));
        assert_eq!(text_of(503, b"down"), Err("down".into()));
        assert_eq!(text_of(404, b""), Err("HTTP 404".into()));
        assert_eq!(not_answering("timeout"), "sidecar not answering: timeout");
    }

    #[test]
    fn settings_fall_back_to_defaults() {
        assert_eq!(Settings::from_json("junk"), Settings::default());
        let s = Settings::from_json(r#"{"url":"http://127.0.0.1:9000/","poll_ms":5}"#);
        assert_eq!(s.url, "http://127.0.0.1:9000");
        assert_eq!(s.poll_ms, 100);
        assert_eq!(s.status_timeout_ms, 3_000);
    }
}
