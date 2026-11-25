//! The device mesh: the trusted peers from `peers.toml`, and a small RPC
//! that runs provider calls on another node, over iroh, Tailscale or both.
//!
//! - Transports: [`transport`] defines the [`Transport`] trait and its
//!   iroh and Tailscale backends. Nothing above it knows which one carries
//!   a connection.
//! - Identity: `node.key` holds this node's secret key ([`identity`]).
//!   Its public half, the endpoint id, names the node to its peers.
//! - Trust: only keys in `peers.toml` get past the handshake, and each peer
//!   may call only the capabilities in its allow list ([`peers`]).
//! - Calls: each call is one bidirectional stream on ALPN `bricolo/1`
//!   ([`wire`]). Dropping the call resets the stream, which drops the
//!   provider's future on the peer.
//! - Routing: [`Mesh`] implements the router's `Peers` trait, so
//!   `rules.toml` targets like `["desktop", "local"]` reach the peer.
//! - Capabilities: [`capabilities`] holds the table of remote methods.
//! - Media: [`media`] carries long-lived streams of byte frames, and
//!   [`datagram`] unreliable messages.
//! - Sharing: [`sharing`] fetches an installed plugin from a peer by hash.
//!
//! See docs/mesh.md for pairing and the security model.

pub mod capabilities;
mod client;
pub mod datagram;
mod error;
pub mod identity;
pub mod media;
mod mesh;
pub mod peers;
pub mod sharing;
pub mod transport;
pub mod wire;

pub use datagram::DatagramHandler;
pub use error::MeshError;
pub use iroh::{EndpointId, SecretKey};
pub use media::{FrameReceiver, FrameSender, MediaStream, StreamHandler};
pub use mesh::{LinkState, Mesh, MeshConfig, MeshStatus, PeerStatus, TransportConfig};
pub use peers::{Peer, PeerList};
pub use sharing::PluginStore;
pub use transport::iroh::IrohConfig;
pub use transport::tailscale::{StatusSource, TailscaleConfig};
pub use transport::{Transport, TransportKind};
