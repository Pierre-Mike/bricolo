//! The Streamable HTTP transport, for agents that reach the gateway over
//! the network, like Claude through a Tailscale Funnel custom connector.
//!
//! - `POST /mcp` takes one JSON-RPC message. A request gets its answer as
//!   `application/json`; a notification or response gets `202`.
//! - `GET /mcp` with `Accept: text/event-stream` opens a stream of the
//!   server's own messages, such as `notifications/tools/list_changed`.
//! - `GET /health` answers `ok` without a token.
//!
//! Every `/mcp` request needs the token: `Authorization: Bearer <token>`,
//! or the path `/mcp/<token>` for clients that cannot set headers. A
//! request with an `Origin` header outside the allowed list is refused, so
//! a web page in a browser on this machine cannot drive the gateway.
//! There is no TLS: bind to loopback and let the tunnel terminate it.

use std::convert::Infallible;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream;
use http_body_util::{BodyExt, Full, Limited, StreamBody, combinators::UnsyncBoxBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{
    ACCEPT, AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, HeaderValue, ORIGIN, WWW_AUTHENTICATE,
};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::broadcast::error::RecvError;

use crate::Server;

/// Largest request body the gateway reads.
const MAX_BODY: usize = 4 << 20;

/// How often an idle event stream sends a comment, so proxies keep it.
const KEEPALIVE: Duration = Duration::from_secs(25);

type Body = UnsyncBoxBody<Bytes, Infallible>;

/// Who may use the HTTP gateway.
#[derive(Clone, Debug)]
pub struct Access {
    /// The secret every `/mcp` request must carry.
    pub token: String,
    /// `Origin` values allowed, like `https://claude.ai`. A request with
    /// no `Origin` header is fine; one with another origin is refused.
    pub origins: Vec<String>,
}

/// A new random token: 32 bytes as hex.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS has no random source");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The token in `path`, made and saved (readable only by this user) when
/// the file does not exist yet.
pub fn load_or_create_token(path: &Path) -> io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => return Ok(text.trim().to_string()),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let token = new_token();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    io::Write::write_all(&mut options.open(path)?, format!("{token}\n").as_bytes())?;
    Ok(token)
}

/// Serves `server` on `listener` until the task is dropped.
pub async fn serve(server: Arc<Server>, listener: TcpListener, access: Access) -> io::Result<()> {
    let access = Arc::new(access);
    loop {
        let (stream, peer) = listener.accept().await?;
        let server = server.clone();
        let access = access.clone();
        tokio::spawn(async move {
            let service = service_fn(move |req| {
                let server = server.clone();
                let access = access.clone();
                async move { Ok::<_, Infallible>(route(&server, &access, req).await) }
            });
            if let Err(e) = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                tracing::debug!(%peer, "MCP HTTP connection ended: {e}");
            }
        });
    }
}

async fn route(server: &Arc<Server>, access: &Access, req: Request<Incoming>) -> Response<Body> {
    let path = req.uri().path().trim_end_matches('/').to_string();
    if path == "/health" && req.method() == Method::GET {
        return text(StatusCode::OK, "ok");
    }
    let path_token = match path.as_str() {
        "/mcp" => None,
        p => match p.strip_prefix("/mcp/") {
            Some(t) if !t.contains('/') => Some(t.to_string()),
            _ => return text(StatusCode::NOT_FOUND, "not found"),
        },
    };
    if let Some(origin) = req.headers().get(ORIGIN) {
        let ok = origin
            .to_str()
            .is_ok_and(|o| access.origins.iter().any(|a| a == o));
        if !ok {
            tracing::warn!(?origin, "refused an MCP request from a web origin");
            return text(StatusCode::FORBIDDEN, "origin not allowed");
        }
    }
    let bearer = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    let token = bearer.or(path_token.as_deref()).unwrap_or_default();
    if !same(token.as_bytes(), access.token.as_bytes()) {
        tracing::warn!("refused an MCP request without the right token");
        let mut answer = text(StatusCode::UNAUTHORIZED, "missing or wrong token");
        answer
            .headers_mut()
            .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return answer;
    }

    match *req.method() {
        Method::POST => post(server, req).await,
        Method::GET => {
            let wants_events = req
                .headers()
                .get(ACCEPT)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("text/event-stream"));
            if wants_events {
                events(server)
            } else {
                text(
                    StatusCode::METHOD_NOT_ALLOWED,
                    "GET needs Accept: text/event-stream",
                )
            }
        }
        // Stateless: there is no session to end.
        Method::DELETE => text(StatusCode::METHOD_NOT_ALLOWED, "no sessions"),
        _ => text(StatusCode::METHOD_NOT_ALLOWED, "use POST"),
    }
}

