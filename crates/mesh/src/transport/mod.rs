//! How bytes reach a peer. The mesh (calls, media streams, the allow
//! lists) sits on [`Transport`] and [`Conn`] and never learns which
//! network carried a connection.
//!
//! A transport has two jobs:
//!
//! - Discovery: [`Transport::discover`] lists the devices it can see, each
//!   with a [`PeerId`] and a display name.
//! - Connections: [`Transport::connect`] opens an authenticated connection
//!   to a [`PeerId`], and [`Transport::accept`] hands over the ones peers
//!   open. Both check the remote key against `peers.toml` before any
//!   stream opens.
//!
//! Every transport authenticates the same identity, the node's
//! `node.key`, so `peers.toml` names a peer once whatever carries it.
//! Two backends exist:
//!
//! - [`iroh`]: an iroh endpoint with n0's discovery and relays.
//! - [`tailscale`]: QUIC straight to the peer's tailnet IP on a fixed port,
//!   with peers found through `tailscale status`. Tailscale only gives
//!   reachability; the QUIC handshake proves each side holds its key.
//!
//! A connection carries reliable bidirectional streams (calls, files,
//! media) and unreliable datagrams (real-time input, audio, video). A
//! backend without datagrams returns `None` from
//! [`Conn::max_datagram_size`]; callers then send on a stream instead.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use ::iroh::EndpointId;
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::future::BoxFuture;
use serde::Serialize;

use crate::peers::{Peer, PeerList};

pub mod iroh;
mod quic;
pub mod tailscale;

pub(crate) use quic::REFUSED_REASON;

/// Which backend carries a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportKind {
    Iroh,
    Tailscale,
}

impl TransportKind {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "iroh" => Some(Self::Iroh),
            "tailscale" => Some(Self::Tailscale),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Iroh => "iroh",
            Self::Tailscale => "tailscale",
        }
    }
}

impl fmt::Display for TransportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A transport's stable name for a device. Never an address that may
/// change: iroh names a device by its key, Tailscale by its machine name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum PeerId {
    Iroh { key: EndpointId },
    Tailscale { host: String },
}

impl PeerId {
    pub fn kind(&self) -> TransportKind {
        match self {
            Self::Iroh { .. } => TransportKind::Iroh,
            Self::Tailscale { .. } => TransportKind::Tailscale,
        }
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Iroh { key } => write!(f, "iroh:{key}"),
            Self::Tailscale { host } => write!(f, "tailscale:{host}"),
        }
    }
}

/// One device a transport can see.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Discovered {
    pub id: PeerId,
    pub name: String,
    /// `None` when the transport cannot tell.
    pub online: Option<bool>,
    /// Addresses the transport would dial.
    pub addrs: Vec<SocketAddr>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TransportStatus {
    pub kind: TransportKind,
    pub bound: Vec<String>,
    /// One line on how the transport finds peers.
    pub detail: String,
}

/// Close codes the mesh uses. A backend maps them to its own.
pub mod close {
    /// The remote key is not in `peers.toml`.
    pub const REFUSED: u32 = 1;
    /// The node is shutting down.
    pub const SHUTDOWN: u32 = 2;
    /// `peers.toml` changed and the connection is no longer wanted.
    pub const REPLACED: u32 = 3;
    /// Reset code for a stream dropped before it finished.
    pub const DROPPED: u32 = 0;
}

/// Why a connection ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Closed {
    /// The peer closed it with a code from [`close`] and a reason.
    ByPeer { code: u32, reason: String },
    /// This node closed it.
    Locally,
    /// Timed out, reset, or the network went away.
    Lost(String),
}

impl Closed {
    /// True when the peer refused this node: it is not in their
    /// `peers.toml`.
    pub fn is_refused(&self) -> bool {
        matches!(self, Self::ByPeer { code, .. } if *code == close::REFUSED)
    }
}

impl fmt::Display for Closed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ByPeer { code, reason } => write!(f, "closed by peer (code {code}): {reason}"),
            Self::Locally => f.write_str("closed by this node"),
            Self::Lost(why) => f.write_str(why),
        }
    }
}

/// Why a connection attempt failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectError {
    pub message: String,
    /// The peer refused this node's key.
    pub refused: bool,
}

impl ConnectError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            refused: false,
        }
    }
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Why a stream read stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// The other side finished after this many bytes of the requested read.
    FinishedEarly(usize),
    /// Reset, connection lost, or anything else.
    Broken(String),
}

/// The sending half of a reliable stream.
#[async_trait]
pub trait SendHalf: Send + Sync {
    async fn write_all(&mut self, buf: &[u8]) -> Result<(), String>;
    /// No more bytes follow; the peer reads to the end.
    fn finish(&mut self);
    /// Abandons the stream; the peer sees it break.
    fn reset(&mut self, code: u32);
    /// Resolves when the peer stops reading or the connection ends.
    fn stopped(&self) -> BoxFuture<'static, ()>;
}

/// The receiving half of a reliable stream.
#[async_trait]
pub trait RecvHalf: Send + Sync {
    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ReadError>;
}

pub type SendStream = Box<dyn SendHalf>;
pub type RecvStream = Box<dyn RecvHalf>;

/// One authenticated connection to a peer.
#[async_trait]
pub trait Conn: Send + Sync {
    /// The key the peer proved it holds during the handshake.
    fn remote_key(&self) -> EndpointId;
    fn transport(&self) -> TransportKind;
    /// Unique among this node's live connections.
    fn stable_id(&self) -> usize;

    async fn open_bi(&self) -> Result<(SendStream, RecvStream), Closed>;
    async fn accept_bi(&self) -> Result<(SendStream, RecvStream), Closed>;

    /// Largest datagram the path takes now, or `None` when this connection
    /// has no datagrams.
    fn max_datagram_size(&self) -> Option<usize>;
    /// Sends one unreliable datagram. It may be lost or reordered.
    fn send_datagram(&self, data: Bytes) -> Result<(), String>;
    async fn read_datagram(&self) -> Result<Bytes, Closed>;

    fn close(&self, code: u32, reason: &[u8]);
    async fn closed(&self) -> Closed;
    fn close_reason(&self) -> Option<Closed>;
}

pub type Connection = Arc<dyn Conn>;

/// A network that carries connections between nodes.
#[async_trait]
pub trait Transport: Send + Sync {
    fn kind(&self) -> TransportKind;

    /// The trusted peers. Only their keys get past the handshake. A
    /// backend may also read its own hints, such as `addrs` or `tailnet`.
    fn set_peers(&self, peers: &PeerList);

    /// This transport's id for a trusted peer, or `None` when it has no way
    /// to reach it.
    fn locate(&self, peer: &Peer) -> Option<PeerId>;

    /// The devices this transport can see now.
    async fn discover(&self) -> Result<Vec<Discovered>, String>;

    /// Opens a connection to `id` and checks that it holds `key`.
    async fn connect(&self, id: &PeerId, key: EndpointId) -> Result<Connection, ConnectError>;

    /// The next incoming connection from a trusted key, or `None` once the
    /// transport is closed.
    async fn accept(&self) -> Option<Connection>;

    /// Resolves when the network this transport sees has changed, such as
    /// a machine joining the tailnet, so offline peers are tried again at
    /// once. The default never resolves.
    async fn changed(&self) {
        std::future::pending::<()>().await
    }

    /// Local socket addresses.
    fn bound(&self) -> Vec<SocketAddr>;

    fn status(&self) -> TransportStatus;

    async fn close(&self);
}
