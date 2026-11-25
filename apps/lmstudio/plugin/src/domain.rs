//! The logic, with no capability calls and no generated bindings, so
//! `cargo test` runs it natively.

use serde_json::Value;

/// How often a detached load checks `lms ps` for its model.
pub const LOAD_POLL_MS: u32 = 2_000;

/// The "Unload when idle" choices, in the order the button cycles them.
/// `lms load --ttl` takes the seconds; 0 means never.
pub const TTLS: &[(&str, u32)] = &[
    ("Never", 0),
    ("10 minutes", 600),
    ("30 minutes", 1800),
    ("1 hour", 3600),
];

/// The settings from `init`, with defaults for missing keys.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub lms: String,
    pub load_log: String,
    pub poll_ms: u32,
    pub catalog_ms: u32,
    pub load_timeout_ms: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            lms: "/Users/me/.lmstudio/bin/lms".into(),
            load_log: "/tmp/bricolo-lmstudio-load.log".into(),
            poll_ms: 15_000,
            catalog_ms: 5 * 60 * 1000,
            load_timeout_ms: 5 * 60 * 1000,
        }
    }
}

impl Settings {
    pub fn from_json(config: &str) -> Self {
        let d = Settings::default();
        let c = serde_json::from_str::<Value>(config).unwrap_or(Value::Null);
        let text = |key: &str, or: String| c[key].as_str().map(str::to_string).unwrap_or(or);
        let ms = |key: &str, or: u32| {
            c[key]
                .as_u64()
                .and_then(|ms| u32::try_from(ms).ok())
                .unwrap_or(or)
                .max(1_000)
        };
        Settings {
            lms: text("lms", d.lms),
            load_log: text("load_log", d.load_log),
            poll_ms: ms("poll_ms", d.poll_ms),
            catalog_ms: ms("catalog_ms", d.catalog_ms),
            load_timeout_ms: ms("load_timeout_ms", d.load_timeout_ms),
        }
    }
}

/// A downloaded model, from `lms ls --json`.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub key: String,
    pub display_name: Option<String>,
    /// "llm" or "embedding".
    pub kind: Option<String>,
    pub size_bytes: f64,
}

/// A loaded model, from `lms ps --json`.
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    pub key: String,
    pub identifier: Option<String>,
    pub kind: Option<String>,
}

fn field(v: &Value, key: &str) -> Option<String> {
    v[key].as_str().map(str::to_string)
}

/// The JSON array in `out`, or None when `out` is no JSON.
fn array(out: &str) -> Option<Vec<Value>> {
    match serde_json::from_str::<Value>(out).ok()? {
        Value::Array(items) => Some(items),
        // The script took any JSON table; an object holds no models.
        Value::Object(_) => Some(Vec::new()),
        _ => None,
    }
}

/// The catalog from `lms ls --json`, or None when it is no JSON.
pub fn parse_catalog(out: &str) -> Option<Vec<Model>> {
    Some(
        array(out)?
            .iter()
            .filter_map(|m| {
                Some(Model {
                    key: field(m, "modelKey")?,
                    display_name: field(m, "displayName"),
                    kind: field(m, "type"),
                    size_bytes: m["sizeBytes"].as_f64().unwrap_or(0.0),
                })
            })
            .collect(),
    )
}

/// The loaded models from `lms ps --json`, or None when it is no JSON.
pub fn parse_loaded(out: &str) -> Option<Vec<Loaded>> {
    Some(
        array(out)?
            .iter()
            .filter_map(|p| {
                Some(Loaded {
                    key: field(p, "modelKey")?,
                    identifier: field(p, "identifier"),
                    kind: field(p, "type"),
                })
            })
            .collect(),
    )
}

/// What `lms server status --json` says.
#[derive(Clone, Debug, PartialEq)]
pub struct Server {
    pub running: bool,
    pub port: u16,
}

impl Default for Server {
    fn default() -> Self {
        Server {
            running: false,
            port: 1234,
        }
    }
}

impl Server {
    /// The server after reading `out`; a port missing from it keeps the old
    /// one, and an answer that is no JSON object changes nothing.
    pub fn read(&self, out: &str) -> Server {
        match serde_json::from_str::<Value>(out) {
            Ok(st @ Value::Object(_)) => Server {
                running: st["running"] == Value::Bool(true),
                port: st["port"]
                    .as_u64()
                    .and_then(|p| u16::try_from(p).ok())
                    .unwrap_or(self.port),
            },
            _ => self.clone(),
        }
    }
}

