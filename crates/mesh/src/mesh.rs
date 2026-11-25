//! One connection supervisor per peer, over whichever transports
//! `node.toml` turns on, and the server that answers peers' calls.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bricolo_providers::clipboard::ClipboardProvider;
use bricolo_providers::disk::DiskProvider;
use bricolo_providers::notify::NotifyProvider;
use bricolo_providers::speech::SpeechProvider;
use bricolo_providers::tts::TtsProvider;
use bricolo_providers::{CallError, Caller, Providers};
use bricolo_router::Peers;
use iroh::{EndpointId, SecretKey};
use serde::Serialize;
use tokio::sync::Notify;

use crate::capabilities::{
    self, GuardedDisk, GuardedSpeech, RemoteClipboard, RemoteDisk, RemoteNotify, RemoteSpeech,
    RemoteTts, dispatch,
};
use crate::client::Remote;
use crate::datagram::{self, DatagramHandler};
use crate::error::MeshError;
use crate::media::{FrameReceiver, FrameSender, MediaStream, StreamHandler};
use crate::peers::{Peer, PeerList};
use crate::sharing::{self, PluginStore};
use crate::transport::iroh::{IrohConfig, IrohTransport};
use crate::transport::tailscale::{TailscaleConfig, TailscaleTransport};
use crate::transport::{
    ConnectError, Connection, Discovered, PeerId, RecvStream, SendStream, Transport, TransportKind,
    TransportStatus, close,
};
use crate::wire::{
    Codec, Header, Json, Kind, MAX_CALL_FRAME, MAX_MEDIA_FRAME, Postcard, Response, WireError,
    read_frame, recv_value, send_value, write_frame,
};

/// How long a connection attempt over one transport may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Wait between reconnection attempts: doubles from the first to the last.
const BACKOFF: (Duration, Duration) = (Duration::from_millis(500), Duration::from_secs(30));
/// How long a peer may take to send the header of a new stream.
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);

/// One transport to start, with its settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportConfig {
    Iroh(IrohConfig),
    Tailscale(TailscaleConfig),
}

impl TransportConfig {
    pub fn kind(&self) -> TransportKind {
        match self {
            Self::Iroh(_) => TransportKind::Iroh,
            Self::Tailscale(_) => TransportKind::Tailscale,
        }
    }

    async fn bind(&self, key: SecretKey) -> Result<Arc<dyn Transport>, MeshError> {
        let bound: Result<Arc<dyn Transport>, String> = match self {
            Self::Iroh(c) => IrohTransport::bind(key, c.clone())
                .await
                .map(|t| Arc::new(t) as _),
            Self::Tailscale(c) => TailscaleTransport::bind(key, c.clone())
                .await
                .map(|t| Arc::new(t) as _),
        };
        bound.map_err(|e| MeshError::Bind(format!("{}: {e}", self.kind())))
    }
}

#[derive(Clone, Debug)]
pub struct MeshConfig {
    /// The transports to run, in order of preference: a peer is dialled
    /// over the first one that reaches it. Each also accepts connections.
    pub transports: Vec<TransportConfig>,
    /// Deadline for one remote call.
    pub call_timeout: Duration,
}

impl Default for MeshConfig {
    fn default() -> Self {
        Self {
            transports: vec![TransportConfig::Iroh(IrohConfig::default())],
            call_timeout: Duration::from_secs(30),
        }
    }
}

impl MeshConfig {
    /// iroh on loopback only, no relays, no discovery: for tests.
    pub fn local_only() -> Self {
        Self {
            transports: vec![TransportConfig::Iroh(IrohConfig::local_only())],
            call_timeout: Duration::from_secs(30),
        }
    }
}

/// The state of this node's own connection to a peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkState {
    Connecting,
    Connected,
    /// The last attempt failed; another follows after a pause.
    Offline,
    /// The peer closed the connection because this node is not in its
    /// `peers.toml`.
    Refused,
}