async fn post(server: &Server, req: Request<Incoming>) -> Response<Body> {
    let bytes = match Limited::new(req.into_body(), MAX_BODY).collect().await {
        Ok(body) => body.to_bytes(),
        Err(_) => return text(StatusCode::PAYLOAD_TOO_LARGE, "body too large"),
    };
    let message: Value = match serde_json::from_slice(&bytes) {
        Ok(Value::Array(_)) => {
            return rpc_error(StatusCode::BAD_REQUEST, -32600, "batches are not supported");
        }
        Ok(m) => m,
        Err(e) => {
            return rpc_error(
                StatusCode::BAD_REQUEST,
                -32700,
                &format!("parse error: {e}"),
            );
        }
    };
    match server.handle(message).await {
        Some(answer) => json_response(StatusCode::OK, &answer),
        None => empty(StatusCode::ACCEPTED),
    }
}

/// A server-sent event stream of the server's notifications.
fn events(server: &Arc<Server>) -> Response<Body> {
    let notices = server.notifications();
    let stream = stream::unfold(notices, |mut notices| async move {
        let frame = tokio::select! {
            got = notices.recv() => match got {
                Ok(note) => format!("event: message\ndata: {note}\n\n"),
                Err(RecvError::Lagged(_)) => ": lagged\n\n".into(),
                Err(RecvError::Closed) => return None,
            },
            () = tokio::time::sleep(KEEPALIVE) => ": keepalive\n\n".into(),
        };
        Some((
            Ok::<_, Infallible>(Frame::data(Bytes::from(frame))),
            notices,
        ))
    });
    let mut answer = Response::new(UnsyncBoxBody::new(StreamBody::new(stream.boxed())));
    let h = answer.headers_mut();
    h.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    h.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    answer
}

/// Compares in time that does not depend on where the bytes differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn full(bytes: impl Into<Bytes>) -> Body {
    UnsyncBoxBody::new(Full::new(bytes.into()))
}

fn empty(status: StatusCode) -> Response<Body> {
    let mut answer = Response::new(full(Bytes::new()));
    *answer.status_mut() = status;
    answer
}

fn text(status: StatusCode, body: &'static str) -> Response<Body> {
    let mut answer = Response::new(full(body));
    *answer.status_mut() = status;
    answer
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    answer
}

fn json_response(status: StatusCode, value: &Value) -> Response<Body> {
    let mut answer = Response::new(full(value.to_string()));
    *answer.status_mut() = status;
    answer
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    answer
}

