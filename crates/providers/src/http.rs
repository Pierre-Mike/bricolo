//! `http`: requests to the `host:port` pairs a plugin lists under `[http]
//! allow` in `plugin.toml`. Requests run on Tokio's blocking pool. Redirects
//! are not followed, so a server cannot send the plugin to a host outside
//! its list; the plugin sees the 3xx response instead.
//!
//! The crate-private [`client`] helpers serve the sidecar clients (`tts`,
//! `speech`) with the same agent setup.

use std::time::Duration;

use async_trait::async_trait;
use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

use crate::caller::Caller;
use crate::error::CallError;

/// Bytes of response body kept; a longer body fails the call.
pub const BODY_CAP: u64 = 8 << 20;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_TIMEOUT: Duration = Duration::from_secs(300);

/// The lowercase scheme and `host:port` of `url`, with the scheme's default
/// port filled in. Userinfo is refused, since `a:1@b` hides the real host
/// from a reader.
pub fn authority(url: &str) -> Result<(String, String), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("{url:?} is not an absolute URL"))?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return Err(format!("{url:?}: only http and https are allowed")),
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let auth = &rest[..end];
    if auth.is_empty() || auth.contains('@') {
        return Err(format!("{url:?} has no plain host"));
    }
    let (host, port) = if let Some(v6) = auth.strip_prefix('[') {
        let (host, after) = v6
            .split_once(']')
            .ok_or_else(|| format!("{url:?} has a broken IPv6 host"))?;
        (format!("[{host}]"), after.strip_prefix(':'))
    } else {
        match auth.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), Some(port)),
            None => (auth.to_string(), None),
        }
    };
    let port = match port {
        Some(p) => p
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or_else(|| format!("{url:?} has a bad port"))?,
        None => default_port,
    };
    if host.is_empty() || host == "[]" {
        return Err(format!("{url:?} has no host"));
    }
    Ok((scheme, format!("{}:{port}", host.to_ascii_lowercase())))
}

/// An allow entry is a lowercase `host:port` with an explicit port.
pub fn valid_allow_entry(entry: &str) -> bool {
    authority(&format!("http://{entry}/")).is_ok_and(|(_, a)| a == entry)
}

/// The allow entry that stands for every host. Only trusted callers (Lua
/// scripts) get it: `plugin.toml` entries must be `host:port`.
pub const ANY: &str = "*";