#[derive(Clone, Debug, Serialize)]
pub struct PeerStatus {
    pub name: String,
    pub id: String,
    pub allow: Vec<String>,
    /// This node's connection to the peer, which carries its calls.
    pub state: LinkState,
    /// The transport that carries this node's connection, while connected.
    pub via: Option<TransportKind>,
    /// Connections the peer holds open to this node.
    pub incoming: usize,
    /// Unix seconds of the last connection or answered call either way.
    pub last_seen: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MeshStatus {
    pub id: String,
    /// True when the iroh transport runs with n0's relays and discovery.
    pub relays: bool,
    /// Every transport's local addresses.
    pub bound: Vec<String>,
    pub transports: Vec<TransportStatus>,
    pub peers: Vec<PeerStatus>,
}

struct Link {
    peer: Peer,
    state: LinkState,
    conn: Option<Connection>,
    incoming: Vec<Connection>,
    last_seen: Option<SystemTime>,
    last_error: Option<String>,
    wake: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Default)]
struct State {
    links: BTreeMap<String, Link>,
}

pub(crate) struct Shared {
    id: EndpointId,
    transports: Vec<Arc<dyn Transport>>,
    relays: bool,
    pub(crate) call_timeout: Duration,
    next_id: AtomicU64,
    state: Mutex<State>,
    local: OnceLock<Providers>,
    streams: Mutex<HashMap<String, Arc<dyn StreamHandler>>>,
    datagrams: Mutex<HashMap<String, Arc<dyn DatagramHandler>>>,
    home: PathBuf,
}

impl Shared {
    pub(crate) fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn with_link(&self, name: &str, f: impl FnOnce(&mut Link)) {
        if let Some(link) = self.state.lock().unwrap().links.get_mut(name) {
            f(link);
        }
    }

    pub(crate) fn touch(&self, name: &str) {
        self.with_link(name, |l| l.last_seen = Some(SystemTime::now()));
    }

    fn peer_by_id(&self, id: &EndpointId) -> Option<Peer> {
        let state = self.state.lock().unwrap();
        state
            .links
            .values()
            .find(|l| &l.peer.id == id)
            .map(|l| l.peer.clone())
    }
}

/// This node's place in the mesh. Cloning shares it.
#[derive(Clone)]
pub struct Mesh(Arc<Shared>);

impl Mesh {
    /// Binds every transport in `config` and starts connecting to every
    /// peer. Incoming calls answer `unavailable` until [`Mesh::serve`]
    /// gives the providers.
    pub async fn start(
        key: SecretKey,
        peers: PeerList,
        config: MeshConfig,
    ) -> Result<Self, MeshError> {
        let mut transports = Vec::new();
        for t in &config.transports {
            if transports
                .iter()
                .any(|b: &Arc<dyn Transport>| b.kind() == t.kind())
            {
                return Err(MeshError::Bind(format!("{} is listed twice", t.kind())));
            }
            transports.push(t.bind(key.clone()).await?);
        }
        let relays = config
            .transports
            .iter()
            .any(|t| matches!(t, TransportConfig::Iroh(c) if c.relays));
        Self::with_transports(key.public(), transports, peers, config.call_timeout, relays)
    }

    /// Runs the mesh over transports that are already bound. `id` is the
    /// key every transport authenticates as. `relays` only shows in
    /// [`Mesh::status`].
    pub fn with_transports(
        id: EndpointId,
        transports: Vec<Arc<dyn Transport>>,
        peers: PeerList,
        call_timeout: Duration,
        relays: bool,
    ) -> Result<Self, MeshError> {
        if transports.is_empty() {
            return Err(MeshError::Bind("no transport is turned on".into()));
        }
        let shared = Arc::new(Shared {
            id,
            transports,
            relays,
            call_timeout,
            next_id: AtomicU64::new(1),
            state: Mutex::default(),
            local: OnceLock::new(),
            streams: Mutex::default(),
            datagrams: Mutex::default(),
            home: bricolo_providers::disk::home(),
        });
        for t in &shared.transports {
            tokio::spawn(accept_loop(Arc::downgrade(&shared), t.clone()));
        }
        let mesh = Self(shared);
        mesh.set_peers(peers);
        let kinds: Vec<_> = mesh.0.transports.iter().map(|t| t.kind()).collect();
        tracing::info!(id = %mesh.id(), transports = ?kinds, bound = ?mesh.bound(), relays, "mesh up");
        Ok(mesh)
    }

