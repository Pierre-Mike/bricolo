//! The iroh backend: an endpoint named by the node's key, with n0's DNS
//! discovery and relays, or direct addresses only.
//!
//! iroh binds every interface, the tailnet's included, so when two
//! devices share a tailnet iroh finds that path too and uses it directly.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use ::iroh::endpoint::presets;
use ::iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey};
use async_trait::async_trait;

use super::quic::{QuicEndpoint, keep_bound};
use super::{
    ConnectError, Connection, Discovered, PeerId, Transport, TransportKind, TransportStatus,
};
use crate::peers::{Peer, PeerList};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrohConfig {
    /// Use n0's relays and DNS discovery, so devices on different networks
    /// find each other. Without them, peers need `addrs` in `peers.toml`.
    pub relays: bool,
    /// UDP addresses to bind. Empty means any free port on every interface.
    pub bind: Vec<SocketAddr>,
}

impl Default for IrohConfig {
    fn default() -> Self {
        Self {
            relays: true,
            bind: Vec::new(),
        }
    }
}

impl IrohConfig {
    /// Loopback only, no relays, no discovery: for tests.
    pub fn local_only() -> Self {
        Self {
            relays: false,
            bind: vec![SocketAddr::from(([127, 0, 0, 1], 0))],
        }
    }
}

pub struct IrohTransport {
    quic: Arc<QuicEndpoint>,
    /// Binds the configured addresses again if the socket dies.
    keeper: tokio::task::JoinHandle<()>,
    relays: bool,
    /// Name and `addrs` from `peers.toml`, by key.
    hints: RwLock<HashMap<EndpointId, (String, Vec<SocketAddr>)>>,
}

impl Drop for IrohTransport {
    fn drop(&mut self) {
        self.keeper.abort();
    }
}

impl IrohTransport {
    pub async fn bind(key: SecretKey, config: IrohConfig) -> Result<Self, String> {
        let relays = config.relays;
        let builder = Box::new(move || {
            if relays {
                Endpoint::builder(presets::N0)
            } else {
                Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
            }
        });
        let quic =
            Arc::new(QuicEndpoint::bind(TransportKind::Iroh, builder, key, &config.bind).await?);
        let keeper = tokio::spawn(keep_bound(Arc::downgrade(&quic), config.bind.clone()));
        Ok(Self {
            quic,
            keeper,
            relays: config.relays,
            hints: RwLock::default(),
        })
    }

    pub fn relays(&self) -> bool {
        self.relays
    }

    /// Closes the endpoint under the transport, as a vanished bind address
    /// does, and leaves the transport to notice. For tests.
    #[doc(hidden)]
    pub async fn kill_socket(&self) {
        self.quic.kill_socket().await;
    }
}

#[async_trait]
impl Transport for IrohTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Iroh
    }

    fn set_peers(&self, peers: &PeerList) {
        self.quic.set_peers(peers);
        *self.hints.write().unwrap() = peers
            .peers
            .values()
            .map(|p| (p.id, (p.name.clone(), p.addrs.clone())))
            .collect();
    }

    fn locate(&self, peer: &Peer) -> Option<PeerId> {
        // iroh finds any key through discovery or relays; without them it
        // needs a direct address.
        (self.relays || !peer.addrs.is_empty()).then_some(PeerId::Iroh { key: peer.id })
    }

    /// iroh looks a device up by its key instead of listing a network, so
    /// this lists the trusted keys and the addresses it was given for them.
    async fn discover(&self) -> Result<Vec<Discovered>, String> {
        Ok(self
            .hints
            .read()
            .unwrap()
            .iter()
            .map(|(key, (name, addrs))| Discovered {
                id: PeerId::Iroh { key: *key },
                name: name.clone(),
                online: None,
                addrs: addrs.clone(),
            })
            .collect())
    }

    async fn connect(&self, id: &PeerId, key: EndpointId) -> Result<Connection, ConnectError> {
        let PeerId::Iroh { key: target } = id else {
            return Err(ConnectError::new(format!("iroh cannot reach {id}")));
        };
        if *target != key {
            return Err(ConnectError::new("the iroh id and the key differ"));
        }
        let addrs = self
            .hints
            .read()
            .unwrap()
            .get(&key)
            .map(|(_, a)| a.clone())
            .unwrap_or_default();
        let addr = addrs
            .iter()
            .fold(EndpointAddr::new(key), |a, ip| a.with_ip_addr(*ip));
        self.quic.connect(addr, key).await
    }

    async fn accept(&self) -> Option<Connection> {
        self.quic.accept().await
    }

    fn bound(&self) -> Vec<SocketAddr> {
        self.quic.bound()
    }

    fn status(&self) -> TransportStatus {
        TransportStatus {
            kind: TransportKind::Iroh,
            bound: self.bound().iter().map(|a| a.to_string()).collect(),
            detail: if self.relays {
                "n0 relays and DNS discovery".into()
            } else {
                "direct addresses only".into()
            },
        }
    }

    /// Resolves when the endpoint was bound again after its socket died.
    async fn changed(&self) {
        self.quic.changed().await;
    }

    async fn close(&self) {
        self.keeper.abort();
        self.quic.close().await;
    }
}
