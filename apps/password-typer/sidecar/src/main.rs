//! touch-id: asks for Touch ID on behalf of the password-typer plugin.
//!
//! WASM plugins can't reach LocalAuthentication, so this small server on
//! 127.0.0.1 opens the system Touch ID prompt and answers whether the
//! owner of the Mac confirmed. It never sees a password: the plugin keeps
//! those in the Keychain through the `secrets` capability.
//!
//!   GET  /health        {"ready": true}
//!   POST /authenticate  the body is the reason macOS shows, as in
//!                       "touch-id is trying to <reason>". Answers
//!                       200 "ok" once you confirm, 401 when you cancel or
//!                       the finger is not recognized, 409 while another
//!                       prompt is open, 408 when nobody answers in time,
//!                       503 when Touch ID can't run.
//!
//! Settings come from the environment (`bricolo sidecar add ... --env K=V`):
//!   SIDECAR_PORT             default 8804
//!   TOUCH_ID_POLICY          "owner" (default): Touch ID, with the Mac
//!                            password as a fallback, which also works with
//!                            the lid closed. "biometrics": Touch ID only.
//!   TOUCH_ID_TIMEOUT_SECS    default 50; the prompt closes after that.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A reason is short text; anything longer is a mistake.
const MAX_BODY: usize = 4096;
const MAX_REASON: usize = 200;
const DEFAULT_REASON: &str = "type a saved password";

/// Which way of proving you are the owner macOS accepts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Policy {
    /// Touch ID, or the Mac password.
    Owner,
    /// Touch ID only.
    Biometrics,
}

impl Policy {
    fn from_env(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("biometrics") => Policy::Biometrics,
            _ => Policy::Owner,
        }
    }
}

/// How one prompt ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Confirmed,
    /// An `LAError` code and macOS's description of it.
    Refused(isize, String),
    TimedOut,
}

/// A status, a content type and a body.
type Reply = (u16, &'static str, String);

const TEXT: &str = "text/plain; charset=utf-8";

/// The answer for an outcome. The codes are `LAError` values.
pub fn reply_for(outcome: &Outcome) -> Reply {
    match outcome {
        Outcome::Confirmed => (200, TEXT, "ok\n".into()),
        Outcome::TimedOut => (408, TEXT, "nobody answered the Touch ID prompt\n".into()),
        Outcome::Refused(code, message) => match code {
            // UserCancel, UserFallback, SystemCancel, AppCancel
            -2 | -3 | -4 | -9 => (401, TEXT, "cancelled\n".into()),
            // AuthenticationFailed
            -1 => (401, TEXT, "not recognized\n".into()),
            // BiometryLockout
            -8 => (
                503,
                TEXT,
                "Touch ID is locked after too many tries; unlock it with your Mac password\n"
                    .into(),
            ),
            // PasscodeNotSet, BiometryNotAvailable, BiometryNotEnrolled, NotInteractive
            -5 | -6 | -7 | -1004 => (503, TEXT, format!("Touch ID can't run: {message}\n")),
            _ => (500, TEXT, format!("Touch ID failed ({code}): {message}\n")),
        },
    }
}

/// The reason to show: the body on one line, cut to 200 characters.
pub fn reason_from(body: &[u8]) -> Result<String, Reply> {
    let text = std::str::from_utf8(body).map_err(|_| (400, TEXT, "the body is not UTF-8\n".into()))?;
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return Ok(DEFAULT_REASON.into());
    }
    Ok(flat.chars().take(MAX_REASON).collect())
}

/// Answers one request. `authenticate` opens the prompt; tests pass a fake.
fn route(
    method: &str,
    path: &str,
    body: &[u8],
    busy: &AtomicBool,
    authenticate: &dyn Fn(&str) -> Outcome,
) -> Reply {
    match (method, path) {
        ("GET", "/health") => (200, "application/json", "{\"ready\": true}".into()),
        ("POST", "/authenticate") => {
            let reason = match reason_from(body) {
                Ok(reason) => reason,
                Err(reply) => return reply,
            };
            if busy
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return (409, TEXT, "a Touch ID prompt is already open\n".into());
            }
            let outcome = authenticate(&reason);
            busy.store(false, Ordering::SeqCst);
            reply_for(&outcome)
        }
        _ => (404, TEXT, "no such endpoint\n".into()),
    }
}

