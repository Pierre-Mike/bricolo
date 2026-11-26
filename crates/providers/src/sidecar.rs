//! Sidecars (Decision 4): the local HTTP servers the node starts and watches.
//! Each one listed in `node.toml` under `[sidecars.<name>]` gets a task
//! that frees its port, starts it, polls its health URL, restarts it with a
//! backoff when it dies, and stops it, with its whole process group (a
//! job object on Windows), when the node shuts down. [`Sidecars::reload`] applies an edited `node.toml`
//! while the node runs (`bricolo sidecar add`, `bricolo sidecar reload`).
//!
//! ```toml
//! [sidecars.parakeet]
//! command = "~/.local/bin/uv"
//! args = ["run", "--script", "/path/to/bricolo/apps/shared/sidecars/parakeet/server.py"]
//! cwd = "~"                       # optional
//! port = 8765
//! health_url = "http://127.0.0.1:8765/health"   # optional, this is the default
//! startup_secs = 180              # optional: how long "starting" may last
//! env = { STT_ENGINE = "parakeet", STT_PORT = "8765" }
//! ```
//!
//! A sidecar is healthy when its health URL answers 200, and, if the body is
//! a JSON object with a `ready` key, `ready` is true (parakeet answers 200
//! with `"ready": false` while it loads its model).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

use crate::disk::expand;
use crate::http::client;

/// Health polls while a sidecar starts, and once it is up.
const POLL_STARTING: Duration = Duration::from_millis(250);
const POLL_RUNNING: Duration = Duration::from_secs(5);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
/// A sidecar that ran this long before dying restarts without delay.
const STABLE: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// SIGTERM (Ctrl+Break on Windows), then this long, then SIGKILL (the
/// job object killed).
const GRACE: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SidecarConfig {
    /// The program; `~/` expands to the home folder.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// Added to the node's own environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub port: u16,
    /// Default `http://127.0.0.1:<port>/health`.
    #[serde(default)]
    pub health_url: Option<String>,
    #[serde(default = "default_startup")]
    pub startup_secs: u64,
}

fn default_startup() -> u64 {
    120
}

