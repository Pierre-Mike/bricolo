//! What travels on one stream of a [`Conn`](crate::transport::Conn): length-prefixed frames, each holding
//! one `postcard` value.
//!
//! A call: the client sends a [`Header`] frame and an arguments frame, then
//! finishes its side. The server answers with one [`Response`] frame. A
//! media stream: the client sends a [`Header`] frame, the server answers
//! with a [`Response`] frame (empty on success), and both sides then send
//! raw byte frames until they finish. A ping: the client sends a
//! [`Header`] frame and the server answers with an empty [`Response`]. A
//! datagram that did not fit: a [`Header`] frame and the payload frame,
//! with no answer.

use bricolo_providers::CallError;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::transport::{ReadError, RecvStream, SendStream};

/// The ALPN both nodes must speak. Bump it when the wire format changes.
pub const ALPN: &[u8] = b"bricolo/1";

/// Largest call header, arguments or result frame (16 MiB, enough for a
/// few minutes of audio).
pub const MAX_CALL_FRAME: usize = 16 << 20;
/// Largest media frame (1 MiB).
pub const MAX_MEDIA_FRAME: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// One request, one response.
    Call,
    /// A long-lived stream of frames in both directions.
    Stream,
    /// Checks that the peer accepted this node. Sent once per connection.
    Ping,
    /// One datagram's payload, on a stream because the connection has no
    /// datagrams or the payload did not fit. No answer.
    Datagram,
}

/// The first frame on every stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    /// Unique per client process; shows up in both nodes' logs.
    pub id: u64,
    pub kind: Kind,
    pub capability: String,
    pub method: String,
    /// Milliseconds the server may spend before it answers `timeout`.
    /// Relative, because the two clocks may disagree. 0 means none.
    pub deadline_ms: u64,
    /// The calling plugin on the client node, for logs.
    pub plugin: String,
}

/// `call-error` on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireError {
    Unavailable,
    Timeout,
    Denied,
    Failed(String),
}

impl From<CallError> for WireError {
    fn from(e: CallError) -> Self {
        match e {
            CallError::Unavailable => Self::Unavailable,
            CallError::Timeout => Self::Timeout,
            CallError::Denied => Self::Denied,
            CallError::Failed(m) => Self::Failed(m),
        }
    }
}

impl From<WireError> for CallError {
    fn from(e: WireError) -> Self {
        match e {
            WireError::Unavailable => Self::Unavailable,
            WireError::Timeout => Self::Timeout,
            WireError::Denied => Self::Denied,
            WireError::Failed(m) => Self::Failed(m),
        }
    }
}

/// The server's answer: the encoded result, or why there is none.
pub type Response = Result<Vec<u8>, WireError>;

/// How a capability method's arguments and result are encoded. The mesh
/// uses [`Postcard`]; `bricolo call` uses [`Json`].
pub trait Codec {
    fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CallError>;
    fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CallError>;
}

pub struct Postcard;

impl Codec for Postcard {
    fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CallError> {
        postcard::from_bytes(bytes).map_err(|e| CallError::Failed(format!("bad message: {e}")))
    }
    fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CallError> {
        postcard::to_allocvec(value).map_err(|e| CallError::Failed(format!("cannot encode: {e}")))
    }
}

/// Arguments are a JSON array, one element per argument.
pub struct Json;

impl Codec for Json {
    fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CallError> {
        serde_json::from_slice(bytes).or_else(|e| {
            // A method without arguments decodes `()`, which JSON spells
            // `null`; accept the empty array too.
            if bytes.trim_ascii() == b"[]" {
                serde_json::from_slice(b"null").map_err(|_| bad_json(e))
            } else {
                Err(bad_json(e))
            }
        })
    }
    fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CallError> {
        serde_json::to_vec(value).map_err(CallError::failed)
    }
}

fn bad_json(e: serde_json::Error) -> CallError {
    CallError::Failed(format!("bad arguments: {e}"))
}

/// Why a frame could not be read.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadFailure {
    /// The other side finished its stream cleanly before the frame began.
    End,
    /// The stream broke: reset, connection lost, or a frame cut short.
    Broken(String),
    TooLarge(usize),
}

