//! `claude-code`: Claude Code sessions on this node, run headless through
//! the `claude` CLI. A session starts with
//! `claude -p <prompt> --output-format stream-json --verbose
//! --permission-mode <mode> <extra_args>` in its folder, and each later
//! prompt runs the same command with `--resume <id>`. The node reads the
//! JSON lines Claude Code prints: the `system` `init` line gives the
//! session id, and the `result` line ends the turn.
//!
//! `node.toml`:
//!
//! ```toml
//! [claude-code]
//! dirs = ["~/Github"]               # folders sessions may work in
//! command = "claude"                # default "claude"; `~/` expands
//! permission_mode = "acceptEdits"   # the default
//! extra_args = ["--model", "opus"]  # added to every run
//! ```
//!
//! Without `dirs` every call answers `unavailable`. The sessions live in
//! memory: `list` shows the ones started since the node started, by any
//! caller.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Notify, oneshot};

use crate::caller::Caller;
use crate::disk::{expand, home};
use crate::error::CallError;

/// How long `start` waits for Claude Code's session id. Under the router's
/// default 30 s, so a slow start answers `timeout` from here.
const START_TIMEOUT: Duration = Duration::from_secs(25);

/// How much of Claude Code's stderr a failed turn keeps.
const STDERR_TAIL: usize = 2000;

/// The longest title kept, in characters.
const TITLE_MAX: usize = 120;

/// Where a session is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    /// Stopped before its turn ended.
    Idle,
    /// A turn is running.
    Working,
    /// The turn ended with permissions refused.
    Waiting,
    /// The turn ended.
    Finished,
    /// The turn ended with an error, or Claude Code exited without one.
    Failed,
}

/// One Claude Code session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Claude Code's session id.
    pub id: String,
    pub dir: String,
    pub state: SessionState,
    /// The text of the last finished turn; for a failed one, the error.
    pub last_result: Option<String>,
    /// The first line of the prompt that started the session.
    pub title: String,
    /// Unix seconds.
    pub started: u64,
    /// Unix seconds.
    pub updated: u64,
}

#[async_trait]
pub trait ClaudeCodeProvider: Send + Sync {
    /// Starts a session on `prompt` in `dir`; answers once it has an id.
    async fn start(
        &self,
        caller: &Caller,
        dir: String,
        prompt: String,
    ) -> Result<Session, CallError>;
    /// Resumes session `id`, which must not be working, with `prompt`.
    async fn send(&self, caller: &Caller, id: String, prompt: String)
    -> Result<Session, CallError>;
    async fn list(&self, caller: &Caller) -> Result<Vec<Session>, CallError>;
    async fn status(&self, caller: &Caller, id: String) -> Result<Session, CallError>;
    /// Stops the running turn, if any; the session becomes idle.
    async fn stop(&self, caller: &Caller, id: String) -> Result<(), CallError>;
}

/// No Claude Code on this node.
pub struct NoClaudeCode;

#[async_trait]
impl ClaudeCodeProvider for NoClaudeCode {
    async fn start(&self, _: &Caller, _: String, _: String) -> Result<Session, CallError> {
        Err(CallError::Unavailable)
    }
    async fn send(&self, _: &Caller, _: String, _: String) -> Result<Session, CallError> {
        Err(CallError::Unavailable)
    }
    async fn list(&self, _: &Caller) -> Result<Vec<Session>, CallError> {
        Err(CallError::Unavailable)
    }
    async fn status(&self, _: &Caller, _: String) -> Result<Session, CallError> {
        Err(CallError::Unavailable)
    }
    async fn stop(&self, _: &Caller, _: String) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
}

/// `[claude-code]` in `node.toml`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClaudeCodeConfig {
    /// The `claude` program; default "claude", found on `PATH`.
    #[serde(default)]
    pub command: Option<String>,
    /// The folders sessions may work in, and everything under them.
    #[serde(default)]
    pub dirs: Vec<String>,
    /// `--permission-mode`; default "acceptEdits".
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// Added to every run, after the node's own arguments.
    #[serde(default)]
    pub extra_args: Vec<String>,
}

/// A session and the run serving it.
struct Entry {
    session: Session,
    /// Which run last changed this session. A run that finishes after a
    /// newer one started leaves the session alone.
    run: u64,
    /// Wakes the running turn's task to kill it.
    stop: Arc<Notify>,
}

