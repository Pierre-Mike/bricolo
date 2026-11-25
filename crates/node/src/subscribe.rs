//! `{"cmd":"subscribe"}`: the connection becomes a stream of JSON lines,
//! one event each, so the UI never polls.
//!
//! - `{"event":"status","status":{...}}`: what `status` answers. Sent at
//!   once, then whenever it changes (`uptime_s` aside). The node checks
//!   after each request that changes something, when a sensor turns on or
//!   off, and every second, which catches plugins crashing and sidecars
//!   changing state.
//! - `{"event":"ui","ui":{"tiles":{...},"panels":[...]}}`: every tile and
//!   panel. Sent at once, then after each change.
//! - `{"event":"logs","lines":[...]}`: the recent log lines, once, then
//!   `{"event":"log","line":{"ts_ms":..,"plugin":..,"level":..,"message":..}}`
//!   for each new line. `{"event":"lagged","skipped":n}` says the client
//!   read too slowly and lost `n` lines.

use std::time::Duration;

use bricolo_providers::log::feed;
use serde_json::{Value, json};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::broadcast::error::RecvError;

use crate::control::Node;

/// How often the stream checks the status for changes nothing announced.
const TICK: Duration = Duration::from_secs(1);

async fn send<W: AsyncWrite + Unpin>(out: &mut W, event: Value) -> std::io::Result<()> {
    let mut line = event.to_string();
    line.push('\n');
    out.write_all(line.as_bytes()).await?;
    out.flush().await
}

/// The status without the fields that change on their own.
fn comparable(status: &Value) -> Value {
    let mut status = status.clone();
    if let Some(map) = status.as_object_mut() {
        map.remove("uptime_s");
    }
    status
}