    pub fn id(&self) -> EndpointId {
        self.0.id
    }

    /// Every transport's local addresses, in order of preference.
    pub fn bound(&self) -> Vec<SocketAddr> {
        self.0.transports.iter().flat_map(|t| t.bound()).collect()
    }

    /// The local addresses of one transport.
    pub fn bound_on(&self, kind: TransportKind) -> Vec<SocketAddr> {
        self.0
            .transports
            .iter()
            .filter(|t| t.kind() == kind)
            .flat_map(|t| t.bound())
            .collect()
    }

    /// What each transport sees on its network now.
    pub async fn discover(&self) -> Vec<(TransportKind, Result<Vec<Discovered>, String>)> {
        let mut found = Vec::new();
        for t in &self.0.transports {
            found.push((t.kind(), t.discover().await));
        }
        found
    }

    /// The providers that answer peers' calls: this node's own, never the
    /// routed ones, so a call cannot bounce between nodes.
    pub fn serve(&self, local: Providers) {
        if self.0.local.set(local).is_err() {
            tracing::warn!("mesh providers were already set");
        }
    }

    /// Serves media streams for `capability`. Peers still need it in their
    /// allow list.
    pub fn register_stream(&self, capability: &str, handler: Arc<dyn StreamHandler>) {
        self.0
            .streams
            .lock()
            .unwrap()
            .insert(capability.to_string(), handler);
    }

    /// Receives datagrams for `capability`. Peers still need it in their
    /// allow list.
    pub fn register_datagrams(&self, capability: &str, handler: Arc<dyn DatagramHandler>) {
        self.0
            .datagrams
            .lock()
            .unwrap()
            .insert(capability.to_string(), handler);
    }

    /// Sends `payload` to `peer`'s handler for `capability`, as an
    /// unreliable datagram when the connection has them and it fits, else
    /// on a stream of its own. `unavailable` when the peer is offline.
    pub async fn send_datagram(
        &self,
        peer: &str,
        capability: &str,
        payload: &[u8],
    ) -> Result<(), CallError> {
        let remote = self.remote(peer).ok_or(CallError::Unavailable)?;
        let framed = datagram::encode(capability, payload).ok_or_else(|| {
            CallError::Failed(format!(
                "capability name over {} bytes",
                datagram::MAX_CAPABILITY
            ))
        })?;
        if remote
            .conn
            .max_datagram_size()
            .is_some_and(|max| framed.len() <= max)
        {
            return remote.conn.send_datagram(framed).map_err(|_| remote.lost());
        }
        if payload.len() > MAX_MEDIA_FRAME {
            return Err(CallError::Failed(format!(
                "datagram of {} bytes is over the {MAX_MEDIA_FRAME} byte limit",
                payload.len()
            )));
        }
        let header = Header {
            id: self.0.next_id(),
            kind: Kind::Datagram,
            capability: capability.to_string(),
            method: String::new(),
            deadline_ms: 0,
            plugin: String::new(),
        };
        let (mut send, _recv) = remote.conn.open_bi().await.map_err(|_| remote.lost())?;
        send_value(&mut send, &header)
            .await
            .map_err(|_| remote.lost())?;
        write_frame(&mut send, payload)
            .await
            .map_err(|_| remote.lost())?;
        send.finish();
        Ok(())
    }

