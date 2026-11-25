//! The logic, with no capability calls and no generated bindings, so
//! `cargo test` runs it natively. `serde_json` stands in for the script's
//! lib/json.lua.

use serde_json::Value;

/// The settings from `init`, with defaults for missing keys.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub chord: String,
    pub project: String,
    pub uv: String,
    pub user: String,
    pub base_url: String,
    pub pocket_tts_url: String,
    pub daemon_log: String,
    pub retro_log: String,
    /// "HH:MM", compared as text with the local time.
    pub retro_at: String,
    pub startup_grace_s: u64,
    pub relaunches: u32,
    /// How many of the latest turns the speed widget summarises.
    pub speed_turns: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            chord: "ctrl+alt+cmd+v".into(),
            project: "/Users/me/Github/pipecat-voice-agent".into(),
            uv: "/Users/me/.local/bin/uv".into(),
            user: "me".into(),
            base_url: "http://127.0.0.1:8792".into(),
            pocket_tts_url: "http://127.0.0.1:8791".into(),
            daemon_log: "/tmp/hs-voice-agent-daemon.log".into(),
            retro_log: "/tmp/hs-voice-agent-retro.log".into(),
            retro_at: "18:30".into(),
            startup_grace_s: 90,
            relaunches: 3,
            speed_turns: 50,
        }
    }
}

impl Settings {
    pub fn from_json(config: &str) -> Self {
        let c = serde_json::from_str::<Value>(config).unwrap_or(Value::Null);
        let d = Settings::default();
        let text = |key: &str, default: String| {
            c[key]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(String::from)
                .unwrap_or(default)
        };
        Settings {
            chord: text("chord", d.chord),
            project: text("project", d.project),
            uv: text("uv", d.uv),
            user: text("user", d.user),
            base_url: text("base_url", d.base_url),
            pocket_tts_url: text("pocket_tts_url", d.pocket_tts_url),
            daemon_log: text("daemon_log", d.daemon_log),
            retro_log: text("retro_log", d.retro_log),
            retro_at: text("retro_at", d.retro_at),
            startup_grace_s: c["startup_grace_s"].as_u64().unwrap_or(d.startup_grace_s),
            relaunches: c["relaunches"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(d.relaunches),
            speed_turns: c["speed_turns"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n > 0)
                .unwrap_or(d.speed_turns),
        }
    }

    /// Where the daemon writes latency-YYYY-MM-DD.jsonl (LATENCY_LOG=1).
    pub fn latency_dir(&self) -> String {
        format!("{}/data/latency", self.project)
    }

    /// The venv's python, which runs the daemon.
    pub fn python(&self) -> String {
        format!("{}/.venv/bin/python", self.project)
    }

    /// Where tools/retro.py writes the daily reviews.
    pub fn retro_dir(&self) -> String {
        format!("{}/data/retros", self.project)
    }

    /// The daemon's port, for lsof.
    pub fn port(&self) -> Option<u16> {
        port_of(&self.base_url)
    }
}

/// The port in "http://127.0.0.1:8792[/...]".
pub fn port_of(url: &str) -> Option<u16> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split('/').next()?;
    host.rsplit_once(':')?.1.parse().ok()
}

/// What the tile shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Live,
    Down,
    AgentDown,
}

impl State {
    /// The names the script logged.
    pub fn name(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Live => "live",
            State::Down => "down",
            State::AgentDown => "agent_down",
        }
    }

    fn description(self) -> &'static str {
        match self {
            State::Idle => "Ready",
            State::Live => "Listening",
            State::Down => "Daemon not responding",
            State::AgentDown => "The agent's Claude Code process is down",
        }
    }
}

/// The parts of the daemon's `/status` the tile uses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub running: bool,
    pub agent: Option<String>,
    pub channel: Option<String>,
    pub held: bool,
    pub voice: Option<String>,
    pub open: i64,
    pub needs_you: i64,
}

