//! The logic, with no capability calls and no generated bindings, so
//! `cargo test` runs it natively. It also holds the parts of the script's
//! lib/sidecar.lua that the tile uses: reading the sidecar's answer.

use serde_json::Value;

/// The hub refuses a status longer than 200 bytes.
pub const MAX_STATUS: usize = 200;

pub const TAILSCALE_APP: &str = "Tailscale";
pub const URL_DOWNLOAD: &str = "https://tailscale.com/download/macos";
pub const URL_DNS: &str = "https://login.tailscale.com/admin/dns";
pub const URL_ACL: &str = "https://login.tailscale.com/admin/acls/file";
pub const URL_MACHINES: &str = "https://login.tailscale.com/admin/machines";

/// After opening the Tailscale app, look again this much later.
pub const AFTER_APP_MS: u32 = 5000;

const SEP: &str = " · ";
const NOT_ANSWERING: &str = "sidecar not answering: ";

/// The settings from `init`, with defaults for missing keys.
#[derive(Debug, PartialEq)]
pub struct Settings {
    /// The sidecar's base URL, with no trailing slash.
    pub url: String,
    pub poll_ms: u32,
    /// Turning the funnel on or off can take a while.
    pub timeout_ms: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            url: "http://127.0.0.1:8795".into(),
            poll_ms: 30_000,
            timeout_ms: 30_000,
        }
    }
}

impl Settings {
    pub fn from_json(config: &str) -> Self {
        let d = Settings::default();
        let c = serde_json::from_str::<Value>(config).unwrap_or(Value::Null);
        let ms = |key: &str, default: u32| {
            c[key]
                .as_u64()
                .and_then(|ms| u32::try_from(ms).ok())
                .unwrap_or(default)
                .max(1_000)
        };
        Settings {
            url: c["url"]
                .as_str()
                .map(|u| u.trim_end_matches('/').to_string())
                .unwrap_or(d.url),
            poll_ms: ms("poll_ms", d.poll_ms),
            timeout_ms: ms("timeout_ms", d.timeout_ms),
        }
    }
}

/// The next setup step, as the sidecar describes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Step {
    pub label: String,
    pub button: Option<String>,
    pub action: String,
}

/// The sidecar's GET /view answer, the fields the tile reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct View {
    pub funnel: bool,
    pub status_line: Option<String>,
    pub url_line: Option<String>,
    pub lease_line: Option<String>,
    pub setup_line: Option<String>,
    pub next_step: Option<Step>,
    pub copyable: Option<String>,
    pub auth_url: Option<String>,
    pub policy_snippet: Option<String>,
    pub env_lines: Option<String>,
    pub webhook_url: Option<String>,
}

fn text(v: &Value, key: &str) -> Option<String> {
    v[key].as_str().map(str::to_string)
}

/// Lua's truth: anything but nil and false.
fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false))
}

impl View {
    pub fn from_value(v: &Value) -> Self {
        let step = &v["next_step"];
        View {
            funnel: truthy(&v["funnel"]),
            status_line: text(v, "status_line"),
            url_line: text(v, "url_line"),
            lease_line: text(v, "lease_line"),
            setup_line: text(v, "setup_line"),
            next_step: step.is_object().then(|| Step {
                label: text(step, "label").unwrap_or_default(),
                button: text(step, "button"),
                action: text(step, "action").unwrap_or_default(),
            }),
            copyable: text(v, "copyable"),
            auth_url: text(v, "auth_url"),
            policy_snippet: text(v, "policy_snippet"),
            env_lines: text(v, "env_lines"),
            webhook_url: text(v, "webhook_url"),
        }
    }
}

/// A call to the sidecar that brought no JSON: why, and the HTTP status,
/// which is none when it didn't answer or answered no JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub why: String,
    pub status: Option<u16>,
}

/// Why a reply that isn't a 200 failed. A JSON body's "error", else the
/// body, else the status.
fn reason(status: u16, body: &str) -> String {
    match serde_json::from_str::<Value>(body) {
        Ok(doc @ (Value::Object(_) | Value::Array(_))) => doc["error"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("HTTP {status}")),
        _ if !body.is_empty() => body.to_string(),
        _ => format!("HTTP {status}"),
    }
}

/// What http.send returned, as the reply's JSON, or why it failed.
/// `reply` is the status and body, or the transport error by name.
pub fn json_of(reply: Result<(u16, &[u8]), String>) -> Result<Value, Failure> {
    let (status, body) = reply.map_err(|err| Failure {
        why: format!("{NOT_ANSWERING}{err}"),
        status: None,
    })?;
    let body = String::from_utf8_lossy(body);
    let body = body.trim_end();
    if status != 200 {
        return Err(Failure {
            why: reason(status, body),
            status: Some(status),
        });
    }
    match serde_json::from_str::<Value>(body) {
        Ok(doc) if !doc.is_null() => Ok(doc),
        _ => Err(Failure {
            why: "sidecar answered no JSON".into(),
            status: None,
        }),
    }
}