    /// Shares the plugins `store` holds with peers that may call
    /// [`sharing::CAPABILITY`].
    pub fn share_plugins(&self, store: Arc<dyn PluginStore>) {
        self.register_stream(sharing::CAPABILITY, Arc::new(sharing::Serve(store)));
    }

    /// Fetches the bundle of the plugin whose `plugin.wasm` has `sha256`
    /// from `peer`. The caller checks the hash of what it unpacks.
    pub async fn fetch_plugin(
        &self,
        peer: &str,
        sha256: &str,
        caller: &Caller,
    ) -> Result<Vec<u8>, CallError> {
        if !sharing::is_sha256(sha256) {
            return Err(CallError::Failed(format!("{sha256:?} is not a sha256")));
        }
        let stream = self
            .open_stream(peer, sharing::CAPABILITY, sha256, caller)
            .await?;
        // Large bundles over slow links: four call deadlines.
        tokio::time::timeout(self.0.call_timeout * 4, sharing::receive(stream))
            .await
            .unwrap_or(Err(CallError::Timeout))
    }

    /// Applies a new peer list: connects to new peers, drops removed ones,
    /// and reconnects to peers whose key or addresses changed. Allow lists
    /// apply from the next call on.
    pub fn set_peers(&self, peers: PeerList) {
        for t in &self.0.transports {
            t.set_peers(&peers);
        }
        let mut state = self.0.state.lock().unwrap();
        state.links.retain(|name, link| {
            let keep = peers.peers.get(name).is_some_and(|p| {
                p.id == link.peer.id && p.addrs == link.peer.addrs && p.tailnet == link.peer.tailnet
            });
            if !keep {
                for conn in link.conn.iter().chain(&link.incoming) {
                    conn.close(close::REPLACED, b"peers.toml changed");
                }
            }
            keep
        });
        for (name, peer) in peers.peers {
            if let Some(link) = state.links.get_mut(&name) {
                link.peer = peer;
                continue;
            }
            let wake = Arc::new(Notify::new());
            let task = tokio::spawn(supervise(
                Arc::downgrade(&self.0),
                peer.clone(),
                wake.clone(),
            ));
            state.links.insert(
                name,
                Link {
                    peer,
                    state: LinkState::Connecting,
                    conn: None,
                    incoming: Vec::new(),
                    last_seen: None,
                    last_error: None,
                    wake,
                    task,
                },
            );
        }
    }