pub async fn write_frame(send: &mut SendStream, bytes: &[u8]) -> Result<(), String> {
    let len = u32::try_from(bytes.len()).map_err(|_| "frame too large".to_string())?;
    send.write_all(&len.to_be_bytes()).await?;
    send.write_all(bytes).await
}

pub async fn read_frame(recv: &mut RecvStream, max: usize) -> Result<Vec<u8>, ReadFailure> {
    let mut len = [0u8; 4];
    match recv.read_exact(&mut len).await {
        Ok(()) => {}
        Err(ReadError::FinishedEarly(0)) => return Err(ReadFailure::End),
        Err(ReadError::FinishedEarly(n)) => {
            return Err(ReadFailure::Broken(format!(
                "frame length cut short after {n} bytes"
            )));
        }
        Err(ReadError::Broken(e)) => return Err(ReadFailure::Broken(e)),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        return Err(ReadFailure::TooLarge(len));
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf)
        .await
        .map_err(|e| ReadFailure::Broken(format!("{e:?}")))?;
    Ok(buf)
}

/// Writes one `postcard` value as a frame.
pub async fn send_value<T: Serialize>(send: &mut SendStream, value: &T) -> Result<(), String> {
    let bytes = postcard::to_allocvec(value).map_err(|e| e.to_string())?;
    write_frame(send, &bytes).await
}

/// Reads one frame and decodes it as a `postcard` value.
pub async fn recv_value<T: DeserializeOwned>(
    recv: &mut RecvStream,
    max: usize,
) -> Result<T, ReadFailure> {
    let bytes = read_frame(recv, max).await?;
    postcard::from_bytes(&bytes).map_err(|e| ReadFailure::Broken(format!("bad message: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: T) {
        let bytes = Postcard::encode(&value).unwrap();
        assert_eq!(Postcard::decode::<T>(&bytes).unwrap(), value);
    }

    #[test]
    fn header_and_responses_round_trip() {
        round_trip(Header {
            id: 7,
            kind: Kind::Call,
            capability: "tts".into(),
            method: "speak".into(),
            deadline_ms: 30_000,
            plugin: "voice-inbox".into(),
        });
        round_trip(Header {
            id: 0,
            kind: Kind::Ping,
            capability: String::new(),
            method: String::new(),
            deadline_ms: 0,
            plugin: String::new(),
        });
        round_trip(Header {
            id: u64::MAX,
            kind: Kind::Stream,
            capability: "mic".into(),
            method: "feed".into(),
            deadline_ms: 0,
            plugin: String::new(),
        });
        let responses: [Response; 5] = [
            Ok(vec![1, 2, 3]),
            Err(WireError::Unavailable),
            Err(WireError::Timeout),
            Err(WireError::Denied),
            Err(WireError::Failed("disk full".into())),
        ];
        for r in responses {
            round_trip(r);
        }
    }

    #[test]
    fn argument_tuples_round_trip() {
        round_trip(("hello".to_string(), Some("alba".to_string())));
        round_trip(("hello".to_string(), None::<String>));
        round_trip((vec![0u8, 255, 7], "wav".to_string()));
        round_trip(());
        round_trip(Some("clip".to_string()));
        round_trip(vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn call_errors_survive_the_wire() {
        for e in [
            CallError::Unavailable,
            CallError::Timeout,
            CallError::Denied,
            CallError::Failed("boom".into()),
        ] {
            let wire = WireError::from(e.clone());
            let bytes = Postcard::encode(&wire).unwrap();
            let back: WireError = Postcard::decode(&bytes).unwrap();
            assert_eq!(CallError::from(back), e);
        }
    }

    #[test]
    fn garbage_is_a_failed_call() {
        let err = Postcard::decode::<Header>(&[0xff, 0xff]).unwrap_err();
        assert!(matches!(err, CallError::Failed(m) if m.starts_with("bad message")));
    }

    #[test]
    fn json_arguments_accept_an_empty_array_for_no_arguments() {
        Json::decode::<()>(b"[]").unwrap();
        Json::decode::<()>(b"null").unwrap();
        let (path,): (String,) = Json::decode(br#"["~/a.txt"]"#).unwrap();
        assert_eq!(path, "~/a.txt");
        assert!(Json::decode::<(String,)>(b"[1]").is_err());
    }
}