impl SidecarConfig {
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn health(&self) -> String {
        self.health_url
            .clone()
            .unwrap_or_else(|| format!("{}/health", self.base_url()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SidecarState {
    /// Started, not healthy yet.
    Starting,
    Ready,
    /// Was ready, now fails its health check, still running.
    Unhealthy,
    /// Died or could not start; waiting out the backoff.
    Restarting,
    Stopped,
}

#[derive(Clone, Debug, Serialize)]
pub struct SidecarStatus {
    pub name: String,
    pub state: SidecarState,
    pub port: u16,
    pub pid: Option<u32>,
    pub restarts: u32,
    /// The last failure, kept after the sidecar recovers.
    pub error: Option<String>,
}

type Shared = Arc<Mutex<SidecarStatus>>;

struct Entry {
    config: SidecarConfig,
    status: Shared,
    task: Option<JoinHandle<()>>,
    restart: Arc<Notify>,
    stop: watch::Sender<bool>,
}

impl Entry {
    /// Starts the supervisor task. Must be called inside a Tokio runtime.
    fn start(name: &str, config: SidecarConfig, logs: Option<&Path>) -> Self {
        let status = Arc::new(Mutex::new(SidecarStatus {
            name: name.to_string(),
            state: SidecarState::Starting,
            port: config.port,
            pid: None,
            restarts: 0,
            error: None,
        }));
        let log = logs.map(|d| d.join(format!("sidecar-{name}.log")));
        let restart = Arc::new(Notify::new());
        let (stop, _) = watch::channel(false);
        let task = tokio::spawn(supervise(
            name.to_string(),
            config.clone(),
            status.clone(),
            Signals {
                stop: stop.subscribe(),
                restart: restart.clone(),
            },
            log,
        ));
        Self {
            config,
            status,
            task: Some(task),
            restart,
            stop,
        }
    }

    /// Asks the supervisor to stop its process. Await the returned task
    /// to wait for it.
    fn signal_stop(&mut self) -> Option<JoinHandle<()>> {
        let _ = self.stop.send(true);
        self.task.take()
    }
}

/// What [`Sidecars::reload`] changed, by sidecar name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ReloadReport {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// Their configuration changed, so they started again.
    pub restarted: Vec<String>,
    pub unchanged: Vec<String>,
}

/// Every sidecar of this node. [`Sidecars::reload`] adds, removes and
/// restarts them while the node runs.
pub struct Sidecars {
    entries: Mutex<BTreeMap<String, Entry>>,
    logs: Option<PathBuf>,
    /// Set by [`Sidecars::shutdown`]; a later reload starts nothing.
    stopped: Mutex<bool>,
}

impl Sidecars {
    /// No sidecars.
    pub fn none() -> Self {
        Self::start(BTreeMap::new(), None)
    }

    /// Starts a supervisor for each sidecar. Output goes to
    /// `<logs>/sidecar-<name>.log` when `logs` is set. Must be called inside
    /// a Tokio runtime.
    pub fn start(configs: BTreeMap<String, SidecarConfig>, logs: Option<PathBuf>) -> Self {
        let entries = configs
            .into_iter()
            .map(|(name, config)| {
                let entry = Entry::start(&name, config, logs.as_deref());
                (name, entry)
            })
            .collect();
        Self {
            entries: Mutex::new(entries),
            logs,
            stopped: Mutex::new(false),
        }
    }

    pub fn status(&self) -> Vec<SidecarStatus> {
        self.entries
            .lock()
            .unwrap()
            .values()
            .map(|e| e.status.lock().unwrap().clone())
            .collect()
    }

    pub fn config(&self, name: &str) -> Option<SidecarConfig> {
        self.entries
            .lock()
            .unwrap()
            .get(name)
            .map(|e| e.config.clone())
    }

    pub fn state(&self, name: &str) -> Option<SidecarState> {
        self.entries
            .lock()
            .unwrap()
            .get(name)
            .map(|e| e.status.lock().unwrap().state)
    }

    /// Stops the sidecar's process and starts it again at once, skipping
    /// any backoff. False when there is no such sidecar.
    pub fn restart(&self, name: &str) -> bool {
        match self.entries.lock().unwrap().get(name) {
            Some(entry) => {
                tracing::info!(sidecar = name, "restart requested");
                entry.restart.notify_one();
                true
            }
            None => false,
        }
    }

    /// Makes the running sidecars match `configs`, as reread from
    /// `node.toml`: starts the new ones, stops the ones no longer listed,
    /// and starts again those whose configuration changed. The others keep
    /// running untouched. Waits for the stopped ones to exit.
    pub async fn reload(&self, configs: BTreeMap<String, SidecarConfig>) -> ReloadReport {
        let mut report = ReloadReport::default();
        let mut stopping = Vec::new();
        {
            let mut entries = self.entries.lock().unwrap();
            let names: Vec<String> = entries.keys().cloned().collect();
            for name in names {
                let keep = configs.get(&name);
                if keep == Some(&entries[&name].config) {
                    report.unchanged.push(name);
                    continue;
                }
                let mut entry = entries.remove(&name).unwrap();
                stopping.extend(entry.signal_stop());
                if keep.is_some() {
                    tracing::info!(sidecar = %name, "changed in node.toml; restarting");
                    report.restarted.push(name);
                } else {
                    tracing::info!(sidecar = %name, "removed from node.toml");
                    report.removed.push(name);
                }
            }
        }
        // The old processes exit before their replacements start, so a
        // restarted sidecar finds its port free.
        for task in stopping {
            let _ = task.await;
        }
        let mut entries = self.entries.lock().unwrap();
        if *self.stopped.lock().unwrap() {
            return report;
        }
        for (name, config) in configs {
            if entries.contains_key(&name) {
                continue;
            }
            if !report.restarted.contains(&name) {
                tracing::info!(sidecar = %name, "added to node.toml");
                report.added.push(name.clone());
            }
            let entry = Entry::start(&name, config, self.logs.as_deref());
            entries.insert(name, entry);
        }
        report
    }

    /// Stops every sidecar and waits for them to exit.
    pub async fn shutdown(&self) {
        *self.stopped.lock().unwrap() = true;
        let tasks: Vec<JoinHandle<()>> = self
            .entries
            .lock()
            .unwrap()
            .values_mut()
            .filter_map(Entry::signal_stop)
            .collect();
        for task in tasks {
            let _ = task.await;
        }
    }
}

/// 1 s after the first failure, doubling up to a minute.
pub fn backoff(failures: u32) -> Duration {
    let secs = 1u64 << failures.saturating_sub(1).min(6);
    Duration::from_secs(secs).min(MAX_BACKOFF)
}

/// True for a 200 whose body, if a JSON object with `ready`, says ready.
pub fn healthy(status: u16, body: &str) -> bool {
    if status != 200 {
        return false;
    }
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(serde_json::Value::Object(map)) => map
            .get("ready")
            .is_none_or(|r| r.as_bool().unwrap_or(false)),
        _ => true,
    }
}

async fn check_health(url: String) -> bool {
    tokio::task::spawn_blocking(move || {
        client::get(&url, HEALTH_TIMEOUT).is_ok_and(|t| healthy(t.status, &t.body))
    })
    .await
    .unwrap_or(false)
}

fn set(status: &Shared, f: impl FnOnce(&mut SidecarStatus)) {
    f(&mut status.lock().unwrap());
}

enum Outcome {
    Exited(String),
    Restart,
    Stop,
}

/// What a supervisor listens to besides its child.
struct Signals {
    stop: watch::Receiver<bool>,
    restart: Arc<Notify>,
}

async fn supervise(
    name: String,
    config: SidecarConfig,
    status: Shared,
    mut signals: Signals,
    log: Option<PathBuf>,
) {
    let mut failures = 0;
    while !*signals.stop.borrow() {
        free_port(config.port).await;
        set(&status, |s| {
            s.state = SidecarState::Starting;
            s.pid = None;
        });
        let started = Instant::now();
        let reason = match spawn(&config, log.as_deref()) {
            Ok(mut child) => {
                let tree = Tree::hold(&name, &child);
                set(&status, |s| s.pid = child.id());
                tracing::info!(sidecar = %name, pid = ?child.id(), port = config.port, "started");
                match watch_child(&name, &config, &status, &mut child, &mut signals).await {
                    Outcome::Stop => {
                        tree.terminate(&mut child).await;
                        break;
                    }
                    Outcome::Restart => {
                        tree.terminate(&mut child).await;
                        failures = 0;
                        set(&status, |s| s.restarts += 1);
                        continue;
                    }
                    Outcome::Exited(why) => why,
                }
            }
            Err(e) => e,
        };
        if started.elapsed() >= STABLE {
            failures = 0;
        }
        failures += 1;
        let delay = backoff(failures);
        tracing::warn!(sidecar = %name, "{reason}; restarting in {} s", delay.as_secs());
        set(&status, |s| {
            s.state = SidecarState::Restarting;
            s.pid = None;
            s.restarts += 1;
            s.error = Some(reason);
        });
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = signals.restart.notified() => failures = 0,
            _ = signals.stop.changed() => break,
        }
    }
    set(&status, |s| {
        s.state = SidecarState::Stopped;
        s.pid = None;
    });
    tracing::info!(sidecar = %name, "stopped");
}

/// Polls health until the child exits or the node stops.
async fn watch_child(
    name: &str,
    config: &SidecarConfig,
    status: &Shared,
    child: &mut Child,
    signals: &mut Signals,
) -> Outcome {
    let started = Instant::now();
    let startup = Duration::from_secs(config.startup_secs);
    let mut warned = false;
    loop {
        let state = status.lock().unwrap().state;
        let pause = if state == SidecarState::Starting {
            POLL_STARTING
        } else {
            POLL_RUNNING
        };
        tokio::select! {
            exit = child.wait() => {
                return Outcome::Exited(match exit {
                    Ok(code) => format!("exited ({code})"),
                    Err(e) => format!("lost the process: {e}"),
                });
            }
            _ = signals.stop.changed() => return Outcome::Stop,
            _ = signals.restart.notified() => return Outcome::Restart,
            _ = tokio::time::sleep(pause) => {}
        }
        let ok = check_health(config.health()).await;
        set(status, |s| match (s.state, ok) {
            (SidecarState::Starting | SidecarState::Unhealthy, true) => {
                tracing::info!(
                    sidecar = name,
                    "ready after {} ms",
                    started.elapsed().as_millis()
                );
                s.state = SidecarState::Ready;
            }
            (SidecarState::Ready, false) => {
                tracing::warn!(sidecar = name, "health check failed");
                s.state = SidecarState::Unhealthy;
            }
            (SidecarState::Starting, false) if !warned && started.elapsed() > startup => {
                warned = true;
                let why = format!("not healthy after {} s", startup.as_secs());
                tracing::warn!(sidecar = name, "{why}");
                s.error = Some(why);
            }
            _ => {}
        });
    }
}

fn spawn(config: &SidecarConfig, log: Option<&Path>) -> Result<Child, String> {
    let home = crate::disk::home();
    let path = |s: &str| expand(s, &home).map_or_else(|| s.into(), |p| p.into_os_string());
    let mut cmd = Command::new(path(&config.command));
    cmd.args(config.args.iter().map(|a| path(a)))
        .envs(&config.env)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if let Some(cwd) = &config.cwd {
        cmd.current_dir(path(cwd));
    }
    match log.map(open_log).transpose()? {
        Some((out, err)) => cmd.stdout(out).stderr(err),
        None => cmd.stdout(Stdio::null()).stderr(Stdio::null()),
    };
    // Its own process group, so stopping it also stops what it started.
    #[cfg(unix)]
    cmd.process_group(0);
    // On Windows, its own console process group, for Ctrl+Break.
    #[cfg(windows)]
    cmd.creation_flags(crate::windows::process::NEW_PROCESS_GROUP);
    cmd.spawn()
        .map_err(|e| format!("cannot start {}: {e}", config.command))
}

fn open_log(path: &Path) -> Result<(Stdio, Stdio), String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let err = file.try_clone().map_err(|e| e.to_string())?;
    Ok((file.into(), err.into()))
}

