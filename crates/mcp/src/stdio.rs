//! The stdio transport: one JSON-RPC message per line in, one per line
//! out. Requests run concurrently, so a slow call does not hold up a ping.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::Server;

/// Serves `server` until `input` closes, then waits for calls in flight.
pub async fn serve<R, W>(server: Arc<Server>, input: R, mut output: W) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let mut notices = server.notifications();
    let forward = {
        let tx = tx.clone();
        tokio::spawn(async move {
            loop {
                match notices.recv().await {
                    Ok(note) => {
                        if tx.send(note).is_err() {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return,
                }
            }
        })
    };
    let writer = tokio::spawn(async move {
        while let Some(answer) = rx.recv().await {
            let mut line = answer.to_string();
            line.push('\n');
            output.write_all(line.as_bytes()).await?;
            output.flush().await?;
        }
        Ok::<_, std::io::Error>(())
    });

    let mut lines = BufReader::new(input).lines();
    let mut tasks = tokio::task::JoinSet::new();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                let _ = tx.send(json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {e}") },
                }));
                continue;
            }
        };
        let server = server.clone();
        let tx = tx.clone();
        tasks.spawn(async move {
            if let Some(answer) = server.handle(message).await {
                let _ = tx.send(answer);
            }
        });
    }
    while tasks.join_next().await.is_some() {}
    forward.abort();
    drop(tx);
    writer.await.map_err(std::io::Error::other)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::tests::server;

    #[tokio::test]
    async fn answers_each_request_line() {
        let (s, _) = server();
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n\nnot json\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"laptop_clipboard_get"}}"#,
            "\n",
        );
        let (out, mut read) = tokio::io::duplex(1 << 20);
        serve(s, input.as_bytes(), out).await.unwrap();
        let mut text = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut read, &mut text)
            .await
            .unwrap();
        let answers: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(answers.len(), 3, "{text}");
        let by_id = |id: Value| answers.iter().find(|a| a["id"] == id).unwrap();
        assert_eq!(by_id(json!(1))["result"]["serverInfo"]["name"], "bricolo");
        assert_eq!(by_id(json!(2))["result"]["content"][0]["text"], "copied");
        assert_eq!(by_id(Value::Null)["error"]["code"], -32700);
    }
}
