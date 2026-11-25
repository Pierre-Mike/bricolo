//! The logic, with no capability calls and no generated bindings, so
//! `cargo test` runs it natively.

/// Polls after a restart, in seconds, until the server answers.
pub const RETRY_SECS: &[u32] = &[2, 5, 10, 20];

/// How much of the end of the server's log to search, as `tail -c` did.
pub const TAIL_BYTES: usize = 65_536;

pub const RESTARTING: &str = "Restarting the server…";

/// The settings from `init`, with defaults for missing keys.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub port: u16,
    pub bricolo: String,
    pub server_log: String,
    pub poll_ms: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            port: 3080,
            bricolo: "/Users/me/Applications/bricolo.app/Contents/Helpers/bricolo.app/Contents/MacOS/bricolo".into(),
            server_log: "~/.local/share/bricolo/logs/sidecar-dsh.log".into(),
            poll_ms: 15_000,
        }
    }
}

impl Settings {
    pub fn from_json(config: &str) -> Self {
        let d = Settings::default();
        let Ok(c) = serde_json::from_str::<serde_json::Value>(config) else {
            return d;
        };
        let text = |key: &str, default: String| {
            c[key]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or(default)
        };
        Settings {
            port: c["port"]
                .as_u64()
                .and_then(|p| u16::try_from(p).ok())
                .unwrap_or(d.port),
            bricolo: text("bricolo", d.bricolo),
            server_log: text("server_log", d.server_log),
            poll_ms: c["poll_ms"]
                .as_u64()
                .and_then(|ms| u32::try_from(ms).ok())
                .unwrap_or(d.poll_ms)
                .max(1_000),
        }
    }

    /// "http://127.0.0.1:3080".
    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// The last `TAIL_BYTES` of the log, as text.
pub fn tail(log: &[u8]) -> String {
    let start = log.len().saturating_sub(TAIL_BYTES);
    String::from_utf8_lossy(&log[start..]).into_owned()
}

/// The newest "dsh web: http://…" address in the server's output, with its
/// token. The log keeps every start, so the last one wins.
pub fn address_from(text: &str) -> Option<String> {
    const MARK: &str = "dsh web:";
    let mut found = None;
    let mut rest = text;
    while let Some(i) = rest.find(MARK) {
        rest = &rest[i + MARK.len()..];
        let url = rest.split_whitespace().next().unwrap_or("");
        if url.starts_with("http://") || url.starts_with("https://") {
            found = Some(url);
        }
    }
    found
        .map(|url| url.trim_end_matches(['.', ',', ';', ')', ']']))
        .map(str::to_string)
}

/// Whether the server answers, and what the tile says while it moves.
#[derive(Debug, Default, PartialEq)]
pub struct State {
    pub running: bool,
    pub busy: Option<String>,
}

impl State {
    /// Records a poll. Returns the line to log when the server came up or
    /// went down.
    pub fn polled(&mut self, up: bool) -> Option<&'static str> {
        let changed = up != self.running;
        self.running = up;
        if up {
            self.busy = None;
        }
        changed.then_some(if up { "server up" } else { "server down" })
    }

    /// Starts a restart. False while one is already under way.
    pub fn begin_restart(&mut self) -> bool {
        if self.busy.is_some() {
            return false;
        }
        self.busy = Some(RESTARTING.into());
        true
    }

    /// The tile's status line.
    pub fn status(&self, base: &str) -> String {
        match &self.busy {
            Some(b) => b.clone(),
            None if self.running => format!("Serving {base}"),
            None => "Server not answering; bricolo restarts it".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_newest_address_and_trims_punctuation() {
        let log = "dsh web: http://127.0.0.1:3080/?token=old\n\
                   noise\n\
                   dsh web:   http://127.0.0.1:3080/?token=new).\n";
        assert_eq!(
            address_from(log).as_deref(),
            Some("http://127.0.0.1:3080/?token=new")
        );
        assert_eq!(
            address_from("dsh web: https://host/x;").as_deref(),
            Some("https://host/x")
        );
    }

    #[test]
    fn ignores_lines_without_an_address() {
        assert_eq!(address_from(""), None);
        assert_eq!(address_from("dsh web: starting"), None);
        assert_eq!(
            address_from("dsh web: http://a/1\ndsh web: ftp://b").as_deref(),
            Some("http://a/1")
        );
    }

    #[test]
    fn tails_the_last_bytes() {
        let mut log = vec![b'x'; TAIL_BYTES];
        log.extend_from_slice(b"dsh web: http://a/");
        let t = tail(&log);
        assert_eq!(t.len(), TAIL_BYTES);
        assert!(t.ends_with("dsh web: http://a/"));
        assert_eq!(tail(b"short"), "short");
    }

    #[test]
    fn polls_log_changes_and_clear_the_restart() {
        let mut s = State::default();
        assert_eq!(s.polled(false), None);
        assert!(s.begin_restart());
        assert!(!s.begin_restart());
        assert_eq!(s.polled(false), None);
        assert_eq!(s.busy.as_deref(), Some(RESTARTING));
        assert_eq!(s.polled(true), Some("server up"));
        assert_eq!(s.busy, None);
        assert_eq!(s.polled(true), None);
        assert_eq!(s.polled(false), Some("server down"));
    }

    #[test]
    fn status_line() {
        let base = "http://127.0.0.1:3080";
        let mut s = State::default();
        assert_eq!(s.status(base), "Server not answering; bricolo restarts it");
        s.polled(true);
        assert_eq!(s.status(base), "Serving http://127.0.0.1:3080");
        s.begin_restart();
        assert_eq!(s.status(base), RESTARTING);
    }

    #[test]
    fn settings_fall_back_to_defaults() {
        let d = Settings::default();
        assert_eq!(Settings::from_json("{}"), d);
        assert_eq!(Settings::from_json("junk"), d);
        let s = Settings::from_json(r#"{"port":4000,"poll_ms":5,"bricolo":""}"#);
        assert_eq!(s.port, 4000);
        assert_eq!(s.poll_ms, 1_000);
        assert_eq!(s.bricolo, d.bricolo);
        assert_eq!(s.base(), "http://127.0.0.1:4000");
        assert_eq!(Settings::from_json(r#"{"port":70000}"#).port, 3080);
    }
}