/// What holds a sidecar's whole process tree besides its process group:
/// on Windows a kill-on-close job object, so what the sidecar started dies
/// with it, even when the node itself is killed.
struct Tree {
    #[cfg(windows)]
    job: Option<crate::windows::process::Job>,
}

impl Tree {
    #[cfg(windows)]
    fn hold(name: &str, child: &Child) -> Self {
        let job = child.raw_handle().and_then(|handle| {
            crate::windows::process::Job::hold(handle)
                .map_err(|e| {
                    tracing::warn!(
                        sidecar = name,
                        "no job object, stopping only the sidecar: {e}"
                    );
                })
                .ok()
        });
        Tree { job }
    }

    #[cfg(not(windows))]
    fn hold(_: &str, _: &Child) -> Self {
        Tree {}
    }

    /// SIGTERM to the process group, then SIGKILL after [`GRACE`]. On
    /// Windows: Ctrl+Break to its console process group, then the job
    /// object killed after [`GRACE`] (`taskkill /T /F` without a job).
    async fn terminate(&self, child: &mut Child) {
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            let group = -(pid as i32);
            // SAFETY: plain kill(2) on the group this child leads.
            unsafe { libc::kill(group, libc::SIGTERM) };
            if tokio::time::timeout(GRACE, child.wait()).await.is_ok() {
                unsafe { libc::kill(group, libc::SIGKILL) };
                return;
            }
            unsafe { libc::kill(group, libc::SIGKILL) };
        }
        #[cfg(windows)]
        if let Some(pid) = child.id() {
            use crate::windows::process::ctrl_break;
            let exited = ctrl_break(pid) && tokio::time::timeout(GRACE, child.wait()).await.is_ok();
            match &self.job {
                // Also takes what the sidecar left behind.
                Some(job) => job.kill(),
                // The pid may be reused once the child has exited.
                None if !exited => kill_tree(pid).await,
                None => {}
            }
            if exited {
                return;
            }
        }
        let _ = child.kill().await;
    }
}