type Sessions = Arc<Mutex<BTreeMap<String, Entry>>>;

/// Runs the `claude` CLI. See the module docs.
pub struct LocalClaudeCode {
    command: String,
    roots: Vec<PathBuf>,
    home: PathBuf,
    permission_mode: String,
    extra_args: Vec<String>,
    sessions: Sessions,
    runs: AtomicU64,
}

impl LocalClaudeCode {
    /// `~/` in `command`, `dirs` and the folders callers name expands to
    /// `home`.
    pub fn new(config: &ClaudeCodeConfig, home: &Path) -> Self {
        let command = config.command.clone().unwrap_or_else(|| "claude".into());
        let command = match expand(&command, home) {
            Some(path) if command.starts_with('~') => path.to_string_lossy().into_owned(),
            _ => command,
        };
        let roots = config
            .dirs
            .iter()
            .filter_map(|d| {
                let root = expand(d, home);
                if root.is_none() {
                    tracing::error!("[claude-code] dirs: {d:?} is not an absolute path");
                }
                root
            })
            .collect();
        Self {
            command,
            roots,
            home: home.to_path_buf(),
            permission_mode: config
                .permission_mode
                .clone()
                .unwrap_or_else(|| "acceptEdits".into()),
            extra_args: config.extra_args.clone(),
            sessions: Arc::default(),
            runs: AtomicU64::new(1),
        }
    }

    /// The node's own config: `~/` is the user's home.
    pub fn from_config(config: &ClaudeCodeConfig) -> Self {
        Self::new(config, &home())
    }

    fn check_configured(&self) -> Result<(), CallError> {
        if self.roots.is_empty() {
            Err(CallError::Unavailable)
        } else {
            Ok(())
        }
    }

    /// The real path of `dir` when it is an existing folder under a root.
    fn authorize(&self, dir: &str) -> Result<PathBuf, CallError> {
        let full = expand(dir, &self.home).ok_or(CallError::Denied)?;
        let real = full
            .canonicalize()
            .map_err(|e| CallError::Failed(format!("cannot open {dir}: {e}")))?;
        if !real.is_dir() {
            return Err(CallError::Failed(format!("{dir} is not a folder")));
        }
        let allowed = self
            .roots
            .iter()
            .filter_map(|r| r.canonicalize().ok())
            .any(|root| real.starts_with(root));
        if allowed {
            Ok(real)
        } else {
            Err(CallError::Denied)
        }
    }

    /// The arguments of one run.
    fn args(&self, prompt: &str, resume: Option<&str>) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            prompt.to_string(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--permission-mode".into(),
            self.permission_mode.clone(),
        ];
        if let Some(id) = resume {
            args.push("--resume".into());
            args.push(id.into());
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }

    fn spawn(&self, dir: &Path, args: &[String]) -> Result<Child, CallError> {
        Command::new(&self.command)
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false)
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    tracing::warn!("claude-code: {} not found", self.command);
                    CallError::Unavailable
                }
                _ => CallError::Failed(format!("cannot run {}: {e}", self.command)),
            })
    }

    fn find(&self, id: &str) -> Result<Session, CallError> {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .get(id)
            .map(|e| e.session.clone())
            .ok_or_else(|| unknown(id))
    }
}