/// Lua's `tonumber`: a number or a numeric string, else 0.
fn number(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0) as i64,
        Value::String(s) => s.trim().parse::<f64>().map_or(0, |f| f as i64),
        _ => 0,
    }
}

/// The status from a 200 `/status` body, or None when the body is not an
/// object with a boolean `running` (the daemon is then unreachable).
pub fn parse_status(body: &[u8]) -> Option<Status> {
    let doc: Value = serde_json::from_slice(body).ok()?;
    let running = doc.get("running")?.as_bool()?;
    let text = |key: &str| doc.get(key).and_then(Value::as_str).map(String::from);
    let tasks = doc.get("tasks").cloned().unwrap_or(Value::Null);
    Some(Status {
        running,
        agent: text("agent"),
        channel: text("channel"),
        held: !matches!(
            doc.get("held"),
            None | Some(Value::Null | Value::Bool(false))
        ),
        voice: text("voice"),
        open: number(&tasks["open"]),
        needs_you: number(&tasks["needs_you"]),
    })
}

pub fn state_of(status: Option<&Status>) -> State {
    match status {
        None => State::Down,
        Some(s) if s.running => State::Live,
        Some(s) if s.agent.as_deref() == Some("down") => State::AgentDown,
        Some(_) => State::Idle,
    }
}

/// The tile's status line: "Listening · 2 jobs open, 1 needs you".
pub fn describe(state: State, status: Option<&Status>) -> String {
    let mut line = state.description().to_string();
    if let (State::Live, Some(s)) = (state, status) {
        if s.channel.as_deref() == Some("phone") {
            line = "On the phone".into();
        } else if s.held {
            line = "On hold".into();
        }
    }
    let (open, needs_you) = status.map_or((0, 0), |s| (s.open, s.needs_you));
    if open > 0 {
        line += &format!(" · {open} job{} open", if open == 1 { "" } else { "s" });
        if needs_you > 0 {
            line += &format!(
                ", {needs_you} need{} you",
                if needs_you == 1 { "s" } else { "" }
            );
        }
    }
    line
}

/// "peter_yearsley" -> "Peter Yearsley"; "" is the automatic choice.
pub fn voice_label(v: &str) -> String {
    if v.is_empty() {
        return "Automatic (by language)".into();
    }
    // Lua's `(%a[%w']*)`: a word starts at an ASCII letter and runs over
    // ASCII letters, digits and apostrophes.
    let mut out = String::with_capacity(v.len());
    let mut in_word = false;
    for c in v.chars() {
        let c = if c == '_' || c == '-' { ' ' } else { c };
        if in_word && (c.is_ascii_alphanumeric() || c == '\'') {
            out.push(c);
        } else if c.is_ascii_alphabetic() {
            in_word = true;
            out.push(c.to_ascii_uppercase());
        } else {
            in_word = false;
            out.push(c);
        }
    }
    out
}

/// The voice the daemon reports, else the one last picked.
pub fn current_voice(status: Option<&Status>, saved: Option<String>) -> String {
    status
        .and_then(|s| s.voice.clone())
        .or(saved)
        .unwrap_or_default()
}

/// The voices in a pocket-tts `/voices` body, or None without a list.
pub fn parse_voices(body: &[u8]) -> Option<Vec<String>> {
    let doc: Value = serde_json::from_slice(body).ok()?;
    match doc.get("voices")? {
        Value::Array(a) => Some(
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect(),
        ),
        // A Lua table with no array part: the script listed no voices.
        Value::Object(_) => Some(Vec::new()),
        _ => None,
    }
}

/// The choice list's values and labels: the automatic voice first, and a
/// dot before the current one.
pub fn voice_choices(voices: &[String], current: &str) -> (Vec<String>, Vec<String>) {
    let values: Vec<String> = std::iter::once(String::new())
        .chain(voices.iter().cloned())
        .collect();
    let labels = values
        .iter()
        .map(|v| {
            let dot = if v == current { "● " } else { "" };
            format!("{dot}{}", voice_label(v))
        })
        .collect();
    (values, labels)
}