/// `Denied` unless `url`'s `host:port` is in `allow`.
pub fn check_url(allow: &[String], url: &str) -> Result<(), CallError> {
    let (_, auth) = authority(url).map_err(CallError::Failed)?;
    if allow.iter().any(|a| *a == auth || a == ANY) {
        Ok(())
    } else {
        Err(CallError::Denied)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// `None` means [`DEFAULT_TIMEOUT`]; capped at [`MAX_TIMEOUT`].
    pub timeout: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[async_trait]
pub trait HttpProvider: Send + Sync {
    /// Any status is a response; only transport problems are errors.
    async fn send(&self, caller: &Caller, request: Request) -> Result<Response, CallError>;
}

/// Requests from this node.
pub struct LocalHttp;

#[async_trait]
impl HttpProvider for LocalHttp {
    async fn send(&self, caller: &Caller, request: Request) -> Result<Response, CallError> {
        check_url(&caller.http, &request.url)?;
        let timeout = request.timeout.unwrap_or(DEFAULT_TIMEOUT).min(MAX_TIMEOUT);
        tokio::task::spawn_blocking(move || send_blocking(request, timeout))
            .await
            .map_err(CallError::failed)?
    }
}

fn send_blocking(request: Request, timeout: Duration) -> Result<Response, CallError> {
    let mut builder = ureq::http::Request::builder()
        .method(request.method.to_ascii_uppercase().as_str())
        .uri(&request.url);
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    let agent = client::agent(timeout);
    let result = if request.body.is_empty() {
        builder.body(()).map(|r| agent.run(r))
    } else {
        builder.body(request.body).map(|r| agent.run(r))
    };
    let mut response = result
        .map_err(|e| CallError::Failed(format!("bad request: {e}")))?
        .map_err(|e| client::call_error(e, false))?;
    let headers = response
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    let body = response
        .body_mut()
        .with_config()
        .limit(BODY_CAP)
        .read_to_vec()
        .map_err(|e| client::call_error(e, false))?;
    Ok(Response {
        status: response.status().as_u16(),
        headers,
        body,
    })
}

/// The HTTP client the sidecar providers share.
pub(crate) mod client {
    use super::*;

    pub fn agent(timeout: Duration) -> Agent {
        let tls = TlsConfig::builder()
            .provider(TlsProvider::Rustls)
            .root_certs(RootCerts::PlatformVerifier)
            .build();
        Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .tls_config(tls)
            .build()
            .into()
    }

    /// Maps a transport error. With `local`, a refused connection means the
    /// sidecar is down: `Unavailable`, so the router tries the next target.
    pub fn call_error(e: ureq::Error, local: bool) -> CallError {
        match e {
            ureq::Error::Timeout(_) => CallError::Timeout,
            ureq::Error::ConnectionFailed | ureq::Error::HostNotFound if local => {
                CallError::Unavailable
            }
            ureq::Error::Io(io) if local && io.kind() == std::io::ErrorKind::ConnectionRefused => {
                CallError::Unavailable
            }
            other => CallError::failed(other),
        }
    }

    /// A text response from a sidecar: status and body.
    pub struct Text {
        pub status: u16,
        pub body: String,
    }

    /// GETs `url` and reads the body as text.
    pub fn get(url: &str, timeout: Duration) -> Result<Text, CallError> {
        let mut response = agent(timeout)
            .get(url)
            .call()
            .map_err(|e| call_error(e, true))?;
        read(&mut response)
    }

    /// POSTs `body` as UTF-8 text with extra `headers`.
    pub fn post(
        url: &str,
        headers: &[(&str, &str)],
        body: &str,
        timeout: Duration,
    ) -> Result<Text, CallError> {
        let mut request = agent(timeout)
            .post(url)
            .header("Content-Type", "text/plain; charset=utf-8");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut response = request.send(body).map_err(|e| call_error(e, true))?;
        read(&mut response)
    }

    fn read(response: &mut ureq::http::Response<ureq::Body>) -> Result<Text, CallError> {
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(BODY_CAP)
            .read_to_string()
            .map_err(|e| call_error(e, true))?;
        Ok(Text { status, body })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// One request as the fake server saw it.
    #[derive(Clone, Debug, Default)]
    pub struct Seen {
        pub method: String,
        pub path: String,
        pub headers: Vec<(String, String)>,
        pub body: String,
    }

    /// A tiny HTTP/1.1 server on a free port. `answer` maps each request to
    /// a status and body. Returns the port and the requests it saw.
    pub fn fake_server(
        answer: impl Fn(&Seen) -> (u16, String) + Send + 'static,
    ) -> (u16, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let mut req = Seen {
                    method: parts.next().unwrap_or_default().to_string(),
                    path: parts.next().unwrap_or_default().to_string(),
                    ..Seen::default()
                };
                let mut length = 0;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.trim().split_once(':') {
                        if k.eq_ignore_ascii_case("content-length") {
                            length = v.trim().parse().unwrap_or(0);
                        }
                        req.headers
                            .push((k.to_ascii_lowercase(), v.trim().to_string()));
                    }
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                req.body = String::from_utf8_lossy(&body).into_owned();
                let (status, text) = answer(&req);
                log.lock().unwrap().push(req);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nX-Fake: yes\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
            }
        });
        (port, seen)
    }

    fn caller(allow: &[&str]) -> Caller {
        Caller {
            http: allow.iter().map(|s| s.to_string()).collect(),
            ..Caller::new("p")
        }
    }

    #[test]
    fn authority_fills_default_ports_and_refuses_userinfo() {
        assert_eq!(
            authority("https://Example.com/x").unwrap(),
            ("https".into(), "example.com:443".into())
        );
        assert_eq!(
            authority("http://127.0.0.1:8765/health").unwrap().1,
            "127.0.0.1:8765"
        );
        assert_eq!(authority("http://[::1]:80/").unwrap().1, "[::1]:80");
        assert!(authority("http://a:b@evil.com/").is_err());
        assert!(authority("ftp://x/").is_err());
        assert!(authority("example.com").is_err());
        assert!(authority("http://x:0/").is_err());
    }

    #[test]
    fn allow_entries_need_an_explicit_lowercase_port() {
        assert!(valid_allow_entry("api.example.com:443"));
        assert!(valid_allow_entry("127.0.0.1:8765"));
        assert!(!valid_allow_entry("api.example.com"));
        assert!(!valid_allow_entry("API.example.com:443"));
        assert!(!valid_allow_entry("http://a:1"));
        // Only trusted callers hold `*`; plugin.toml cannot list it.
        assert!(!valid_allow_entry(ANY));
        assert!(check_url(&[ANY.into()], "https://example.com/x").is_ok());
        assert!(check_url(&[ANY.into()], "file:///etc/hosts").is_err());
    }

    #[tokio::test]
    async fn requests_outside_the_allow_list_are_denied() {
        let (port, seen) = fake_server(|_| (200, "hi".into()));
        let req = |url: String| Request {
            method: "GET".into(),
            url,
            headers: vec![],
            body: vec![],
            timeout: None,
        };
        let c = caller(&["127.0.0.1:1"]);
        let denied = LocalHttp
            .send(&c, req(format!("http://127.0.0.1:{port}/")))
            .await;
        assert_eq!(denied, Err(CallError::Denied));
        // localhost is a different host than 127.0.0.1 for the list.
        let c = caller(&[&format!("127.0.0.1:{port}")]);
        let denied = LocalHttp
            .send(&c, req(format!("http://localhost:{port}/")))
            .await;
        assert_eq!(denied, Err(CallError::Denied));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn allowed_request_returns_status_headers_and_body() {
        let (port, seen) = fake_server(|r| {
            if r.path == "/missing" {
                (404, "nope".into())
            } else {
                (200, format!("got {}", r.body))
            }
        });
        let c = caller(&[&format!("127.0.0.1:{port}")]);
        let response = LocalHttp
            .send(
                &c,
                Request {
                    method: "post".into(),
                    url: format!("http://127.0.0.1:{port}/echo"),
                    headers: vec![("X-Test".into(), "1".into())],
                    body: b"ping".to_vec(),
                    timeout: Some(Duration::from_secs(5)),
                },
            )
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"got ping");
        assert!(
            response
                .headers
                .iter()
                .any(|(k, v)| k == "x-fake" && v == "yes")
        );
        let req = seen.lock().unwrap()[0].clone();
        assert_eq!(req.method, "POST");
        assert!(req.headers.iter().any(|(k, v)| k == "x-test" && v == "1"));
        // A 404 is a response, not an error.
        let response = LocalHttp
            .send(
                &c,
                Request {
                    method: "GET".into(),
                    url: format!("http://127.0.0.1:{port}/missing"),
                    headers: vec![],
                    body: vec![],
                    timeout: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(response.status, 404);
    }

    #[test]
    fn a_closed_local_port_is_unavailable() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let err = client::get(
            &format!("http://127.0.0.1:{port}/health"),
            // Windows retries a refused loopback connect for about 2 s.
            Duration::from_secs(10),
        )
        .err();
        assert_eq!(err, Some(CallError::Unavailable));
    }
}