/// Why the sidecar didn't answer, for the tile.
pub fn unreachable(why: &str) -> String {
    why.strip_prefix(NOT_ANSWERING).unwrap_or(why).to_string()
}

/// One part of the status line: a text, or versions of it, longest first.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    One(String),
    Versions(Vec<String>),
}

/// Joins the parts that fit, in order of importance, cutting the first one
/// short if it alone is too long. Cuts on a UTF-8 character boundary.
pub fn fit(parts: &[Part], max: usize) -> String {
    let mut out = String::new();
    for (i, part) in parts.iter().enumerate() {
        let part = match part {
            Part::One(s) => s.as_str(),
            Part::Versions(versions) => {
                let mut pick = None;
                for version in versions {
                    pick = Some(version.as_str());
                    if out.len() + SEP.len() + version.len() <= max {
                        break;
                    }
                }
                match pick {
                    Some(p) => p,
                    None => continue,
                }
            }
        };
        let next = if i == 0 {
            part.to_string()
        } else {
            format!("{out}{SEP}{part}")
        };
        if next.len() <= max {
            out = next;
        } else if i == 0 {
            let mut cut = max.saturating_sub("…".len());
            while cut > 0 && !part.is_char_boundary(cut) {
                cut -= 1;
            }
            out = format!("{}…", &part[..cut]);
        }
    }
    out
}

/// The tile's status: the state in words, the address, the leases and the
/// setup progress, on one line because the hub shows one.
pub fn status_text(
    view: Option<&View>,
    why: Option<&str>,
    doing: Option<&str>,
    base: &str,
) -> String {
    if let Some(doing) = doing {
        return doing.to_string();
    }
    let Some(v) = view else {
        let line = format!(
            "The tailscale-funnel sidecar isn't answering on {base}: {}",
            why.unwrap_or("no answer")
        );
        return fit(&[Part::One(line)], MAX_STATUS);
    };
    // Like the script's list, the parts end at the first missing one.
    let mut parts = Vec::new();
    for line in [&v.status_line, &v.url_line, &v.lease_line] {
        match line {
            Some(l) => parts.push(Part::One(l.clone())),
            None => return fit(&parts, MAX_STATUS),
        }
    }
    if let Some(setup) = &v.setup_line {
        let mut versions = vec![setup.clone()];
        if let Some(step) = &v.next_step {
            versions.insert(0, format!("{setup}; next: {}", step.label));
        }
        parts.push(Part::Versions(versions));
    }
    fit(&parts, MAX_STATUS)
}

pub const TOGGLE: &str = "toggle";
pub const CHECK: &str = "check";
pub const COPY: &str = "copy";
pub const STEP: &str = "step";
pub const APP: &str = "app";
pub const ADMIN: &str = "admin";

/// The next setup step as a button, unless the tile already has one for it.
fn step_button(view: Option<&View>) -> Option<(String, String)> {
    let v = view?;
    let step = v.next_step.as_ref()?;
    let label = step.button.as_ref()?;
    if step.action == "arm" || step.action == "check" {
        return None;
    }
    if step.action == "openapp" || (step.action == "signin" && v.auth_url.is_none()) {
        return None; // "Open the Tailscale app" is always there
    }
    Some((STEP.into(), label.clone()))
}

/// The tile's buttons, as (id, label).
pub fn buttons(view: Option<&View>) -> Vec<(String, String)> {
    let mut list = Vec::new();
    if let Some(v) = view {
        let label = if v.funnel {
            "Turn the funnel off"
        } else {
            "Turn the funnel on"
        };
        list.push((TOGGLE.into(), label.into()));
    }
    list.push((CHECK.into(), "Check now".into()));
    if view.is_some_and(|v| v.copyable.is_some()) {
        list.push((COPY.into(), "Copy URL".into()));
    }
    list.extend(step_button(view));
    list.push((APP.into(), "Open the Tailscale app".into()));
    list.push((ADMIN.into(), "Open the admin console".into()));
    list
}

/// One thing a button does.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    Open(String),
    OpenApp,
    /// The text to copy, if there is one, and what it is.
    Copy(Option<String>, &'static str),
}