/// The body of `POST /voice`.
pub fn voice_body(voice: &str) -> String {
    serde_json::json!({ "voice": voice }).to_string()
}

/// The newest "YYYY-MM-DD[-N].md" in a list of names; "DAY.md" counts as
/// N = 1, and the first of equals wins.
pub fn latest_review(names: &[String]) -> Option<&str> {
    let mut best: Option<(&str, &str, u64)> = None;
    for name in names {
        let Some((day, n)) = review_key(name) else {
            continue;
        };
        if best.is_none_or(|(_, d, bn)| day > d || (day == d && n > bn)) {
            best = Some((name, day, n));
        }
    }
    best.map(|(name, _, _)| name)
}

fn review_key(name: &str) -> Option<(&str, u64)> {
    let stem = name.strip_suffix(".md")?;
    let day = stem.get(..10)?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let b = day.as_bytes();
    if !(digits(&day[..4])
        && b[4] == b'-'
        && digits(&day[5..7])
        && b[7] == b'-'
        && digits(&day[8..]))
    {
        return None;
    }
    match &stem[10..] {
        "" => Some((day, 1)),
        rest => {
            let n = rest.strip_prefix('-').filter(|n| digits(n))?;
            Some((day, n.parse().ok()?))
        }
    }
}

/// Once a day at `retro_at`, or at the first check after it. `now` is
/// "YYYY-MM-DD HH:MM", local time.
pub fn review_due(now: &str, retro_at: &str, last_review: Option<&str>) -> bool {
    let Some((day, hm)) = now.trim().split_once(' ') else {
        return false;
    };
    hm >= retro_at && last_review != Some(day)
}

/// The day in "YYYY-MM-DD HH:MM".
pub fn day_of(now: &str) -> Option<&str> {
    now.trim().split_once(' ').map(|(day, _)| day)
}

/// `s` quoted for sh.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The sh line that starts the daemon detached, so a reload or a node
/// restart never ends a call.
pub fn daemon_command(s: &Settings) -> String {
    format!(
        "cd {} && nohup {} -m voice_agent.daemon >>{} 2>&1 </dev/null &",
        quote(&s.project),
        quote(&s.python()),
        quote(&s.daemon_log)
    )
}

/// The sh line that starts the daily review detached.
pub fn review_command(s: &Settings) -> String {
    format!(
        "cd {} && nohup {} run python tools/retro.py >>{} 2>&1 </dev/null &",
        quote(&s.project),
        quote(&s.uv),
        quote(&s.retro_log)
    )
}

/// The pids in `lsof -t` output.
pub fn pids(stdout: &str) -> Vec<String> {
    stdout
        .split_whitespace()
        .filter(|p| p.bytes().all(|b| b.is_ascii_digit()))
        .map(String::from)
        .collect()
}

/// The daemon as the plugin last saw it, and its relaunch budget.
#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    pub state: State,
    pub status: Option<Status>,
    /// Seconds (any clock) of our last launch.
    pub launched_at: Option<u64>,
    pub relaunches: u32,
}

impl Default for Agent {
    fn default() -> Self {
        Agent {
            state: State::Down,
            status: None,
            launched_at: None,
            relaunches: 0,
        }
    }
}

/// What one poll found.
#[derive(Debug, PartialEq)]
pub struct Observed {
    /// The old and new state, when it changed.
    pub changed: Option<(State, State)>,
    /// Relaunch number n, when the daemon stayed down past the grace time.
    pub relaunch: Option<u32>,
}