    pub fn status(&self) -> MeshStatus {
        let state = self.0.state.lock().unwrap();
        let unix = |t: SystemTime| t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs());
        MeshStatus {
            id: self.id().to_string(),
            relays: self.0.relays,
            bound: self.bound().iter().map(|a| a.to_string()).collect(),
            transports: self.0.transports.iter().map(|t| t.status()).collect(),
            peers: state
                .links
                .values()
                .map(|l| PeerStatus {
                    name: l.peer.name.clone(),
                    id: l.peer.id.to_string(),
                    allow: l.peer.allow.clone(),
                    state: l.state,
                    via: l.conn.as_ref().map(|c| c.transport()),
                    incoming: l.incoming.len(),
                    last_seen: l.last_seen.and_then(unix),
                    last_error: l.last_error.clone(),
                })
                .collect(),
        }
    }

    /// Waits until this node's connection to `peer` is up. `denied` if the
    /// peer refuses this node, `unavailable` if `within` runs out.
    pub async fn wait_connected(&self, peer: &str, within: Duration) -> Result<(), CallError> {
        let until = Instant::now() + within;
        loop {
            let state = self
                .0
                .state
                .lock()
                .unwrap()
                .links
                .get(peer)
                .map(|l| l.state);
            match state {
                None => return Err(CallError::Unavailable),
                Some(LinkState::Connected) => return Ok(()),
                Some(LinkState::Refused) => return Err(CallError::Denied),
                _ if Instant::now() >= until => return Err(CallError::Unavailable),
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    }

    /// Opens a media stream to `peer`'s handler for `capability`.
    pub async fn open_stream(
        &self,
        peer: &str,
        capability: &str,
        method: &str,
        caller: &Caller,
    ) -> Result<MediaStream, CallError> {
        let remote = self.remote(peer).ok_or(CallError::Unavailable)?;
        let header = Header {
            id: self.0.next_id(),
            kind: Kind::Stream,
            capability: capability.to_string(),
            method: method.to_string(),
            deadline_ms: 0,
            plugin: caller.plugin.clone(),
        };
        let open = async {
            let (mut send, mut recv) = remote.conn.open_bi().await.map_err(|_| remote.lost())?;
            send_value(&mut send, &header)
                .await
                .map_err(|_| remote.lost())?;
            let ack: Response = recv_value(&mut recv, MAX_CALL_FRAME)
                .await
                .map_err(|_| remote.lost())?;
            ack?;
            Ok(MediaStream {
                tx: FrameSender::new(send),
                rx: FrameReceiver::new(recv),
            })
        };
        tokio::time::timeout(self.0.call_timeout, open)
            .await
            .unwrap_or(Err(CallError::Timeout))
    }

    /// Closes every connection and every transport.
    pub async fn close(&self) {
        let links = std::mem::take(&mut self.0.state.lock().unwrap().links);
        for link in links.values() {
            for conn in link.conn.iter().chain(&link.incoming) {
                conn.close(close::SHUTDOWN, b"node stopping");
            }
        }
        drop(links);
        for t in &self.0.transports {
            t.close().await;
        }
    }

    /// `peer`'s connection, if it is up.
    /// Runs one method on the named peer, with JSON arguments and result,
    /// as `caller`. A peer that is not connected is `unavailable`.
    pub async fn call_json(
        &self,
        peer: &str,
        capability: &str,
        method: &str,
        caller: &Caller,
        args: &[u8],
    ) -> Result<Vec<u8>, CallError> {
        let remote = self.remote(peer).ok_or(CallError::Unavailable)?;
        capabilities::dispatch_remote::<Json>(&remote, caller, capability, method, args).await
    }

    /// The methods the named peer lets this node call, as
    /// `(capability, method)`.
    pub async fn peer_methods(&self, peer: &str) -> Result<Vec<(String, String)>, CallError> {
        let remote = self.remote(peer).ok_or(CallError::Unavailable)?;
        let caller = Caller {
            plugin: "bricolo-mesh".into(),
            ..Caller::default()
        };
        remote
            .call(
                capabilities::MESH,
                "methods",
                &caller,
                Postcard::encode(&()),
            )
            .await
    }

    fn remote(&self, peer: &str) -> Option<Remote> {
        let state = self.0.state.lock().unwrap();
        let conn = state.links.get(peer)?.conn.clone()?;
        if conn.close_reason().is_some() {
            return None;
        }
        Some(Remote {
            conn,
            peer: peer.to_string(),
            shared: self.0.clone(),
        })
    }
}

/// A peer counts as online while this node's connection to it is up.
/// Offline peers return `None`, and the router moves to its next target.
impl Peers for Mesh {
    fn tts(&self, peer: &str) -> Option<Arc<dyn TtsProvider>> {
        Some(Arc::new(RemoteTts(self.remote(peer)?)))
    }
    fn speech(&self, peer: &str) -> Option<Arc<dyn SpeechProvider>> {
        Some(Arc::new(GuardedSpeech(RemoteSpeech(self.remote(peer)?))))
    }
    fn disk(&self, peer: &str) -> Option<Arc<dyn DiskProvider>> {
        Some(Arc::new(GuardedDisk(RemoteDisk(self.remote(peer)?))))
    }
    fn clipboard(&self, peer: &str) -> Option<Arc<dyn ClipboardProvider>> {
        Some(Arc::new(RemoteClipboard(self.remote(peer)?)))
    }
    fn notify(&self, peer: &str) -> Option<Arc<dyn NotifyProvider>> {
        Some(Arc::new(RemoteNotify(self.remote(peer)?)))
    }
    fn claude_code(
        &self,
        peer: &str,
    ) -> Option<Arc<dyn bricolo_providers::claude_code::ClaudeCodeProvider>> {
        Some(Arc::new(crate::capabilities::RemoteClaudeCode(
            self.remote(peer)?,
        )))
    }
}

/// Keeps one connection to `peer` open: tries each transport in order
/// until one connects, waits for that connection to close, pauses, and
/// starts over. An incoming connection from the peer cuts the pause short.
async fn supervise(shared: Weak<Shared>, peer: Peer, wake: Arc<Notify>) {
    let mut backoff = BACKOFF.0;
    loop {
        let Some(transports) = shared.upgrade().map(|s| {
            s.with_link(&peer.name, |l| l.state = LinkState::Connecting);
            s.transports.clone()
        }) else {
            return;
        };
        let mut errors = Vec::new();
        let mut refused = false;
        let mut connected = None;
        for t in &transports {
            let Some(id) = t.locate(&peer) else { continue };
            let attempt = tokio::time::timeout(CONNECT_TIMEOUT, connect(t.as_ref(), &id, peer.id))
                .await
                .unwrap_or_else(|_| Err(ConnectError::new("connection attempt timed out")));
            match attempt {
                Ok(conn) => {
                    connected = Some((id, conn));
                    break;
                }
                Err(e) => {
                    tracing::debug!(peer = %peer.name, "cannot connect over {id}: {e}");
                    refused |= e.refused;
                    errors.push(format!("{}: {e}", t.kind()));
                }
            }
        }
        drop(transports);
        if connected.is_none() && errors.is_empty() {
            errors.push("no transport can reach this peer; give it addrs or turn relays on".into());
        }
        match connected {
            Some((id, conn)) => {
                if let Some(s) = shared.upgrade() {
                    tracing::info!(peer = %peer.name, "connected over {id}");
                    s.with_link(&peer.name, |l| {
                        l.state = LinkState::Connected;
                        l.conn = Some(conn.clone());
                        l.last_seen = Some(SystemTime::now());
                        l.last_error = None;
                    });
                }
                backoff = BACKOFF.0;
                let reason = conn.closed().await;
                let Some(s) = shared.upgrade() else { return };
                tracing::info!(peer = %peer.name, "disconnected: {reason}");
                s.with_link(&peer.name, |l| {
                    l.state = if reason.is_refused() {
                        LinkState::Refused
                    } else {
                        LinkState::Offline
                    };
                    l.conn = None;
                    l.last_error = Some(reason.to_string());
                });
            }
            None => {
                let Some(s) = shared.upgrade() else { return };
                s.with_link(&peer.name, |l| {
                    l.state = if refused {
                        LinkState::Refused
                    } else {
                        LinkState::Offline
                    };
                    l.last_error = Some(errors.join("; "));
                });
            }
        }
        // A transport whose network changed cuts the pause short too.
        let Some(transports) = shared.upgrade().map(|s| s.transports.clone()) else {
            return;
        };
        let changed = futures_util::future::select_all(
            transports.iter().map(|t| t.changed()).collect::<Vec<_>>(),
        );
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = wake.notified() => {}
            _ = changed => {
                tracing::debug!(peer = %peer.name, "a transport's network changed; trying again");
            }
        }
        drop(transports);
        backoff = (backoff * 2).min(BACKOFF.1);
    }
}