/// What the next setup step's button does. Unknown actions do nothing.
pub fn step_acts(view: Option<&View>) -> Vec<Act> {
    let Some(v) = view else { return vec![] };
    let Some(step) = &v.next_step else {
        return vec![];
    };
    match step.action.as_str() {
        "install" => vec![Act::Open(URL_DOWNLOAD.into())],
        "openapp" => vec![Act::OpenApp],
        "signin" => match &v.auth_url {
            Some(url) => vec![Act::Open(url.clone())],
            None => vec![Act::OpenApp],
        },
        "admindns" => vec![Act::Open(URL_DNS.into())],
        "adminmachines" => vec![Act::Open(URL_MACHINES.into())],
        "adminacl" => vec![
            Act::Copy(v.policy_snippet.clone(), "the policy snippet"),
            Act::Open(URL_ACL.into()),
        ],
        "copyenv" => vec![Act::Copy(v.env_lines.clone(), "the voice agent's settings")],
        "copyhook" => vec![Act::Copy(v.webhook_url.clone(), "the webhook URL")],
        _ => vec![],
    }
}

/// What the tile says while switching the funnel.
pub fn busy_text(on: bool) -> &'static str {
    if on {
        "Turning the funnel on…"
    } else {
        "Turning the funnel off…"
    }
}

/// What to tell the user after a call to the sidecar.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// Log "what: err" as a warning and show it in a toast.
    Report(String, String),
    Toast(String),
    Info(String),
    Nothing,
}

/// "what: err", the line a report logs and shows.
pub fn report_line(what: &str, err: &str) -> String {
    format!("{what}: {err}")
}

/// After POST /arm (`on`) or /disarm.
pub fn toggled(on: bool, answer: &Result<Value, Failure>) -> Notice {
    match answer {
        Err(f) if f.status.is_none() => Notice::Report("Tailscale funnel".into(), f.why.clone()),
        Err(f) => {
            let what = if on {
                "The funnel stayed off"
            } else {
                "The funnel stayed on"
            };
            Notice::Report(what.into(), f.why.clone())
        }
        Ok(_) => Notice::Info(format!(
            "funnel turned {} from the tile",
            if on { "on" } else { "off" }
        )),
    }
}