impl Agent {
    pub fn observe(&mut self, status: Option<Status>, now: u64, s: &Settings) -> Observed {
        let next = state_of(status.as_ref());
        let changed = (next != self.state).then_some((self.state, next));
        let reachable = status.is_some();
        self.state = next;
        self.status = status;
        let mut relaunch = None;
        if reachable {
            self.relaunches = 0;
        } else if self
            .launched_at
            .is_some_and(|at| now.saturating_sub(at) >= s.startup_grace_s)
            && self.relaunches < s.relaunches
        {
            self.relaunches += 1;
            relaunch = Some(self.relaunches);
        }
        Observed { changed, relaunch }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(json: &str) -> Option<Status> {
        parse_status(json.as_bytes())
    }

    #[test]
    fn settings_fall_back_to_defaults() {
        assert_eq!(Settings::from_json("{}"), Settings::default());
        assert_eq!(Settings::from_json("junk"), Settings::default());
        let s = Settings::from_json(r#"{"chord":"cmd+v","relaunches":1,"project":"/p"}"#);
        assert_eq!(s.chord, "cmd+v");
        assert_eq!(s.relaunches, 1);
        assert_eq!(s.python(), "/p/.venv/bin/python");
        assert_eq!(s.retro_dir(), "/p/data/retros");
        assert_eq!(Settings::default().port(), Some(8792));
    }

    #[test]
    fn finds_the_port() {
        assert_eq!(port_of("http://127.0.0.1:8792"), Some(8792));
        assert_eq!(port_of("http://127.0.0.1:8791/voices"), Some(8791));
        assert_eq!(port_of("http://example.com/"), None);
    }

    #[test]
    fn reads_the_state() {
        assert_eq!(state_of(None), State::Down);
        assert_eq!(status(r#"{"running":"yes"}"#), None);
        assert_eq!(status("[]"), None);
        assert_eq!(
            state_of(status(r#"{"running":true}"#).as_ref()),
            State::Live
        );
        assert_eq!(
            state_of(status(r#"{"running":false,"agent":"down"}"#).as_ref()),
            State::AgentDown
        );
        assert_eq!(
            state_of(status(r#"{"running":false,"agent":"up"}"#).as_ref()),
            State::Idle
        );
    }

    #[test]
    fn describes_the_state_and_jobs() {
        let live = status(r#"{"running":true,"channel":"phone","held":true}"#);
        assert_eq!(describe(State::Live, live.as_ref()), "On the phone");
        let held = status(r#"{"running":true,"held":1}"#);
        assert_eq!(describe(State::Live, held.as_ref()), "On hold");
        let not_held = status(r#"{"running":true,"held":false}"#);
        assert_eq!(describe(State::Live, not_held.as_ref()), "Listening");
        let jobs = status(r#"{"running":false,"tasks":{"open":2,"needs_you":1}}"#);
        assert_eq!(
            describe(State::Idle, jobs.as_ref()),
            "Ready · 2 jobs open, 1 needs you"
        );
        let one = status(r#"{"running":false,"tasks":{"open":"1","needs_you":2}}"#);
        assert_eq!(
            describe(State::Idle, one.as_ref()),
            "Ready · 1 job open, 2 need you"
        );
        assert_eq!(describe(State::Down, None), "Daemon not responding");
        assert_eq!(
            describe(State::AgentDown, None),
            "The agent's Claude Code process is down"
        );
    }

    #[test]
    fn labels_voices() {
        assert_eq!(voice_label("peter_yearsley"), "Peter Yearsley");
        assert_eq!(voice_label("jean-paul"), "Jean Paul");
        assert_eq!(voice_label("o'brien_2b"), "O'brien 2B");
        assert_eq!(voice_label("alba"), "Alba");
        assert_eq!(voice_label(""), "Automatic (by language)");
    }

    #[test]
    fn lists_voice_choices_with_the_current_one_marked() {
        let voices = parse_voices(br#"{"voices":["alba","peter_yearsley",3]}"#).unwrap();
        let (values, labels) = voice_choices(&voices, "alba");
        assert_eq!(values, ["", "alba", "peter_yearsley"]);
        assert_eq!(
            labels,
            ["Automatic (by language)", "● Alba", "Peter Yearsley"]
        );
        let (_, labels) = voice_choices(&[], "");
        assert_eq!(labels, ["● Automatic (by language)"]);
        assert_eq!(parse_voices(b"{}"), None);
        assert_eq!(parse_voices(br#"{"voices":"alba"}"#), None);
        assert_eq!(parse_voices(br#"{"voices":{}}"#), Some(vec![]));
    }

    #[test]
    fn prefers_the_daemons_voice() {
        let s = status(r#"{"running":false,"voice":"alba"}"#);
        assert_eq!(current_voice(s.as_ref(), Some("eve".into())), "alba");
        let s = status(r#"{"running":false}"#);
        assert_eq!(current_voice(s.as_ref(), Some("eve".into())), "eve");
        assert_eq!(current_voice(None, None), "");
        assert_eq!(voice_body("alba"), r#"{"voice":"alba"}"#);
        assert_eq!(voice_body(""), r#"{"voice":""}"#);
    }

    #[test]
    fn picks_the_latest_review() {
        let names: Vec<String> = [
            "2026-11-09.md",
            "2026-11-17-2.md",
            "2026-11-17.md",
            "notes.md",
            "2026-11-18.txt",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(latest_review(&names), Some("2026-11-17-2.md"));
        let names = ["2026-11-17-10.md", "2026-11-17-9.md"].map(String::from);
        assert_eq!(latest_review(&names), Some("2026-11-17-10.md"));
        let names = ["2026-11-17.md", "2026-11-17-1.md"].map(String::from);
        assert_eq!(latest_review(&names), Some("2026-11-17.md"));
        let names = ["2026-1-17.md", "2026-11-17-.md", "x2026-11-17.md"].map(String::from);
        assert_eq!(latest_review(&names), None);
        assert_eq!(latest_review(&[]), None);
    }

    #[test]
    fn runs_the_review_once_a_day_after_its_time() {
        assert!(!review_due("2026-11-17 18:29", "18:30", None));
        assert!(review_due("2026-11-17 18:30", "18:30", None));
        assert!(review_due(
            "2026-11-17 23:00\n",
            "18:30",
            Some("2026-11-16")
        ));
        assert!(!review_due("2026-11-17 23:00", "18:30", Some("2026-11-17")));
        assert!(!review_due("", "18:30", None));
        assert_eq!(day_of("2026-11-17 18:30\n"), Some("2026-11-17"));
    }

    #[test]
    fn builds_the_detached_commands() {
        let s = Settings::default();
        assert_eq!(
            daemon_command(&s),
            "cd '/Users/me/Github/pipecat-voice-agent' && nohup \
             '/Users/me/Github/pipecat-voice-agent/.venv/bin/python' \
             -m voice_agent.daemon >>'/tmp/hs-voice-agent-daemon.log' 2>&1 </dev/null &"
        );
        assert_eq!(
            review_command(&s),
            "cd '/Users/me/Github/pipecat-voice-agent' && nohup \
             '/Users/me/.local/bin/uv' run python tools/retro.py \
             >>'/tmp/hs-voice-agent-retro.log' 2>&1 </dev/null &"
        );
        assert_eq!(quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn reads_pids() {
        assert_eq!(pids("123\n456\n"), ["123", "456"]);
        assert!(pids("").is_empty());
        assert!(pids("lsof: oops").is_empty());
    }

    #[test]
    fn relaunches_after_the_grace_time_a_few_times() {
        let s = Settings::default();
        let mut a = Agent::default();
        // Never launched: no relaunch.
        assert_eq!(a.observe(None, 1000, &s).relaunch, None);
        a.launched_at = Some(1000);
        assert_eq!(a.observe(None, 1089, &s).relaunch, None);
        assert_eq!(a.observe(None, 1090, &s).relaunch, Some(1));
        assert_eq!(a.observe(None, 1100, &s).relaunch, Some(2));
        assert_eq!(a.observe(None, 1110, &s).relaunch, Some(3));
        assert_eq!(a.observe(None, 1120, &s).relaunch, None);
        // An answer resets the budget and reports the change.
        let up = status(r#"{"running":true}"#);
        let seen = a.observe(up, 1130, &s);
        assert_eq!(seen.changed, Some((State::Down, State::Live)));
        assert_eq!(a.relaunches, 0);
        assert_eq!(a.observe(None, 1140, &s).relaunch, Some(1));
    }
}