fn rpc_error(status: StatusCode, code: i64, message: &str) -> Response<Body> {
    json_response(
        status,
        &json!({ "jsonrpc": "2.0", "id": null, "error": { "code": code, "message": message } }),
    )
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::*;
    use crate::server::tests::server;

    const TOKEN: &str = "s3cret";

    async fn start() -> (SocketAddr, Arc<Server>) {
        let (s, _) = server();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let access = Access {
            token: TOKEN.into(),
            origins: vec!["https://claude.ai".into()],
        };
        tokio::spawn(serve(s.clone(), listener, access));
        (addr, s)
    }

    /// Sends one HTTP/1.1 request and returns the status and body.
    async fn send(
        addr: SocketAddr,
        method: &str,
        path: &str,
        headers: &[&str],
        body: &str,
    ) -> (u16, String) {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        for h in headers {
            req.push_str(h);
            req.push_str("\r\n");
        }
        req.push_str("\r\n");
        req.push_str(body);
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).await.unwrap();
        let status = out[9..12].parse().unwrap();
        let body = out
            .split_once("\r\n\r\n")
            .map(|(_, b)| b.to_string())
            .unwrap_or_default();
        (status, body)
    }

    const PING: &str = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;

    #[tokio::test]
    async fn needs_the_token_in_a_header_or_the_path() {
        let (addr, _) = start().await;
        assert_eq!(send(addr, "POST", "/mcp", &[], PING).await.0, 401);
        assert_eq!(
            send(addr, "POST", "/mcp", &["Authorization: Bearer nope"], PING)
                .await
                .0,
            401
        );
        assert_eq!(send(addr, "POST", "/mcp/s3cre", &[], PING).await.0, 401);

        let (status, body) = send(
            addr,
            "POST",
            "/mcp",
            &["Authorization: Bearer s3cret"],
            PING,
        )
        .await;
        assert_eq!(status, 200);
        assert!(body.contains(r#""result":{}"#), "{body}");
        assert_eq!(send(addr, "POST", "/mcp/s3cret", &[], PING).await.0, 200);
        assert_eq!(
            send(addr, "GET", "/health", &[], "").await,
            (200, "ok".into())
        );
        assert_eq!(send(addr, "POST", "/other", &[], PING).await.0, 404);
    }

    #[tokio::test]
    async fn refuses_foreign_origins() {
        let (addr, _) = start().await;
        let auth = "Authorization: Bearer s3cret";
        assert_eq!(
            send(
                addr,
                "POST",
                "/mcp",
                &[auth, "Origin: https://evil.example"],
                PING
            )
            .await
            .0,
            403
        );
        assert_eq!(
            send(
                addr,
                "POST",
                "/mcp",
                &[auth, "Origin: https://claude.ai"],
                PING
            )
            .await
            .0,
            200
        );
    }

    #[tokio::test]
    async fn answers_requests_and_accepts_notifications() {
        let (addr, _) = start().await;
        let auth = "Authorization: Bearer s3cret";
        let call = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"laptop_clipboard_get","arguments":{}}}"#;
        let (status, body) = send(addr, "POST", "/mcp", &[auth], call).await;
        assert_eq!(status, 200);
        let answer: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(answer["result"]["content"][0]["text"], "copied");

        let note = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert_eq!(send(addr, "POST", "/mcp", &[auth], note).await.0, 202);
        assert_eq!(send(addr, "POST", "/mcp", &[auth], "[]").await.0, 400);
        assert_eq!(send(addr, "POST", "/mcp", &[auth], "{").await.0, 400);
        assert_eq!(send(addr, "GET", "/mcp", &[auth], "").await.0, 405);
    }

    #[tokio::test]
    async fn streams_notifications_as_events() {
        let (addr, s) = start().await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /mcp HTTP/1.1\r\nHost: x\r\nAccept: text/event-stream\r\nAuthorization: Bearer s3cret\r\n\r\n")
            .await
            .unwrap();
        // Wait until the stream subscribed, then send a notice.
        let mut seen = String::new();
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        assert!(seen.contains("text/event-stream"), "{seen}");
        s.notices_for_test(
            json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" }),
        );
        while !seen.contains("list_changed") {
            let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf))
                .await
                .unwrap()
                .unwrap();
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        assert!(seen.contains("data: {"), "{seen}");
    }

    #[test]
    fn tokens_are_random_and_saved_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/mcp.token");
        let a = load_or_create_token(&path).unwrap();
        assert_eq!(a.len(), 64);
        assert_eq!(load_or_create_token(&path).unwrap(), a);
        assert_ne!(new_token(), a);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(same(b"abc", b"abc") && !same(b"abc", b"abd") && !same(b"ab", b"abc"));
    }
}