/// Writes events to `out` until writing fails.
pub async fn stream<W: AsyncWrite + Unpin>(node: &Node, out: &mut W) -> std::io::Result<()> {
    let providers = node.runtime.local_providers();
    let (recent, mut logs) = feed().subscribe();
    let mut ui = providers.ui.subscribe();
    let mut sensors = providers.sensors.subscribe();
    let mut changed = node.changed.subscribe();
    let mut last = node.status().await;
    send(out, json!({ "event": "status", "status": last })).await?;
    ui.mark_unchanged();
    send(out, json!({ "event": "ui", "ui": providers.ui.snapshot() })).await?;
    send(out, json!({ "event": "logs", "lines": recent })).await?;
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            r = changed.changed() => if r.is_err() { return Ok(()) },
            r = sensors.changed() => if r.is_err() { return Ok(()) },
            r = ui.changed() => {
                if r.is_err() {
                    return Ok(());
                }
                ui.mark_unchanged();
                send(out, json!({ "event": "ui", "ui": providers.ui.snapshot() })).await?;
                continue;
            }
            line = logs.recv() => {
                match line {
                    Ok(line) => send(out, json!({ "event": "log", "line": line })).await?,
                    Err(RecvError::Lagged(n)) => {
                        send(out, json!({ "event": "lagged", "skipped": n })).await?
                    }
                    Err(RecvError::Closed) => return Ok(()),
                }
                continue;
            }
        }
        let now = node.status().await;
        if comparable(&now) != comparable(&last) {
            send(out, json!({ "event": "status", "status": now })).await?;
            last = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use bricolo_providers::Caller;
    use bricolo_providers::log::Level;
    use bricolo_providers::ui::{Panel, PanelKind};
    use bricolo_runtime::Paths;
    use tokio::io::{AsyncBufReadExt, BufReader};

    use super::*;
    use crate::control::{NodeOptions, handle};

    /// Reads events until one matches, or fails after 5 s.
    async fn until<R: tokio::io::AsyncBufRead + Unpin>(
        lines: &mut tokio::io::Lines<R>,
        what: &str,
        check: impl Fn(&Value) -> bool,
    ) -> Value {
        let start = Instant::now();
        loop {
            let left = Duration::from_secs(5).saturating_sub(start.elapsed());
            let line = tokio::time::timeout(left, lines.next_line())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .unwrap()
                .unwrap();
            let event: Value = serde_json::from_str(&line).unwrap();
            if check(&event) {
                return event;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn streams_status_ui_and_logs() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("config"), dir.path().join("data"));
        std::fs::create_dir_all(paths.scripts_dir()).unwrap();
        std::fs::write(
            paths.scripts_dir().join("tick.lua"),
            "bricolo.log.info('up')",
        )
        .unwrap();
        let node = Node::start(&paths, NodeOptions::default()).await.unwrap();
        let (client, mut server) = tokio::io::duplex(1 << 16);
        let mut lines = BufReader::new(client).lines();
        let streaming = async {
            let _ = stream(&node, &mut server).await;
        };
        let checks = async {
            let first = until(&mut lines, "status", |e| e["event"] == "status").await;
            assert_eq!(first["status"]["scripts"][0]["state"], "running");
            assert_eq!(first["status"]["active_sensors"], json!([]));
            until(&mut lines, "ui", |e| e["event"] == "ui").await;
            let logs = until(&mut lines, "logs", |e| e["event"] == "logs").await;
            assert!(logs["lines"].is_array());

            // A new log line arrives on its own.
            node.runtime
                .local_providers()
                .log
                .write("tick.lua", Level::Info, "streamed");
            until(&mut lines, "the log line", |e| {
                e["event"] == "log" && e["line"]["message"] == "streamed"
            })
            .await;

            // Disabling a script sends the new status at once.
            let answer = handle(&node, r#"{"cmd":"disable","script":"tick"}"#).await;
            assert_eq!(answer["status"]["state"], "disabled", "{answer}");
            until(&mut lines, "the disabled script", |e| {
                e["event"] == "status" && e["status"]["scripts"][0]["state"] == "disabled"
            })
            .await;

            // A panel and a sensor.
            let ui = &node.runtime.local_providers().ui;
            let panel = Panel {
                kind: PanelKind::Caption,
                title: String::new(),
                text: "hello".into(),
                choices: vec![],
                timeout_ms: None,
            };
            ui.show_panel(&Caller::new("p"), panel).unwrap();
            let event = until(&mut lines, "the panel", |e| e["event"] == "ui").await;
            assert_eq!(event["ui"]["panels"][0]["text"], "hello");
            assert_eq!(event["ui"]["panels"][0]["kind"], "caption");
            let answer = handle(
                &node,
                r#"{"cmd":"sensor","plugin":"p","sensor":"mic","active":true}"#,
            )
            .await;
            assert_eq!(answer["active_sensors"][0]["sensor"], "mic", "{answer}");
            until(&mut lines, "the sensor", |e| {
                e["event"] == "status" && e["status"]["active_sensors"][0]["plugin"] == "p"
            })
            .await;
        };
        tokio::select! {
            _ = streaming => panic!("the stream ended"),
            _ = checks => {}
        }
        node.shutdown().await;
    }

    /// A real `mic` recording, through a fake ffmpeg, turns the `mic`
    /// sensor on in `status` and in the stream, and stopping it turns it
    /// off: the same list the hub and menu bar draw the dot from.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_recording_shows_in_active_sensors() {
        use std::os::unix::fs::PermissionsExt;

        use bricolo_providers::mic::RecordOptions;

        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("config"), dir.path().join("data"));
        let fake = dir.path().join("ffmpeg");
        std::fs::write(
            &fake,
            "#!/bin/sh\ntrap 'exit 255' INT\nfor a in \"$@\"; do out=\"$a\"; done\n\
             { printf 'RIFF\\0\\0\\0\\0WAVEdata\\0\\0\\0\\0'; head -c 3200 /dev/zero; } > \"$out\"\n\
             while :; do sleep 0.1; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(
            paths.node_toml(),
            format!("[mic]\nffmpeg = {:?}\n", fake.display().to_string()),
        )
        .unwrap();
        let node = Node::start(&paths, NodeOptions::default()).await.unwrap();
        let (client, mut server) = tokio::io::duplex(1 << 16);
        let mut lines = BufReader::new(client).lines();
        let streaming = async {
            let _ = stream(&node, &mut server).await;
        };
        let checks = async {
            let first = until(&mut lines, "status", |e| e["event"] == "status").await;
            assert_eq!(first["status"]["active_sensors"], json!([]));

            let mic = &node.runtime.local_providers().mic;
            let caller = Caller::new("dict.lua");
            let id = mic
                .record_start(&caller, RecordOptions::default())
                .await
                .unwrap();
            let on = json!([{ "plugin": "dict.lua", "sensor": "mic" }]);
            until(&mut lines, "the mic turning on", |e| {
                e["event"] == "status" && e["status"]["active_sensors"] == on
            })
            .await;
            assert_eq!(node.status().await["active_sensors"], on);

            mic.record_stop(&caller, id).await.unwrap();
            until(&mut lines, "the mic turning off", |e| {
                e["event"] == "status" && e["status"]["active_sensors"] == json!([])
            })
            .await;
            assert_eq!(node.status().await["active_sensors"], json!([]));
        };
        tokio::select! {
            _ = streaming => panic!("the stream ended"),
            _ = checks => {}
        }
        node.shutdown().await;
    }
}