/// Connects and pings. A transport can finish the client's side of the
/// handshake before the peer's trust check runs, so only the answered ping
/// shows that the peer accepted this node.
async fn connect(
    transport: &dyn Transport,
    id: &PeerId,
    key: EndpointId,
) -> Result<Connection, ConnectError> {
    let conn = transport.connect(id, key).await?;
    let ping = async {
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
        let header = Header {
            id: 0,
            kind: Kind::Ping,
            capability: String::new(),
            method: String::new(),
            deadline_ms: 0,
            plugin: String::new(),
        };
        send_value(&mut send, &header).await?;
        send.finish();
        let answer: Response = recv_value(&mut recv, MAX_CALL_FRAME)
            .await
            .map_err(|e| format!("{e:?}"))?;
        answer.map_err(|e| format!("{e:?}"))
    };
    match ping.await {
        Ok(_) => Ok(conn),
        Err(e) => Err(match conn.close_reason() {
            Some(reason) => ConnectError {
                message: reason.to_string(),
                refused: reason.is_refused(),
            },
            None => {
                conn.close(close::SHUTDOWN, b"ping failed");
                ConnectError::new(format!("the peer did not answer the ping: {e}"))
            }
        }),
    }
}

async fn accept_loop(shared: Weak<Shared>, transport: Arc<dyn Transport>) {
    while let Some(conn) = transport.accept().await {
        tokio::spawn(serve_connection(shared.clone(), conn));
    }
}