/// After POST /check.
pub fn checked(answer: &Result<Value, Failure>) -> Notice {
    match answer {
        Err(f) if f.status.is_none() => Notice::Report("Tailscale funnel".into(), f.why.clone()),
        Err(f) => Notice::Toast(f.why.clone()),
        Ok(_) => Notice::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view(v: Value) -> View {
        View::from_value(&v)
    }

    fn one(s: &str) -> Part {
        Part::One(s.into())
    }

    #[test]
    fn fit_joins_what_fits_and_skips_the_rest() {
        assert_eq!(fit(&[one("a"), one("b")], 200), "a · b");
        // "a · b" is 6 bytes; "a · bbb" is 8.
        assert_eq!(fit(&[one("a"), one("bbb"), one("c")], 7), "a · c");
        assert_eq!(fit(&[], 10), "");
    }

    #[test]
    fn fit_cuts_the_first_part_on_a_char_boundary() {
        assert_eq!(fit(&[one("abcdefgh")], 6), "abc…");
        // "é" is two bytes; a cut inside it backs off.
        assert_eq!(fit(&[one("aéééé")], 7), "aé…");
        assert!(fit(&[one(&"x".repeat(300))], MAX_STATUS).len() <= MAX_STATUS);
    }

    #[test]
    fn fit_picks_the_longest_version_that_fits() {
        let versions = Part::Versions(vec!["long version".into(), "short".into()]);
        assert_eq!(fit(&[one("a"), versions.clone()], 200), "a · long version");
        assert_eq!(fit(&[one("a"), versions.clone()], 12), "a · short");
        // The last version is kept even when it doesn't fit, then skipped.
        assert_eq!(fit(&[one("a"), versions], 5), "a");
    }

    #[test]
    fn status_says_why_the_sidecar_is_silent() {
        let base = "http://127.0.0.1:8795";
        assert_eq!(
            status_text(None, Some("unavailable"), None, base),
            "The tailscale-funnel sidecar isn't answering on http://127.0.0.1:8795: unavailable"
        );
        assert!(status_text(None, None, None, base).ends_with(": no answer"));
        assert_eq!(status_text(None, None, Some("Turning…"), base), "Turning…");
    }

    #[test]
    fn status_joins_the_view_lines_and_the_next_step() {
        let v = view(json!({
            "status_line": "On", "url_line": "https://m.t.ts.net",
            "lease_line": "no leases", "setup_line": "5/6 done",
            "next_step": {"label": "Copy the hook", "action": "copyhook"},
        }));
        assert_eq!(
            status_text(Some(&v), None, None, ""),
            "On · https://m.t.ts.net · no leases · 5/6 done; next: Copy the hook"
        );
        let short = view(json!({"status_line": "Off", "lease_line": "x"}));
        assert_eq!(status_text(Some(&short), None, None, ""), "Off");
    }

    #[test]
    fn buttons_follow_the_view() {
        let ids = |v: Option<&View>| buttons(v).into_iter().map(|b| b.0).collect::<Vec<_>>();
        assert_eq!(ids(None), ["check", "app", "admin"]);
        let on = view(json!({"funnel": true, "copyable": "https://x",
            "next_step": {"button": "Copy", "action": "copyhook"}}));
        assert_eq!(
            ids(Some(&on)),
            ["toggle", "check", "copy", "step", "app", "admin"]
        );
        assert_eq!(buttons(Some(&on))[0].1, "Turn the funnel off");
        let off = view(json!({"funnel": false, "next_step": {"button": "Arm", "action": "arm"}}));
        assert_eq!(buttons(Some(&off))[0].1, "Turn the funnel on");
        assert_eq!(ids(Some(&off)), ["toggle", "check", "app", "admin"]);
    }

    #[test]
    fn sign_in_has_a_button_only_with_an_auth_url() {
        let ids = |v: &View| {
            buttons(Some(v))
                .into_iter()
                .map(|b| b.0)
                .collect::<Vec<_>>()
        };
        let without = view(json!({"next_step": {"button": "Sign in", "action": "signin"}}));
        assert!(!ids(&without).contains(&"step".to_string()));
        let with = view(json!({"auth_url": "https://login.tailscale.com/a/1",
            "next_step": {"button": "Sign in", "action": "signin"}}));
        assert!(ids(&with).contains(&"step".to_string()));
        assert_eq!(
            step_acts(Some(&with)),
            [Act::Open("https://login.tailscale.com/a/1".into())]
        );
        assert_eq!(step_acts(Some(&without)), [Act::OpenApp]);
    }

    #[test]
    fn steps_map_to_acts() {
        let acl = view(json!({"policy_snippet": "{}", "next_step": {"action": "adminacl"}}));
        assert_eq!(
            step_acts(Some(&acl)),
            [
                Act::Copy(Some("{}".into()), "the policy snippet"),
                Act::Open(URL_ACL.into())
            ]
        );
        let env = view(json!({"next_step": {"action": "copyenv"}}));
        assert_eq!(
            step_acts(Some(&env)),
            [Act::Copy(None, "the voice agent's settings")]
        );
        let odd = view(json!({"next_step": {"action": "dance"}}));
        assert!(step_acts(Some(&odd)).is_empty());
        assert!(step_acts(None).is_empty());
    }

    #[test]
    fn reads_the_sidecar_answer() {
        assert_eq!(json_of(Ok((200, br#"{"a":1}"#))).unwrap()["a"], 1);
        let silent = json_of(Err("unavailable".into())).unwrap_err();
        assert_eq!(silent.why, "sidecar not answering: unavailable");
        assert_eq!(silent.status, None);
        assert_eq!(unreachable(&silent.why), "unavailable");
        let no_json = json_of(Ok((200, b"hello\n"))).unwrap_err();
        assert_eq!(
            (no_json.why.as_str(), no_json.status),
            ("sidecar answered no JSON", None)
        );
        let refused = json_of(Ok((409, br#"{"error":"not signed in"}"#))).unwrap_err();
        assert_eq!(
            (refused.why.as_str(), refused.status),
            ("not signed in", Some(409))
        );
        assert_eq!(json_of(Ok((500, b"{}"))).unwrap_err().why, "HTTP 500");
        assert_eq!(
            json_of(Ok((502, b"bad gateway \n"))).unwrap_err().why,
            "bad gateway"
        );
        assert_eq!(json_of(Ok((404, b""))).unwrap_err().why, "HTTP 404");
    }

    #[test]
    fn notices_after_toggle_and_check() {
        let silent = Err(Failure {
            why: "x".into(),
            status: None,
        });
        let refused = Err(Failure {
            why: "no".into(),
            status: Some(409),
        });
        assert_eq!(
            toggled(true, &silent),
            Notice::Report("Tailscale funnel".into(), "x".into())
        );
        assert_eq!(
            toggled(true, &refused),
            Notice::Report("The funnel stayed off".into(), "no".into())
        );
        assert_eq!(
            toggled(false, &refused),
            Notice::Report("The funnel stayed on".into(), "no".into())
        );
        assert_eq!(
            toggled(false, &Ok(json!({}))),
            Notice::Info("funnel turned off from the tile".into())
        );
        assert_eq!(checked(&refused), Notice::Toast("no".into()));
        assert_eq!(checked(&Ok(json!({}))), Notice::Nothing);
        assert_eq!(report_line("a", "b"), "a: b");
    }

    #[test]
    fn settings_fall_back_to_defaults() {
        assert_eq!(Settings::from_json("{}"), Settings::default());
        assert_eq!(Settings::from_json("junk"), Settings::default());
        let s = Settings::from_json(
            r#"{"url":"http://127.0.0.1:9000/","poll_ms":5,"timeout_ms":60000}"#,
        );
        assert_eq!(s.url, "http://127.0.0.1:9000");
        assert_eq!(s.poll_ms, 1_000);
        assert_eq!(s.timeout_ms, 60_000);
    }
}
