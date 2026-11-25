//! The node's control socket, seen from the UI: `<data>/bricolo.sock`, or on
//! Windows the named pipe [`pipe_name`] of the data folder, JSON lines.
//! [`follow`] keeps one subscription open, reconnecting while the node is
//! down or restarting; [`request`] sends one request on its own
//! connection.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

/// The first wait after a failed connection, doubled up to [`MAX_RETRY`].
pub const FIRST_RETRY: Duration = Duration::from_millis(250);
pub const MAX_RETRY: Duration = Duration::from_secs(2);
/// How often a node without `subscribe` is asked for its status.
pub const POLL: Duration = Duration::from_secs(2);

/// What [`follow`] reports.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeEvent {
    /// A node answers. Events follow.
    Up,
    /// No node answers, or the connection broke. Reported once per outage.
    Down(String),
    /// One event from the node, such as `{"event":"status",...}`.
    Event(Value),
}

/// `$BRICOLO_DATA_DIR`, or the default data folder. Mirrors
/// `bricolo_runtime::Paths::from_env`, which this crate does not depend on so
/// that the UI does not link Wasmtime.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("BRICOLO_DATA_DIR") {
        return Some(dir.into());
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("bricolo"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/bricolo"))
    }
}

/// The control socket in `data`; on Windows, its named pipe.
pub fn socket(data: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(pipe_name(data))
    }
    #[cfg(not(windows))]
    {
        data.join("bricolo.sock")
    }
}

/// The node's pipe on Windows. Mirrors `bricolo_runtime::paths::pipe_name`:
/// FNV-1a over the data folder, lowercased with `\` separators; both
/// crates test the same value.
#[cfg(any(windows, test))]
pub fn pipe_name(data: &Path) -> String {
    let key = data.to_string_lossy().replace('/', "\\").to_lowercase();
    let key = key.trim_end_matches('\\');
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!(r"\\.\pipe\bricolo-{hash:016x}")
}

/// The wait before connection attempt `n` (counting from 1).
pub fn retry_delay(failures: u32) -> Duration {
    FIRST_RETRY
        .saturating_mul(1 << failures.saturating_sub(1).min(8))
        .min(MAX_RETRY)
}

#[cfg(any(unix, windows))]
mod imp {
    use std::path::Path;

    use anyhow::{Context, Result, bail};
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::{NodeEvent, POLL, retry_delay};

    #[cfg(unix)]
    async fn connect(socket: &Path) -> std::io::Result<tokio::net::UnixStream> {
        tokio::net::UnixStream::connect(socket).await
    }

    /// Opens the node's pipe, waiting up to 2 s while every instance is
    /// busy (ERROR_PIPE_BUSY).
    #[cfg(windows)]
    async fn connect(
        socket: &Path,
    ) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
        use tokio::net::windows::named_pipe::ClientOptions;
        const ERROR_PIPE_BUSY: i32 = 231;
        let mut tries = 0;
        loop {
            match ClientOptions::new().open(socket) {
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tries < 40 => {
                    tries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                other => return other,
            }
        }
    }

    pub async fn request(socket: &Path, request: &Value) -> Result<Value> {
        let stream = connect(socket).await?;
        let (read, mut write) = tokio::io::split(stream);
        write.write_all(format!("{request}\n").as_bytes()).await?;
        let line = BufReader::new(read)
            .lines()
            .next_line()
            .await?
            .context("the node closed the connection")?;
        Ok(serde_json::from_str(&line)?)
    }