fn unknown(id: &str) -> CallError {
    CallError::Failed(format!("no session {id:?}"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The prompt's first non-empty line, shortened.
fn title(prompt: &str) -> String {
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    line.chars().take(TITLE_MAX).collect()
}

#[async_trait]
impl ClaudeCodeProvider for LocalClaudeCode {
    async fn start(
        &self,
        _caller: &Caller,
        dir: String,
        prompt: String,
    ) -> Result<Session, CallError> {
        self.check_configured()?;
        let real = self.authorize(&dir)?;
        let mut child = self.spawn(&real, &self.args(&prompt, None))?;
        let run = self.runs.fetch_add(1, Ordering::Relaxed);
        let stop = Arc::new(Notify::new());
        let (tx, rx) = oneshot::channel();
        let new = NewSession {
            dir: real.to_string_lossy().into_owned(),
            title: title(&prompt),
            started: tx,
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        tokio::spawn(drive(
            child,
            stdout,
            stderr,
            Run {
                sessions: self.sessions.clone(),
                run,
                stop: stop.clone(),
            },
            Known::New(new),
        ));
        match tokio::time::timeout(START_TIMEOUT, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(CallError::Failed("claude stopped".into())),
            Err(_) => {
                stop.notify_one();
                Err(CallError::Timeout)
            }
        }
    }

    async fn send(
        &self,
        _caller: &Caller,
        id: String,
        prompt: String,
    ) -> Result<Session, CallError> {
        self.check_configured()?;
        let dir = {
            let sessions = self.sessions.lock().unwrap();
            let entry = sessions.get(&id).ok_or_else(|| unknown(&id))?;
            if entry.session.state == SessionState::Working {
                return Err(CallError::Failed("session is working".into()));
            }
            PathBuf::from(&entry.session.dir)
        };
        let mut child = self.spawn(&dir, &self.args(&prompt, Some(&id)))?;
        let run = self.runs.fetch_add(1, Ordering::Relaxed);
        let stop = Arc::new(Notify::new());
        let session = {
            let mut sessions = self.sessions.lock().unwrap();
            let Some(entry) = sessions.get_mut(&id) else {
                let _ = child.start_kill();
                return Err(unknown(&id));
            };
            entry.run = run;
            entry.stop = stop.clone();
            entry.session.state = SessionState::Working;
            entry.session.updated = now();
            entry.session.clone()
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        tokio::spawn(drive(
            child,
            stdout,
            stderr,
            Run {
                sessions: self.sessions.clone(),
                run,
                stop,
            },
            Known::Id(id),
        ));
        Ok(session)
    }

    async fn list(&self, _caller: &Caller) -> Result<Vec<Session>, CallError> {
        self.check_configured()?;
        let sessions = self.sessions.lock().unwrap();
        let mut list: Vec<Session> = sessions.values().map(|e| e.session.clone()).collect();
        list.sort_by_key(|s| s.started);
        Ok(list)
    }

    async fn status(&self, _caller: &Caller, id: String) -> Result<Session, CallError> {
        self.check_configured()?;
        self.find(&id)
    }

    async fn stop(&self, _caller: &Caller, id: String) -> Result<(), CallError> {
        self.check_configured()?;
        let mut sessions = self.sessions.lock().unwrap();
        let entry = sessions.get_mut(&id).ok_or_else(|| unknown(&id))?;
        if entry.session.state == SessionState::Working {
            entry.stop.notify_one();
        }
        entry.session.state = SessionState::Idle;
        entry.session.updated = now();
        Ok(())
    }
}

/// What `start` waits for: the session, once its id is known.
struct NewSession {
    dir: String,
    title: String,
    started: oneshot::Sender<Result<Session, CallError>>,
}

/// Whether the run's session already has an id.
enum Known {
    New(NewSession),
    Id(String),
}

/// One run of the CLI: which session map it reports to, its number, and
/// what wakes it to stop.
struct Run {
    sessions: Sessions,
    run: u64,
    stop: Arc<Notify>,
}

impl Run {
    /// Applies `f` to the session, unless a newer run took it over.
    fn update(&self, id: &str, f: impl FnOnce(&mut Session)) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(entry) = sessions.get_mut(id)
            && entry.run == self.run
        {
            f(&mut entry.session);
            entry.session.updated = now();
        }
    }
}

/// The end of a turn, from a `result` line.
fn turn_end(line: &serde_json::Value) -> (SessionState, Option<String>) {
    let failed = line["is_error"].as_bool().unwrap_or(false)
        || line["subtype"].as_str().is_some_and(|s| s != "success");
    let denied = line["permission_denials"]
        .as_array()
        .is_some_and(|a| !a.is_empty());
    let state = if failed {
        SessionState::Failed
    } else if denied {
        SessionState::Waiting
    } else {
        SessionState::Finished
    };
    let text = match &line["result"] {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Null => line["subtype"].as_str().map(str::to_string),
        other => Some(other.to_string()),
    };
    (state, text)
}

/// Reads one run's output until it exits or is stopped, and keeps its
/// session up to date. A new session joins the map, and `start` gets its
/// answer, when the `init` line names it.
async fn drive(
    mut child: Child,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    run: Run,
    known: Known,
) {
    let tail = tokio::spawn(read_tail(stderr));
    let (mut pending, mut id) = match known {
        Known::New(new) => (Some(new), None),
        Known::Id(id) => (None, Some(id)),
    };
    let mut ended = false;
    let mut stopped = false;
    if let Some(stdout) = stdout {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            let line = tokio::select! {
                line = lines.next_line() => line,
                _ = run.stop.notified() => {
                    stopped = true;
                    break;
                }
            };
            let Ok(Some(line)) = line else { break };
            let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            match json["type"].as_str() {
                Some("system") if json["subtype"] == "init" && id.is_none() => {
                    let (Some(sid), Some(new)) = (json["session_id"].as_str(), pending.take())
                    else {
                        continue;
                    };
                    let t = now();
                    let session = Session {
                        id: sid.to_string(),
                        dir: new.dir,
                        state: SessionState::Working,
                        last_result: None,
                        title: new.title,
                        started: t,
                        updated: t,
                    };
                    run.sessions.lock().unwrap().insert(
                        sid.to_string(),
                        Entry {
                            session: session.clone(),
                            run: run.run,
                            stop: run.stop.clone(),
                        },
                    );
                    tracing::info!(session = sid, dir = %session.dir, "claude-code: started");
                    id = Some(sid.to_string());
                    // `start` may have given up waiting; it then stops this run.
                    let _ = new.started.send(Ok(session));
                }
                Some("result") => {
                    let (state, text) = turn_end(&json);
                    if let Some(id) = &id {
                        tracing::info!(session = %id, ?state, "claude-code: turn ended");
                        run.update(id, |s| {
                            s.state = state;
                            s.last_result = text;
                        });
                    }
                    ended = true;
                }
                _ => {}
            }
        }
    }
    if stopped {
        let _ = child.start_kill();
    }
    let status = child.wait().await;
    let tail = tail.await.unwrap_or_default();
    let why = if tail.is_empty() {
        match status {
            Ok(s) => format!("claude exited ({s}) without a result"),
            Err(e) => format!("claude: {e}"),
        }
    } else {
        tail
    };
    if let Some(new) = pending {
        let _ = new.started.send(Err(if stopped {
            CallError::Timeout
        } else {
            CallError::Failed(why)
        }));
        return;
    }
    let Some(id) = id else { return };
    if stopped {
        run.update(&id, |s| s.state = SessionState::Idle);
    } else if !ended {
        tracing::warn!(session = %id, "claude-code: exited without a result: {why}");
        run.update(&id, |s| {
            s.state = SessionState::Failed;
            s.last_result = Some(why);
        });
    }
}

/// The last [`STDERR_TAIL`] bytes of `stream`.
async fn read_tail<R: AsyncRead + Unpin>(stream: Option<R>) -> String {
    let Some(mut stream) = stream else {
        return String::new();
    };
    let mut tail: VecDeque<u8> = VecDeque::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                tail.extend(&buf[..n]);
                while tail.len() > STDERR_TAIL {
                    tail.pop_front();
                }
            }
        }
    }
    let bytes: Vec<u8> = tail.into();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_serialize_in_lowercase() {
        assert_eq!(
            serde_json::to_string(&SessionState::Working).unwrap(),
            "\"working\""
        );
        let s: SessionState = serde_json::from_str("\"waiting\"").unwrap();
        assert_eq!(s, SessionState::Waiting);
    }

    #[test]
    fn titles_take_the_first_line() {
        assert_eq!(title("\n  fix the tests  \nthen push"), "fix the tests");
        assert_eq!(title(&"x".repeat(500)).len(), TITLE_MAX);
    }

    #[test]
    fn reads_the_end_of_a_turn() {
        let line = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert_eq!(
            turn_end(&line(
                r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","permission_denials":[]}"#
            )),
            (SessionState::Finished, Some("ok".into()))
        );
        assert_eq!(
            turn_end(&line(
                r#"{"type":"result","subtype":"success","result":"hm","permission_denials":[{"tool_name":"Bash"}]}"#
            )),
            (SessionState::Waiting, Some("hm".into()))
        );
        assert_eq!(
            turn_end(&line(
                r#"{"type":"result","subtype":"error_max_turns","is_error":true}"#
            )),
            (SessionState::Failed, Some("error_max_turns".into()))
        );
    }

    #[tokio::test]
    async fn without_dirs_every_call_is_unavailable() {
        let home = tempfile::tempdir().unwrap();
        let cc = LocalClaudeCode::new(&ClaudeCodeConfig::default(), home.path());
        let c = Caller::new("p");
        let dir = home.path().to_string_lossy().into_owned();
        assert_eq!(cc.list(&c).await, Err(CallError::Unavailable));
        assert_eq!(
            cc.start(&c, dir, "hi".into()).await,
            Err(CallError::Unavailable)
        );
        assert_eq!(cc.status(&c, "x".into()).await, Err(CallError::Unavailable));
        assert_eq!(cc.stop(&c, "x".into()).await, Err(CallError::Unavailable));
    }

    /// Against a fake `claude` script, so no test runs the real one.
    #[cfg(unix)]
    mod fake {
        use super::*;

        /// Prints what `claude -p ... --output-format stream-json` prints,
        /// picked by the prompt (`$2`), and logs its arguments and folder.
        const FAKE: &str = r#"#!/bin/sh
here="$(dirname "$0")"
echo "$@" >> "$here/args.log"
pwd -P >> "$here/cwd.log"
case "$2" in
  noinit*) echo "not logged in" >&2; exit 1 ;;
esac
echo '{"type":"system","subtype":"init","session_id":"sess-1","cwd":"x"}'
case "$2" in
  deny*) echo '{"type":"result","subtype":"success","is_error":false,"result":"need Bash","permission_denials":[{"tool_name":"Bash"}]}' ;;
  error*) echo '{"type":"result","subtype":"error_during_execution","is_error":true,"result":"it broke"}' ;;
  crash*) echo "boom" >&2; exit 3 ;;
  slow*) exec sleep 30 ;;
  *) echo '{"type":"result","subtype":"success","is_error":false,"result":"done","permission_denials":[]}' ;;