async fn serve_connection(shared: Weak<Shared>, conn: Connection) {
    let id = conn.remote_key();
    let Some(name) = shared
        .upgrade()
        .and_then(|s| s.peer_by_id(&id))
        .map(|p| p.name)
    else {
        conn.close(close::REFUSED, crate::transport::REFUSED_REASON.as_bytes());
        return;
    };
    if let Some(s) = shared.upgrade() {
        tracing::info!(peer = %name, transport = %conn.transport(), "accepted a connection");
        s.with_link(&name, |l| {
            l.incoming.push(conn.clone());
            l.last_seen = Some(SystemTime::now());
            if l.state != LinkState::Connected {
                l.wake.notify_one();
            }
        });
    }
    let reader = tokio::spawn(read_datagrams(shared.clone(), conn.clone(), name.clone()));
    while let Ok((send, recv)) = conn.accept_bi().await {
        tokio::spawn(serve_stream(shared.clone(), id, send, recv));
    }
    reader.abort();
    if let Some(s) = shared.upgrade() {
        s.with_link(&name, |l| l.incoming.retain(|c| !Arc::ptr_eq(c, &conn)));
    }
}

/// Hands each datagram on `conn` to its capability's handler, when
/// `name` may call that capability.
async fn read_datagrams(shared: Weak<Shared>, conn: Connection, name: String) {
    if conn.max_datagram_size().is_none() {
        return;
    }
    while let Ok(d) = conn.read_datagram().await {
        let Some((capability, payload)) = datagram::decode(d) else {
            continue;
        };
        let Some(s) = shared.upgrade() else { return };
        deliver_datagram(&s, &name, &capability, payload);
    }
}

fn deliver_datagram(s: &Shared, name: &str, capability: &str, payload: bytes::Bytes) {
    let allowed = s
        .state
        .lock()
        .unwrap()
        .links
        .get(name)
        .is_some_and(|l| l.peer.allows(capability));
    if !allowed {
        tracing::debug!(peer = %name, "dropped a {capability} datagram: not in its allow list");
        return;
    }
    let handler = s.datagrams.lock().unwrap().get(capability).cloned();
    match handler {
        Some(h) => h.receive(name, payload),
        None => tracing::debug!(peer = %name, "dropped a {capability} datagram: no handler"),
    }
}

async fn reply(send: &mut SendStream, response: Response) {
    let _ = send_value(send, &response).await;
    send.finish();
}