/// Why `lms` failed: the first line of stderr, else of stdout, without
/// leading space.
pub fn lms_error(stdout: &str, stderr: &str) -> String {
    let out = if stderr.is_empty() { stdout } else { stderr };
    out.trim_start().lines().next().unwrap_or("").to_string()
}

/// "15.3 GB".
pub fn gb(bytes: f64) -> String {
    format!("{:.1} GB", bytes / 1e9)
}

/// Everything the tile shows and the load waits on.
#[derive(Debug, Default)]
pub struct State {
    pub catalog: Vec<Model>,
    pub loaded: Vec<Loaded>,
    pub server: Server,
    /// One line while the server or a model moves.
    pub busy: Option<String>,
    /// The model key a detached load is waiting for.
    pub waiting: Option<String>,
    /// The model keys of the open choice list, in its order.
    pub picks: Vec<String>,
}

impl State {
    pub fn name_of(&self, key: &str) -> String {
        self.catalog
            .iter()
            .find(|m| m.key == key)
            .map(|m| m.display_name.clone().unwrap_or_else(|| key.to_string()))
            .unwrap_or_else(|| key.to_string())
    }

    pub fn kind_of(&self, key: &str) -> Option<String> {
        self.catalog
            .iter()
            .find(|m| m.key == key)
            .and_then(|m| m.kind.clone())
    }

    /// The loaded models a new `key` replaces: those of the same kind, by
    /// the identifier `lms unload` takes.
    pub fn replaced(&self, key: &str) -> Vec<String> {
        let kind = self.kind_of(key);
        self.loaded
            .iter()
            .filter(|p| p.key != key && p.kind.clone().or_else(|| self.kind_of(&p.key)) == kind)
            .map(|p| p.identifier.clone().unwrap_or_else(|| p.key.clone()))
            .collect()
    }

    pub fn is_loaded(&self, key: &str) -> bool {
        self.loaded.iter().any(|p| p.key == key)
    }

    /// The chat models to pick from: their keys, and labels with a dot on
    /// the loaded ones.
    pub fn choices(&self) -> (Vec<String>, Vec<String>) {
        self.catalog
            .iter()
            .filter(|m| m.kind.as_deref() == Some("llm"))
            .map(|m| {
                let dot = if self.is_loaded(&m.key) { "● " } else { "" };
                let name = m.display_name.as_deref().unwrap_or(&m.key);
                (m.key.clone(), format!("{dot}{name} · {}", gb(m.size_bytes)))
            })
            .unzip()
    }

    /// The tile's status: the busy line, else the server and what it holds.
    pub fn status(&self) -> String {
        if let Some(busy) = &self.busy {
            return busy.clone();
        }
        let server = if self.server.running {
            format!("Server on :{}", self.server.port)
        } else {
            "Server off".into()
        };
        let names: Vec<String> = self.loaded.iter().map(|p| self.name_of(&p.key)).collect();
        let models = if names.is_empty() {
            "nothing loaded".into()
        } else {
            names.join(", ")
        };
        format!("{server} · {models}")
    }
}

/// The index in TTLS of the kv value "ttl" (seconds), the first when it is
/// missing or unknown.
pub fn ttl_index(kv: Option<&str>) -> usize {
    let secs = kv.and_then(|s| s.trim().parse::<f64>().ok()).unwrap_or(0.0);
    TTLS.iter()
        .position(|(_, s)| f64::from(*s) == secs)
        .unwrap_or(0)
}

/// The kv value "ttl" after the button: the next choice, round the list.
pub fn next_ttl(kv: Option<&str>) -> String {
    TTLS[(ttl_index(kv) + 1) % TTLS.len()].1.to_string()
}