    /// One connection: subscribes, or polls a node that cannot. Returns why
    /// it ended.
    async fn session(socket: &Path, on: &mut impl FnMut(NodeEvent)) -> Result<()> {
        let stream = connect(socket).await?;
        let (read, mut write) = tokio::io::split(stream);
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"{\"cmd\":\"subscribe\"}\n").await?;
        let Some(first) = lines.next_line().await? else {
            bail!("the node closed the connection");
        };
        let first: Value = serde_json::from_str(&first)?;
        on(NodeEvent::Up);
        if first.get("error").is_some() {
            // An older node: ask for the status on the same connection.
            loop {
                write.write_all(b"{\"cmd\":\"status\"}\n").await?;
                let line = lines.next_line().await?.context("the node went away")?;
                let status: Value = serde_json::from_str(&line)?;
                on(NodeEvent::Event(
                    json!({ "event": "status", "status": status }),
                ));
                tokio::time::sleep(POLL).await;
            }
        }
        on(NodeEvent::Event(first));
        while let Some(line) = lines.next_line().await? {
            match serde_json::from_str(&line) {
                Ok(event) => on(NodeEvent::Event(event)),
                Err(e) => bail!("bad line from the node: {e}"),
            }
        }
        bail!("the node closed the connection")
    }

    pub async fn follow(socket: &Path, mut on: impl FnMut(NodeEvent)) {
        // None until the first attempt, so the first outage is reported.
        let mut up: Option<bool> = None;
        let mut failures = 0;
        loop {
            let mut seen = false;
            let mut wrapped = |e: NodeEvent| {
                if e == NodeEvent::Up {
                    seen = true;
                }
                on(e)
            };
            let ended = session(socket, &mut wrapped).await;
            if seen {
                up = Some(true);
                failures = 0;
            }
            if up != Some(false) {
                let why = match ended {
                    Ok(()) => "the node closed the connection".to_string(),
                    Err(e) => format!("{e:#}"),
                };
                on(NodeEvent::Down(why));
                up = Some(false);
            }
            failures += 1;
            tokio::time::sleep(retry_delay(failures)).await;
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::path::Path;

    use anyhow::{Result, bail};
    use serde_json::Value;

    use super::NodeEvent;

    pub async fn request(_: &Path, _: &Value) -> Result<Value> {
        bail!("the control socket needs Unix or Windows")
    }

    pub async fn follow(_: &Path, mut on: impl FnMut(NodeEvent)) {
        on(NodeEvent::Down(
            "the control socket needs Unix or Windows".into(),
        ));
        std::future::pending::<()>().await;
    }
}

/// Sends one request and returns the answer.
pub async fn request(socket: &Path, request: &Value) -> anyhow::Result<Value> {
    imp::request(socket, request).await
}

/// Follows the node at `socket` forever: [`NodeEvent::Up`], its events,
/// [`NodeEvent::Down`] when it goes away, and again when it comes back.
pub async fn follow(socket: &Path, on: impl FnMut(NodeEvent)) {
    imp::follow(socket, on).await
}

/// The requests the hub may send. Anything else from the webview is
/// refused, so a bug in the page cannot reach `call`.
pub const ALLOWED: [&str; 15] = [
    "status",
    "reload",
    "reload-script",
    "enable",
    "disable",
    "restart-sidecar",
    "ui-action",
    "fire",
    "set-hotkey",
    // The Library view: read the stdlib and grants, ask macOS for one
    // grant, open its System Settings pane.
    "library",
    "permissions",
    "open-settings",
    // An app's Settings tab: read [settings], write [plugins.<name>] in
    // node.toml.
    "settings",
    "set-settings",
    // The mesh view's Connect buttons: add the MCP gateway to Claude.
    "mcp-connect",
];

/// Checks that `request` is one the hub may send.
pub fn allowed(request: &Value) -> Result<(), Value> {
    match request["cmd"].as_str() {
        Some(cmd) if ALLOWED.contains(&cmd) => Ok(()),
        _ => Err(json!({ "error": format!("the hub does not send {}", request["cmd"]) })),
    }
}

#[cfg(test)]
mod pure_tests {
    use super::*;

    #[test]
    fn the_pipe_name_matches_the_nodes() {
        // The same value as bricolo_runtime::paths' test.
        let name = pipe_name(Path::new(r"C:\Users\Ada\AppData\Local\bricolo"));
        assert_eq!(name, r"\\.\pipe\bricolo-9857ab219b873b74");
        assert_eq!(
            pipe_name(Path::new("c:/users/ada/appdata/local/bricolo/")),
            name
        );
    }

    #[test]
    fn retry_delay_doubles_up_to_two_seconds() {
        let ms: Vec<u128> = (1..=6).map(|n| retry_delay(n).as_millis()).collect();
        assert_eq!(ms, [250, 500, 1000, 2000, 2000, 2000]);
    }

    #[test]
    fn only_hub_requests_pass() {
        assert!(allowed(&json!({ "cmd": "disable", "plugin": "p" })).is_ok());
        assert!(allowed(&json!({ "cmd": "library" })).is_ok());
        assert!(allowed(&json!({ "cmd": "open-settings", "grant": "camera" })).is_ok());
        assert!(allowed(&json!({ "cmd": "set-settings", "plugin": "p", "values": {} })).is_ok());
        assert!(allowed(&json!({ "cmd": "call", "capability": "disk" })).is_err());
        assert!(allowed(&json!({ "plugin": "p" })).is_err());
    }
}

/// Against a fake node on a Unix socket.
#[cfg(all(test, unix))]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    use super::*;

    type Seen = Arc<Mutex<Vec<NodeEvent>>>;

    fn recorder() -> (Seen, impl FnMut(NodeEvent) + Send + 'static) {
        let seen: Seen = Arc::default();
        let s = seen.clone();
        (seen, move |e| s.lock().unwrap().push(e))
    }

    async fn until(seen: &Seen, what: &str, check: impl Fn(&[NodeEvent]) -> bool) {
        let start = std::time::Instant::now();
        while !check(&seen.lock().unwrap()) {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "timed out waiting for {what}: {:?}",
                seen.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// A node that answers `subscribe` with `events`, then hangs up.
    async fn fake_node(listener: UnixListener, events: Vec<Value>) {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        assert_eq!(line, r#"{"cmd":"subscribe"}"#);
        for e in events {
            write.write_all(format!("{e}\n").as_bytes()).await.unwrap();
        }
    }

    #[tokio::test]
    async fn reports_down_once_then_up_and_down_again_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let path = socket(dir.path());
        let (seen, on) = recorder();
        let p = path.clone();
        let task = tokio::spawn(async move { follow(&p, on).await });
        // No node yet: one Down, however many attempts fail.
        until(&seen, "the first down", |s| s.len() == 1).await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(matches!(seen.lock().unwrap()[..], [NodeEvent::Down(_)]));

        // The node starts, streams one event and stops.
        let listener = UnixListener::bind(&path).unwrap();
        fake_node(
            listener,
            vec![json!({ "event": "status", "status": { "pid": 1 } })],
        )
        .await;
        std::fs::remove_file(&path).unwrap();
        until(&seen, "up, the event, down", |s| s.len() == 4).await;
        {
            let s = seen.lock().unwrap();
            assert_eq!(s[1], NodeEvent::Up);
            assert_eq!(
                s[2],
                NodeEvent::Event(json!({ "event": "status", "status": { "pid": 1 } }))
            );
            assert!(matches!(s[3], NodeEvent::Down(_)));
        }

        // It comes back: the client reconnects by itself.
        let listener = UnixListener::bind(&path).unwrap();
        fake_node(listener, vec![json!({ "event": "ui", "ui": {} })]).await;
        until(&seen, "the second session", |s| {
            s.len() >= 6 && s[4] == NodeEvent::Up
        })
        .await;
        task.abort();
    }

    #[tokio::test]
    async fn polls_a_node_without_subscribe() {
        let dir = tempfile::tempdir().unwrap();
        let path = socket(dir.path());
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let answer = if line.contains("subscribe") {
                    json!({ "error": "bad request: unknown variant `subscribe`" })
                } else {
                    json!({ "running": true, "pid": 7 })
                };
                write
                    .write_all(format!("{answer}\n").as_bytes())
                    .await
                    .unwrap();
            }
        });
        let (seen, on) = recorder();
        let task = tokio::spawn(async move { follow(&path, on).await });
        until(&seen, "a polled status", |s| {
            s.iter().any(|e| {
                *e == NodeEvent::Event(
                    json!({ "event": "status", "status": { "running": true, "pid": 7 } }),
                )
            })
        })
        .await;
        assert_eq!(seen.lock().unwrap()[0], NodeEvent::Up);
        task.abort();
    }

    #[tokio::test]
    async fn request_gets_one_answer() {
        let dir = tempfile::tempdir().unwrap();
        let path = socket(dir.path());
        assert!(request(&path, &json!({ "cmd": "status" })).await.is_err());
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let line = BufReader::new(read)
                .lines()
                .next_line()
                .await
                .unwrap()
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            let answer = json!({ "echo": request["cmd"] });
            write
                .write_all(format!("{answer}\n").as_bytes())
                .await
                .unwrap();
        });
        let answer = request(&path, &json!({ "cmd": "status" })).await.unwrap();
        assert_eq!(answer, json!({ "echo": "status" }));
    }
}