/// Kills `pid` and the processes it started.
#[cfg(windows)]
async fn kill_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

/// Kills whatever listens on `port`, never this process, then waits up to
/// 5 s for the port to free up. Finds it with `lsof` (`netstat -ano` on
/// Windows); without it, does nothing. A port we can bind is already free,
/// so the lookup, which can take seconds on a busy machine, runs only when
/// something holds it.
async fn free_port(port: u16) {
    if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
        return;
    }
    let me = std::process::id();
    let Some(pids) = listeners(port).await else {
        return;
    };
    let pids: Vec<u32> = pids.into_iter().filter(|p| *p != me).collect();
    if pids.is_empty() {
        return;
    }
    for pid in &pids {
        tracing::warn!(port, pid, "killing the process that holds the port");
        #[cfg(unix)]
        // SAFETY: plain kill(2).
        unsafe {
            libc::kill(*pid as i32, libc::SIGKILL)
        };
        #[cfg(windows)]
        kill_tree(*pid).await;
    }
    for _ in 0..50 {
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The processes listening on TCP `port`; `None` when `lsof` is missing.
#[cfg(not(windows))]
async fn listeners(port: u16) -> Option<Vec<u32>> {
    let found = Command::new("lsof")
        .args(["-nP", "-t", &format!("-iTCP:{port}"), "-sTCP:LISTEN"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await;
    let Ok(found) = found else {
        tracing::debug!(port, "lsof is missing; not freeing the port");
        return None;
    };
    Some(
        String::from_utf8_lossy(&found.stdout)
            .lines()
            .filter_map(|p| p.trim().parse().ok())
            .collect(),
    )
}

/// The processes listening on TCP `port`, from `netstat -ano -p TCP`.
#[cfg(windows)]
async fn listeners(port: u16) -> Option<Vec<u32>> {
    let found = Command::new("netstat")
        .args(["-ano", "-p", "TCP"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await;
    let Ok(found) = found else {
        tracing::debug!(port, "netstat is missing; not freeing the port");
        return None;
    };
    Some(netstat_listeners(
        &String::from_utf8_lossy(&found.stdout),
        port,
    ))
}

/// Parses `netstat -ano` lines like
/// `  TCP    127.0.0.1:8765    0.0.0.0:0    LISTENING    4242`. The state
/// column is translated on some Windows languages, so a listener is told
/// apart by its remote address, `0.0.0.0:0` or `[::]:0`.
#[cfg(any(windows, test))]
fn netstat_listeners(output: &str, port: u16) -> Vec<u32> {
    let mut pids = Vec::new();
    for line in output.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let [proto, local, remote, .., pid] = cols[..] else {
            continue;
        };
        let listening = remote == "0.0.0.0:0" || remote == "[::]:0";
        let local_port = local.rsplit(':').next().and_then(|p| p.parse::<u16>().ok());
        if proto.eq_ignore_ascii_case("TCP")
            && listening
            && local_port == Some(port)
            && let Ok(pid) = pid.parse()
            && !pids.contains(&pid)
        {
            pids.push(pid);
        }
    }
    pids
}

#[cfg(test)]
mod netstat_tests {
    use super::netstat_listeners;

    #[test]
    fn finds_the_listeners_on_a_port_in_netstat_output() {
        let out = "\nActive Connections\n\n  Proto  Local Address          Foreign Address        State           PID\n  \
                   TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1000\n  \
                   TCP    127.0.0.1:8765         0.0.0.0:0              LISTENING       4242\n  \
                   TCP    127.0.0.1:8765         127.0.0.1:50000        ESTABLISHED     4242\n  \
                   TCP    127.0.0.1:50000        127.0.0.1:8765         ESTABLISHED     77\n  \
                   TCP    [::]:8765              [::]:0                 ABH\u{d6}REN       4242\n  \
                   TCP    [::1]:18765            [::]:0                 LISTENING       9\n";
        assert_eq!(netstat_listeners(out, 8765), [4242]);
        assert_eq!(netstat_listeners(out, 135), [1000]);
        assert_eq!(netstat_listeners(out, 18765), [9]);
        assert!(netstat_listeners(out, 50000).is_empty());
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A sidecar in Python: `/health` answers once `delay` seconds have
    /// passed, `/die` exits. `None` when python3 is missing.
    fn fake(port: u16, delay: f32) -> Option<SidecarConfig> {
        let python = [
            "/usr/bin/python3",
            "/opt/homebrew/bin/python3",
            "/usr/local/bin/python3",
        ]
        .into_iter()
        .find(|p| Path::new(p).is_file())?;
        let script = format!(
            r#"
import http.server, os, sys, time
start = time.time()
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        if self.path == "/die":
            os._exit(1)
        ready = time.time() - start >= {delay}
        body = ('{{"ready": %s, "pid": %d}}' % ("true" if ready else "false", os.getpid())).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
http.server.HTTPServer(("127.0.0.1", {port}), H).serve_forever()
"#
        );
        Some(SidecarConfig {
            command: python.into(),
            args: vec!["-c".into(), script],
            cwd: None,
            env: BTreeMap::new(),
            port,
            health_url: None,
            startup_secs: 30,
        })
    }

    fn free() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    async fn until(what: &str, mut check: impl FnMut() -> bool) {
        let start = Instant::now();
        while !check() {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    #[test]
    fn backoff_doubles_up_to_a_minute() {
        let secs: Vec<u64> = (1..=9).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
    }

    #[test]
    fn health_needs_200_and_a_true_ready_flag() {
        assert!(healthy(200, "ok"));
        assert!(healthy(200, r#"{"ready": true}"#));
        assert!(healthy(200, r#"{"engine": "x"}"#));
        assert!(!healthy(200, r#"{"ready": false}"#));
        assert!(!healthy(503, "loading"));
    }

    #[test]
    fn config_reads_node_toml_fields() {
        let c: SidecarConfig =
            toml_like(r#"{"command": "~/py", "args": ["a"], "port": 8765, "env": {"A": "1"}}"#);
        assert_eq!(c.health(), "http://127.0.0.1:8765/health");
        assert_eq!(c.startup_secs, 120);
        assert_eq!(c.env["A"], "1");
    }

    fn toml_like(json: &str) -> SidecarConfig {
        serde_json::from_str(json).unwrap()
    }

    #[tokio::test]
    async fn starts_waits_for_health_and_stops_with_the_node() {
        let port = free();
        let Some(config) = fake(port, 0.5) else {
            eprintln!("python3 missing; skipped");
            return;
        };
        let sidecars = Sidecars::start(BTreeMap::from([("fake".to_string(), config)]), None);
        assert_eq!(sidecars.state("fake"), Some(SidecarState::Starting));
        until("ready", || {
            sidecars.state("fake") == Some(SidecarState::Ready)
        })
        .await;
        let pid = sidecars.status()[0].pid.unwrap();
        assert!(alive(pid));
        sidecars.shutdown().await;
        let status = &sidecars.status()[0];
        assert_eq!(status.state, SidecarState::Stopped);
        assert_eq!(status.restarts, 0);
        // Reaped and the port is free again.
        assert!(!alive(pid));
        assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
    }

    #[tokio::test]
    async fn restarts_a_sidecar_that_dies() {
        let port = free();
        let Some(config) = fake(port, 0.0) else {
            eprintln!("python3 missing; skipped");
            return;
        };
        let sidecars = Sidecars::start(BTreeMap::from([("fake".to_string(), config)]), None);
        until("ready", || {
            sidecars.state("fake") == Some(SidecarState::Ready)
        })
        .await;
        let first = sidecars.status()[0].pid.unwrap();
        let _ = client::get(
            &format!("http://127.0.0.1:{port}/die"),
            Duration::from_secs(1),
        );
        until("the restart", || {
            let s = &sidecars.status()[0];
            s.restarts == 1 && s.state == SidecarState::Ready
        })
        .await;
        let status = &sidecars.status()[0];
        assert_ne!(status.pid, Some(first));
        assert!(status.error.as_deref().unwrap_or("").contains("exited"));
        sidecars.shutdown().await;
    }

    #[tokio::test]
    async fn restart_replaces_the_process() {
        let port = free();
        let Some(config) = fake(port, 0.0) else {
            eprintln!("python3 missing; skipped");
            return;
        };
        let sidecars = Sidecars::start(BTreeMap::from([("fake".to_string(), config)]), None);
        assert!(!sidecars.restart("nope"));
        until("ready", || {
            sidecars.state("fake") == Some(SidecarState::Ready)
        })
        .await;
        let first = sidecars.status()[0].pid.unwrap();
        assert!(sidecars.restart("fake"));
        until("the new process", || {
            let s = &sidecars.status()[0];
            s.state == SidecarState::Ready && s.pid.is_some_and(|p| p != first)
        })
        .await;
        assert!(!alive(first));
        assert_eq!(sidecars.status()[0].restarts, 1);
        sidecars.shutdown().await;
    }

    #[tokio::test]
    async fn reload_adds_removes_and_restarts_what_changed() {
        let (a_port, b_port) = (free(), free());
        let (Some(a), Some(b)) = (fake(a_port, 0.0), fake(b_port, 0.0)) else {
            eprintln!("python3 missing; skipped");
            return;
        };
        let sidecars = Sidecars::start(BTreeMap::from([("a".to_string(), a.clone())]), None);
        until("a ready", || {
            sidecars.state("a") == Some(SidecarState::Ready)
        })
        .await;
        let a_pid = sidecars.status()[0].pid.unwrap();

        // Unchanged a keeps its process; b is new.
        let both = BTreeMap::from([("a".to_string(), a.clone()), ("b".to_string(), b.clone())]);
        let report = sidecars.reload(both).await;
        assert_eq!(report.added, ["b"]);
        assert_eq!(report.unchanged, ["a"]);
        until("b ready", || {
            sidecars.state("b") == Some(SidecarState::Ready)
        })
        .await;
        assert_eq!(sidecars.status()[0].pid, Some(a_pid));

        // A changed a starts again; b, no longer listed, stops.
        let mut changed = a.clone();
        changed.startup_secs = 31;
        let report = sidecars
            .reload(BTreeMap::from([("a".to_string(), changed)]))
            .await;
        assert_eq!(report.restarted, ["a"]);
        assert_eq!(report.removed, ["b"]);
        assert_eq!(sidecars.state("b"), None);
        until("a ready again", || {
            sidecars.state("a") == Some(SidecarState::Ready)
                && sidecars.status()[0].pid.is_some_and(|p| p != a_pid)
        })
        .await;
        assert!(!alive(a_pid));
        assert!(std::net::TcpListener::bind(("127.0.0.1", b_port)).is_ok());

        // After shutdown a reload starts nothing.
        sidecars.shutdown().await;
        let report = sidecars
            .reload(BTreeMap::from([("b".to_string(), b)]))
            .await;
        assert!(report.added.is_empty(), "{report:?}");
        assert_eq!(sidecars.state("b"), None);
    }

    #[tokio::test]
    async fn frees_a_port_held_by_another_process() {
        let port = free();
        let (Some(squatter), Some(config)) = (fake(port, 0.0), fake(port, 0.0)) else {
            eprintln!("python3 missing; skipped");
            return;
        };
        if std::process::Command::new("lsof")
            .arg("-v")
            .output()
            .is_err()
        {
            eprintln!("lsof missing; skipped");
            return;
        }
        let mut other = spawn(&squatter, None).unwrap();
        let other_pid = other.id().unwrap();
        until("the squatter to listen", || {
            client::get(
                &format!("http://127.0.0.1:{port}/health"),
                Duration::from_millis(500),
            )
            .is_ok()
        })
        .await;
        let sidecars = Sidecars::start(BTreeMap::from([("fake".to_string(), config)]), None);
        until("ready", || {
            sidecars.state("fake") == Some(SidecarState::Ready)
        })
        .await;
        let _ = other.wait().await;
        let mine = sidecars.status()[0].pid.unwrap();
        assert_ne!(mine, other_pid);
        let body = client::get(
            &format!("http://127.0.0.1:{port}/health"),
            Duration::from_secs(1),
        )
        .unwrap()
        .body;
        assert!(body.contains(&format!("\"pid\": {mine}")), "{body}");
        sidecars.shutdown().await;
    }

    #[tokio::test]
    async fn a_missing_program_keeps_retrying_with_an_error() {
        let config = SidecarConfig {
            command: "/nonexistent/server".into(),
            args: vec![],
            cwd: None,
            env: BTreeMap::new(),
            port: free(),
            health_url: None,
            startup_secs: 1,
        };
        let sidecars = Sidecars::start(BTreeMap::from([("gone".to_string(), config)]), None);
        until("the failure", || sidecars.status()[0].restarts >= 1).await;
        let status = &sidecars.status()[0];
        assert_eq!(status.state, SidecarState::Restarting);
        assert!(status.error.as_deref().unwrap().contains("cannot start"));
        sidecars.shutdown().await;
        assert_eq!(sidecars.state("gone"), Some(SidecarState::Stopped));
    }
}