esac
"#;

        struct Setup {
            bin: tempfile::TempDir,
            work: tempfile::TempDir,
            cc: LocalClaudeCode,
        }

        /// The fake in one temp folder; `work` is the allowed root and
        /// the home folder, with `work/repo` in it.
        fn setup() -> Setup {
            use std::os::unix::fs::PermissionsExt;
            let bin = tempfile::tempdir().unwrap();
            let fake = bin.path().join("claude");
            std::fs::write(&fake, FAKE).unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
            let work = tempfile::tempdir().unwrap();
            std::fs::create_dir(work.path().join("repo")).unwrap();
            let config = ClaudeCodeConfig {
                command: Some(fake.to_string_lossy().into()),
                dirs: vec!["~/".into()],
                permission_mode: None,
                extra_args: vec!["--model".into(), "opus".into()],
            };
            let cc = LocalClaudeCode::new(&config, work.path());
            Setup { bin, work, cc }
        }

        impl Setup {
            fn repo(&self) -> String {
                self.work.path().join("repo").to_string_lossy().into()
            }

            fn log(&self, name: &str) -> String {
                std::fs::read_to_string(self.bin.path().join(name)).unwrap_or_default()
            }

            /// The session once it stops working.
            async fn settled(&self, id: &str) -> Session {
                let c = Caller::new("p");
                for _ in 0..250 {
                    let s = self.cc.status(&c, id.into()).await.unwrap();
                    if s.state != SessionState::Working {
                        return s;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                panic!("session {id} still working");
            }
        }

        #[tokio::test]
        async fn a_turn_runs_to_finished() {
            let s = setup();
            let c = Caller::new("p");
            let session =
                s.cc.start(&c, s.repo(), "fix it\nplease".into())
                    .await
                    .unwrap();
            assert_eq!(session.id, "sess-1");
            assert_eq!(session.title, "fix it");
            assert_eq!(session.state, SessionState::Working);
            let done = s.settled("sess-1").await;
            assert_eq!(done.state, SessionState::Finished);
            assert_eq!(done.last_result.as_deref(), Some("done"));
            assert_eq!(
                s.log("args.log"),
                "-p fix it\nplease --output-format stream-json --verbose \
                 --permission-mode acceptEdits --model opus\n"
            );
            let real = s.work.path().join("repo").canonicalize().unwrap();
            assert_eq!(s.log("cwd.log").trim(), real.to_string_lossy());
            assert_eq!(done.dir, real.to_string_lossy());
            assert_eq!(s.cc.list(&c).await.unwrap(), vec![done]);
        }

        #[tokio::test]
        async fn refused_permissions_wait() {
            let s = setup();
            let c = Caller::new("p");
            s.cc.start(&c, s.repo(), "deny me".into()).await.unwrap();
            let done = s.settled("sess-1").await;
            assert_eq!(done.state, SessionState::Waiting);
            assert_eq!(done.last_result.as_deref(), Some("need Bash"));
        }

        #[tokio::test]
        async fn errors_fail() {
            let c = Caller::new("p");
            let s = setup();
            s.cc.start(&c, s.repo(), "error".into()).await.unwrap();
            let done = s.settled("sess-1").await;
            assert_eq!(done.state, SessionState::Failed);
            assert_eq!(done.last_result.as_deref(), Some("it broke"));

            // An exit without a result line keeps the end of stderr.
            let s = setup();
            s.cc.start(&c, s.repo(), "crash".into()).await.unwrap();
            let done = s.settled("sess-1").await;
            assert_eq!(done.state, SessionState::Failed);
            assert_eq!(done.last_result.as_deref(), Some("boom"));

            let s = setup();
            assert_eq!(
                s.cc.start(&c, s.repo(), "noinit".into()).await,
                Err(CallError::Failed("not logged in".into()))
            );
            assert_eq!(s.cc.list(&c).await.unwrap(), vec![]);
        }

        #[tokio::test]
        async fn folders_outside_the_roots_are_denied() {
            let s = setup();
            let c = Caller::new("p");
            let other = tempfile::tempdir().unwrap();
            let other = other.path().to_string_lossy().into_owned();
            assert_eq!(
                s.cc.start(&c, other, "x".into()).await,
                Err(CallError::Denied)
            );
            let escape = format!("{}/../..", s.repo());
            assert_eq!(
                s.cc.start(&c, escape, "x".into()).await,
                Err(CallError::Denied)
            );
            assert_eq!(
                s.cc.start(&c, "relative/path".into(), "x".into()).await,
                Err(CallError::Denied)
            );
            assert_eq!(s.log("args.log"), "");
            // `~/` is the home folder, here the allowed root.
            let session = s.cc.start(&c, "~/repo".into(), "x".into()).await.unwrap();
            assert_eq!(session.id, "sess-1");
        }

        #[tokio::test]
        async fn send_resumes_the_session() {
            let s = setup();
            let c = Caller::new("p");
            s.cc.start(&c, s.repo(), "first".into()).await.unwrap();
            s.settled("sess-1").await;
            let again =
                s.cc.send(&c, "sess-1".into(), "second".into())
                    .await
                    .unwrap();
            assert_eq!(again.state, SessionState::Working);
            assert_eq!(again.title, "first");
            assert_eq!(s.settled("sess-1").await.state, SessionState::Finished);
            let log = s.log("args.log");
            let second = log.lines().nth(1).unwrap();
            assert!(
                second.starts_with("-p second ") && second.contains(" --resume sess-1 "),
                "{second}"
            );
            assert!(matches!(
                s.cc.send(&c, "nope".into(), "x".into()).await,
                Err(CallError::Failed(_))
            ));
        }

        #[tokio::test]
        async fn a_working_session_takes_no_prompt_and_stops_to_idle() {
            let s = setup();
            let c = Caller::new("p");
            s.cc.start(&c, s.repo(), "slow".into()).await.unwrap();
            assert_eq!(
                s.cc.send(&c, "sess-1".into(), "more".into()).await,
                Err(CallError::Failed("session is working".into()))
            );
            s.cc.stop(&c, "sess-1".into()).await.unwrap();
            assert_eq!(
                s.cc.status(&c, "sess-1".into()).await.unwrap().state,
                SessionState::Idle
            );
            // The killed run leaves it idle, and it takes prompts again.
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(
                s.cc.status(&c, "sess-1".into()).await.unwrap().state,
                SessionState::Idle
            );
            s.cc.send(&c, "sess-1".into(), "go on".into())
                .await
                .unwrap();
            assert_eq!(s.settled("sess-1").await.state, SessionState::Finished);
        }
    }
}