#[cfg(target_os = "macos")]
mod touch_id {
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LAContext, LAPolicy};

    use super::{Outcome, Policy};

    fn refused(error: &NSError) -> Outcome {
        Outcome::Refused(error.code(), error.localizedDescription().to_string())
    }

    /// Shows the system prompt and waits for it to close, at most
    /// `timeout`; then it closes the prompt itself.
    pub fn authenticate(reason: &str, policy: Policy, timeout: Duration) -> Outcome {
        let policy = match policy {
            Policy::Owner => LAPolicy::DeviceOwnerAuthentication,
            Policy::Biometrics => LAPolicy::DeviceOwnerAuthenticationWithBiometrics,
        };
        // SAFETY: plain Objective-C calls on an LAContext this function owns.
        let context = unsafe { LAContext::new() };
        if let Err(error) = unsafe { context.canEvaluatePolicy_error(policy) } {
            return refused(&error);
        }
        let (sender, receiver) = mpsc::channel();
        let reply = RcBlock::new(move |ok: Bool, error: *mut NSError| {
            let outcome = if ok.as_bool() {
                Outcome::Confirmed
            } else {
                // SAFETY: macOS passes a valid NSError, or null, for the
                // length of the call.
                match unsafe { error.as_ref() } {
                    Some(error) => refused(error),
                    None => Outcome::Refused(-1, "no reason given".into()),
                }
            };
            let _ = sender.send(outcome);
        });
        let reason = NSString::from_str(reason);
        // SAFETY: macOS copies the block and calls it once, on its own queue.
        unsafe { context.evaluatePolicy_localizedReason_reply(policy, &reason, &reply) };
        match receiver.recv_timeout(timeout) {
            Ok(outcome) => outcome,
            Err(_) => {
                // SAFETY: invalidate may be called from any thread; it
                // closes the prompt and the reply block gets AppCancel.
                unsafe { context.invalidate() };
                Outcome::TimedOut
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod touch_id {
    use std::time::Duration;

    use super::{Outcome, Policy};

    pub fn authenticate(_: &str, _: Policy, _: Duration) -> Outcome {
        Outcome::Refused(-6, "Touch ID exists only on macOS".into())
    }
}

/// Reads one request: the method, the path and the body.
fn read_request(stream: &TcpStream) -> Result<(String, String, Vec<u8>), Reply> {
    let bad = |why: &str| (400, TEXT, format!("{why}\n"));
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|_| bad("no request line"))?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Err(bad("bad request line"));
    };
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).map_err(|_| bad("bad header"))?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().map_err(|_| bad("bad Content-Length"))?;
        }
    }
    if length > MAX_BODY {
        return Err((413, TEXT, "the body is larger than 4 KiB\n".into()));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).map_err(|_| bad("short body"))?;
    Ok((method.to_string(), path.to_string(), body))
}

fn serve(
    mut stream: TcpStream,
    busy: &AtomicBool,
    authenticate: &dyn Fn(&str) -> Outcome,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let (status, content_type, body) = match read_request(&stream) {
        Ok((method, path, body)) => {
            let reply = route(&method, &path, &body, busy, authenticate);
            // stderr goes to <data>/logs/sidecar-touch-id.log under the node.
            if path != "/health" {
                eprintln!("{method} {path} {} {}", reply.0, reply.2.trim_end());
            }
            reply
        }
        Err(reply) => reply,
    };
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

static BUSY: AtomicBool = AtomicBool::new(false);

fn main() -> std::io::Result<()> {
    let port: u16 = std::env::var("SIDECAR_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8804);
    let policy = Policy::from_env(std::env::var("TOUCH_ID_POLICY").ok().as_deref());
    let timeout = Duration::from_secs(
        std::env::var("TOUCH_ID_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(50),
    );
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    println!("touch-id listening on http://127.0.0.1:{port}, policy {policy:?}");
    for stream in listener.incoming().flatten() {
        std::thread::spawn(move || {
            serve(stream, &BUSY, &|reason| {
                touch_id::authenticate(reason, policy, timeout)
            })
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn confirm(_: &str) -> Outcome {
        Outcome::Confirmed
    }

    #[test]
    fn health_is_ready_at_once() {
        let busy = AtomicBool::new(false);
        let reply = route("GET", "/health", b"", &busy, &confirm);
        assert_eq!(reply.2, "{\"ready\": true}");
    }

    #[test]
    fn authenticate_passes_the_reason_and_answers_ok() {
        let busy = AtomicBool::new(false);
        let seen = std::sync::Mutex::new(String::new());
        let reply = route("POST", "/authenticate", b"type the\n password \"work\"", &busy, &|r| {
            *seen.lock().unwrap() = r.to_string();
            Outcome::Confirmed
        });
        assert_eq!((reply.0, reply.2.as_str()), (200, "ok\n"));
        assert_eq!(*seen.lock().unwrap(), "type the password \"work\"");
        assert!(!busy.load(Ordering::SeqCst), "busy is cleared after the prompt");
    }

    #[test]
    fn empty_and_long_reasons() {
        assert_eq!(reason_from(b"  ").unwrap(), DEFAULT_REASON);
        assert_eq!(reason_from("x".repeat(500).as_bytes()).unwrap().len(), MAX_REASON);
        assert_eq!(reason_from(&[0xff]).unwrap_err().0, 400);
    }

    #[test]
    fn one_prompt_at_a_time() {
        let busy = AtomicBool::new(true);
        let reply = route("POST", "/authenticate", b"x", &busy, &|_| {
            panic!("no second prompt while one is open")
        });
        assert_eq!(reply.0, 409);
    }

    #[test]
    fn maps_refusals_to_statuses() {
        let refused = |code| reply_for(&Outcome::Refused(code, "why".into())).0;
        assert_eq!(refused(-2), 401);
        assert_eq!(refused(-4), 401);
        assert_eq!(refused(-1), 401);
        assert_eq!(refused(-6), 503);
        assert_eq!(refused(-7), 503);
        assert_eq!(refused(-8), 503);
        assert_eq!(refused(-42), 500);
        assert_eq!(reply_for(&Outcome::TimedOut).0, 408);
        assert!(reply_for(&Outcome::Refused(-7, "No fingers".into())).2.contains("No fingers"));
    }

    #[test]
    fn policy_defaults_to_owner() {
        assert_eq!(Policy::from_env(None), Policy::Owner);
        assert_eq!(Policy::from_env(Some("biometrics")), Policy::Biometrics);
        assert_eq!(Policy::from_env(Some("nonsense")), Policy::Owner);
    }

    #[test]
    fn serves_over_tcp() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let busy = AtomicBool::new(false);
            serve(listener.accept().unwrap().0, &busy, &|_| {
                Outcome::Refused(-2, "Canceled by user.".into())
            });
        });
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .write_all(b"POST /authenticate HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\nok")
            .unwrap();
        let mut answer = String::new();
        client.read_to_string(&mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.1 401 Unauthorized\r\n"), "{answer}");
        assert!(answer.ends_with("\r\n\r\ncancelled\n"), "{answer}");
    }
}