async fn serve_stream(
    shared: Weak<Shared>,
    id: EndpointId,
    mut send: SendStream,
    mut recv: RecvStream,
) {
    let header: Header =
        match tokio::time::timeout(HEADER_TIMEOUT, recv_value(&mut recv, MAX_CALL_FRAME)).await {
            Ok(Ok(header)) => header,
            _ => return,
        };
    let Some(s) = shared.upgrade() else { return };
    let Some(peer) = s.peer_by_id(&id) else {
        return reply(&mut send, Err(WireError::Denied)).await;
    };
    s.touch(&peer.name);
    if header.kind == Kind::Ping {
        return reply(&mut send, Ok(Vec::new())).await;
    }
    let call = format!("{}.{}", header.capability, header.method);
    if header.kind == Kind::Call && header.capability == capabilities::MESH {
        // Read the (empty) arguments so the caller's stream finishes cleanly.
        let _ = tokio::time::timeout(HEADER_TIMEOUT, read_frame(&mut recv, MAX_CALL_FRAME)).await;
        let result = match header.method.as_str() {
            "methods" => Postcard::encode(&capabilities::allowed_methods(|c| peer.allows(c))),
            other => Err(CallError::Failed(format!("mesh has no method {other}"))),
        };
        return reply(&mut send, result.map_err(WireError::from)).await;
    }
    if !peer.allows(&header.capability) {
        tracing::warn!(peer = %peer.name, id = header.id, "denied {call}: not in its allow list");
        return reply(&mut send, Err(WireError::Denied)).await;
    }
    let caller = Caller {
        plugin: format!("{}/{}", peer.name, header.plugin),
        disk: peer.disk_grants(&s.home),
        ..Caller::default()
    };
    match header.kind {
        Kind::Call => {
            let providers = s.local.get().cloned();
            drop(s);
            let args =
                match tokio::time::timeout(HEADER_TIMEOUT, read_frame(&mut recv, MAX_CALL_FRAME))
                    .await
                {
                    Ok(Ok(args)) => args,
                    _ => return,
                };
            let Some(providers) = providers else {
                return reply(&mut send, Err(WireError::Unavailable)).await;
            };
            let started = Instant::now();
            let work = dispatch::<Postcard>(
                &providers,
                &caller,
                &header.capability,
                &header.method,
                &args,
            );
            let work = async {
                match header.deadline_ms {
                    0 => work.await,
                    ms => tokio::time::timeout(Duration::from_millis(ms), work)
                        .await
                        .unwrap_or(Err(CallError::Timeout)),
                }
            };
            // The caller dropping its side stops this stream; dropping
            // `work` then cancels the provider's future.
            let stopped = send.stopped();
            let result = tokio::select! {
                result = work => result,
                _ = stopped => {
                    tracing::info!(peer = %peer.name, id = header.id, "{call} cancelled by the caller");
                    return;
                }
            };
            let ms = started.elapsed().as_millis();
            match &result {
                Ok(_) => {
                    tracing::info!(peer = %peer.name, plugin = %header.plugin, id = header.id, "served {call} in {ms} ms")
                }
                Err(e) => {
                    tracing::info!(peer = %peer.name, plugin = %header.plugin, id = header.id, "served {call} in {ms} ms: {e}")
                }
            }
            reply(&mut send, result.map_err(WireError::from)).await;
        }
        Kind::Ping => {}
        Kind::Datagram => {
            let frame =
                tokio::time::timeout(HEADER_TIMEOUT, read_frame(&mut recv, MAX_MEDIA_FRAME)).await;
            if let Ok(Ok(payload)) = frame {
                deliver_datagram(&s, &peer.name, &header.capability, payload.into());
            }
        }
        Kind::Stream => {
            let handler = s.streams.lock().unwrap().get(&header.capability).cloned();
            drop(s);
            let Some(handler) = handler else {
                return reply(&mut send, Err(WireError::Unavailable)).await;
            };
            if send_value(&mut send, &Response::Ok(Vec::new()))
                .await
                .is_err()
            {
                return;
            }
            tracing::info!(peer = %peer.name, id = header.id, "{call} stream opened");
            let stream = MediaStream {
                tx: FrameSender::new(send),
                rx: FrameReceiver::new(recv),
            };
            match handler.serve(caller, header.method, stream).await {
                Ok(()) => tracing::info!(peer = %peer.name, id = header.id, "{call} stream ended"),
                Err(e) => {
                    tracing::info!(peer = %peer.name, id = header.id, "{call} stream ended: {e}")
                }
            }
        }
    }
}
