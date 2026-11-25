//! Unreliable messages for one capability, such as real-time input or a
//! frame of audio that is worthless once late.
//!
//! A client sends with [`Mesh::send_datagram`](crate::Mesh::send_datagram).
//! The peer runs the [`DatagramHandler`] registered for that capability
//! with [`Mesh::register_datagrams`](crate::Mesh::register_datagrams), if
//! the sender's allow list holds the capability.
//!
//! On the wire a datagram is one byte with the capability's length, the
//! capability, then the payload. When the connection has no datagrams, or
//! the payload does not fit in one, the message goes on its own stream
//! instead ([`Kind::Datagram`](crate::wire::Kind::Datagram)): it then
//! arrives, late perhaps, but whole.

use bytes::{BufMut, Bytes, BytesMut};

/// Receives one capability's datagrams on this node.
pub trait DatagramHandler: Send + Sync {
    /// `peer` is the sender's name in `peers.toml`. Runs on the
    /// connection's reader, so hand long work to a task.
    fn receive(&self, peer: &str, payload: Bytes);
}

/// The longest capability name a datagram can carry.
pub const MAX_CAPABILITY: usize = u8::MAX as usize;

pub(crate) fn encode(capability: &str, payload: &[u8]) -> Option<Bytes> {
    let len = u8::try_from(capability.len()).ok()?;
    let mut out = BytesMut::with_capacity(1 + capability.len() + payload.len());
    out.put_u8(len);
    out.put_slice(capability.as_bytes());
    out.put_slice(payload);
    Some(out.freeze())
}

pub(crate) fn decode(datagram: Bytes) -> Option<(String, Bytes)> {
    let len = *datagram.first()? as usize;
    let name = datagram.get(1..1 + len)?;
    let name = std::str::from_utf8(name).ok()?.to_string();
    Some((name, datagram.slice(1 + len..)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_garbage() {
        let d = encode("pointer", b"xy").unwrap();
        assert_eq!(
            decode(d),
            Some(("pointer".into(), Bytes::from_static(b"xy")))
        );
        let empty = encode("a", b"").unwrap();
        assert_eq!(decode(empty), Some(("a".into(), Bytes::new())));
        assert_eq!(decode(Bytes::new()), None);
        assert_eq!(decode(Bytes::from_static(&[5, b'a'])), None);
        assert_eq!(decode(Bytes::from_static(&[1, 0xff])), None);
        assert!(encode(&"x".repeat(256), b"").is_none());
    }
}