/// `s` in single quotes for sh.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The `sh -c` line that starts `lms load` detached, with its output in
/// `log`, so it may outlast the 60 seconds a run may take.
pub fn load_command(lms: &str, key: &str, ttl_secs: u32, log: &str) -> String {
    let ttl = if ttl_secs > 0 {
        format!(" --ttl {ttl_secs}")
    } else {
        String::new()
    };
    format!(
        "nohup {} load {} -y{ttl} >{} 2>&1 </dev/null &",
        quote(lms),
        quote(key),
        quote(log)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const LS: &str = r#"[
      {"type":"llm","modelKey":"qwen3-30b","displayName":"Qwen3 30B","sizeBytes":17200000000},
      {"type":"llm","modelKey":"gemma-3-27b","sizeBytes":15000000000},
      {"type":"embedding","modelKey":"nomic-embed","displayName":"Nomic Embed","sizeBytes":84000000}
    ]"#;

    fn state(ps: &str) -> State {
        State {
            catalog: parse_catalog(LS).unwrap(),
            loaded: parse_loaded(ps).unwrap(),
            ..State::default()
        }
    }

    #[test]
    fn parses_the_catalog_and_skips_entries_without_a_key() {
        let catalog = parse_catalog(r#"[{"modelKey":"a","type":"llm"},{"type":"llm"}]"#).unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].size_bytes, 0.0);
        assert_eq!(parse_catalog("not json"), None);
        assert_eq!(parse_catalog("{}"), Some(vec![]));
    }

    #[test]
    fn reads_the_server_status_and_keeps_the_old_port() {
        let old = Server::default();
        assert_eq!(
            old.read(r#"{"running":true,"port":4321}"#),
            Server {
                running: true,
                port: 4321
            }
        );
        assert_eq!(
            old.read(r#"{"running":false}"#),
            Server {
                running: false,
                port: 1234
            }
        );
        let on = Server {
            running: true,
            port: 9,
        };
        assert_eq!(on.read("lms: error"), on);
    }

    #[test]
    fn takes_the_first_line_of_an_error() {
        assert_eq!(
            lms_error("out", "\n  No model found\nmore"),
            "No model found"
        );
        assert_eq!(lms_error("  from stdout\nx", ""), "from stdout");
        assert_eq!(lms_error("", ""), "");
    }

    #[test]
    fn replaces_only_loaded_models_of_the_same_kind() {
        let s = state(
            r#"[{"modelKey":"gemma-3-27b","identifier":"gemma:2"},
                {"modelKey":"nomic-embed","identifier":"nomic-embed"},
                {"modelKey":"qwen3-30b","type":"llm"}]"#,
        );
        assert_eq!(s.replaced("qwen3-30b"), ["gemma:2"]);
        assert_eq!(s.replaced("nomic-embed"), Vec::<String>::new());
        // A model `lms ps` names by its kind, missing from the catalog.
        let s = state(r#"[{"modelKey":"other","type":"llm"}]"#);
        assert_eq!(s.replaced("gemma-3-27b"), ["other"]);
    }

    #[test]
    fn lists_chat_models_with_a_dot_on_the_loaded_one() {
        let s = state(r#"[{"modelKey":"gemma-3-27b"}]"#);
        let (keys, labels) = s.choices();
        assert_eq!(keys, ["qwen3-30b", "gemma-3-27b"]);
        assert_eq!(labels, ["Qwen3 30B · 17.2 GB", "● gemma-3-27b · 15.0 GB"]);
    }

    #[test]
    fn draws_the_status_line() {
        let mut s = state(r#"[{"modelKey":"qwen3-30b"},{"modelKey":"nomic-embed"}]"#);
        assert_eq!(s.status(), "Server off · Qwen3 30B, Nomic Embed");
        s.server.running = true;
        s.loaded.clear();
        assert_eq!(s.status(), "Server on :1234 · nothing loaded");
        s.busy = Some("Loading Qwen3 30B…".into());
        assert_eq!(s.status(), "Loading Qwen3 30B…");
    }

    #[test]
    fn cycles_the_idle_unload_choices() {
        assert_eq!(ttl_index(None), 0);
        assert_eq!(ttl_index(Some("1800")), 2);
        assert_eq!(ttl_index(Some("42")), 0);
        assert_eq!(next_ttl(None), "600");
        assert_eq!(next_ttl(Some("1800")), "3600");
        assert_eq!(next_ttl(Some("3600")), "0");
    }

    #[test]
    fn builds_the_detached_load_command() {
        assert_eq!(
            load_command("/bin/lms", "it's", 600, "/tmp/l.log"),
            r#"nohup '/bin/lms' load 'it'\''s' -y --ttl 600 >'/tmp/l.log' 2>&1 </dev/null &"#
        );
        assert_eq!(
            load_command("/bin/lms", "m", 0, "/tmp/l.log"),
            "nohup '/bin/lms' load 'm' -y >'/tmp/l.log' 2>&1 </dev/null &"
        );
    }

    #[test]
    fn settings_fall_back_to_defaults() {
        assert_eq!(Settings::from_json("{}"), Settings::default());
        assert_eq!(Settings::from_json("junk"), Settings::default());
        let s = Settings::from_json(r#"{"lms":"/x/lms","poll_ms":5}"#);
        assert_eq!(s.lms, "/x/lms");
        assert_eq!(s.poll_ms, 1_000);
    }
}
