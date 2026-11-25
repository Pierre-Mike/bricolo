//! What both backends share: an iroh QUIC endpoint keyed by `node.key`,
//! the trust check after the handshake, and the [`Conn`] adapter.
//!
//! The iroh backend runs the endpoint with discovery and relays; the
//! Tailscale backend runs it bare, bound to the tailnet address. Either
//! way the TLS 1.3 handshake uses the two nodes' raw public keys, so each
//! side proves it holds the key that `peers.toml` lists.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock};
use std::time::Duration;

use ::iroh::endpoint::{
    AfterHandshakeOutcome, Builder, ConnectionError, EndpointHooks, QuicTransportConfig,
    ReadExactError, VarInt,
};
use ::iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::future::BoxFuture;
use tokio::sync::{Mutex, Notify, mpsc, watch};

use super::{
    Closed, Conn, ConnectError, Connection, ReadError, RecvHalf, SendHalf, TransportKind, close,
};
use crate::peers::PeerList;
use crate::wire::ALPN;

/// A connection with no traffic for this long counts as lost. Keep-alives
/// go out every 5 seconds, so only a vanished peer hits it.
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);

/// The close reason sent to a key that is not in `peers.toml`.
pub(crate) const REFUSED_REASON: &str = "not in peers.toml";

type Trusted = Arc<RwLock<HashSet<EndpointId>>>;

/// Refuses, right after the TLS handshake, any key not in `peers.toml`.
#[derive(Debug)]
struct Trust(Trusted);

impl EndpointHooks for Trust {
    async fn after_handshake<'a>(
        &'a self,
        conn: &'a ::iroh::endpoint::Connection,
    ) -> AfterHandshakeOutcome {
        if self.0.read().unwrap().contains(&conn.remote_id()) {
            AfterHandshakeOutcome::Accept
        } else {
            tracing::warn!(id = %conn.remote_id(), "refused a key that is not in peers.toml");
            AfterHandshakeOutcome::Reject {
                error_code: VarInt::from_u32(close::REFUSED),
                reason: REFUSED_REASON.as_bytes().to_vec(),
            }
        }
    }
}

/// A connection attempt or the accept loop gives up on a dead socket
/// after this; closing an old socket that still drains may take this long.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Makes a fresh endpoint builder; iroh's builder is used up by `bind`.
pub(crate) type MakeBuilder = Box<dyn Fn() -> Builder + Send + Sync>;

/// One bound iroh endpoint and the task that accepts on it.
struct Socket {
    endpoint: Endpoint,
    /// Read at bind time: a closed endpoint no longer reports them.
    addrs: Vec<SocketAddr>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl Socket {
    /// False once the endpoint closed or its driver died, such as when the
    /// address it was bound to went away. Nothing reads the socket then.
    fn alive(&self) -> bool {
        !self.accept_task.is_finished() && !self.endpoint.is_closed()
    }

    async fn close(&self) {
        self.accept_task.abort();
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, self.endpoint.close()).await;
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

/// A QUIC endpoint that can be bound again. The trusted keys, the channel
/// of accepted connections and the waiters on [`QuicEndpoint::changed`]
/// outlive each socket, so the transport above never sees a rebind.
pub(crate) struct QuicEndpoint {
    kind: TransportKind,
    key: SecretKey,
    builder: MakeBuilder,
    trusted: Trusted,
    /// `None` once closed, so `accept` returns `None`.
    tx: StdMutex<Option<mpsc::Sender<Connection>>>,
    incoming: Mutex<mpsc::Receiver<Connection>>,
    /// `None` between losing a socket and binding the next one.
    socket: RwLock<Option<Arc<Socket>>>,
    /// The addresses the last socket had, to keep its ports.
    last: StdMutex<Vec<SocketAddr>>,
    /// Woken when a socket's accept loop ends.
    ended: Arc<Notify>,
    /// Counts binds after the first.
    rebound: watch::Sender<u64>,
    closed: AtomicBool,
}

impl QuicEndpoint {
    /// Finishes the builder with the key, ALPN, trust hook and idle
    /// timeout, binds `bind` (all interfaces on a free port when empty) and
    /// starts accepting.
    pub(crate) async fn bind(
        kind: TransportKind,
        builder: MakeBuilder,
        key: SecretKey,
        bind: &[SocketAddr],
    ) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel(16);
        let quic = Self {
            kind,
            key,
            builder,
            trusted: Arc::default(),
            tx: StdMutex::new(Some(tx)),
            incoming: Mutex::new(rx),
            socket: RwLock::new(None),
            last: StdMutex::default(),
            ended: Arc::new(Notify::new()),
            rebound: watch::Sender::new(0),
            closed: AtomicBool::new(false),
        };
        quic.install(quic.open(bind).await?);
        Ok(quic)
    }

    async fn open(&self, bind: &[SocketAddr]) -> Result<Socket, String> {
        let tx = self
            .tx
            .lock()
            .unwrap()
            .clone()
            .ok_or("the transport is closed")?;
        let idle = IDLE_TIMEOUT.try_into().map_err(|e| format!("{e}"))?;
        let mut builder = (self.builder)()
            .secret_key(self.key.clone())
            .alpns(vec![ALPN.to_vec()])
            .hooks(Trust(self.trusted.clone()))
            .transport_config(
                QuicTransportConfig::builder()
                    .max_idle_timeout(Some(idle))
                    .build(),
            );
        if !bind.is_empty() {
            builder = builder.clear_ip_transports();
            for addr in bind {
                builder = builder
                    .bind_addr(*addr)
                    .map_err(|e| format!("{addr}: {e}"))?;
            }
        }
        let endpoint = builder.bind().await.map_err(|e| e.to_string())?;
        let accept_task = tokio::spawn(accept_loop(
            self.kind,
            endpoint.clone(),
            tx,
            self.ended.clone(),
        ));
        Ok(Socket {
            addrs: endpoint.bound_sockets(),
            endpoint,
            accept_task,
        })
    }

    /// Closes the socket, if any, and binds `bind` instead. A port of 0
    /// keeps the port the last socket had on that IP, so peers that were
    /// given it still reach this node. Waiters on [`Self::changed`] wake.
    pub(crate) async fn rebind(&self, bind: &[SocketAddr]) -> Result<(), String> {
        let old = self.socket.write().unwrap().take();
        if let Some(old) = old {
            old.close().await;
        }
        if self.closed.load(Ordering::SeqCst) {
            return Err("the transport is closed".into());
        }
        let mut want = bind.to_vec();
        let last = self.last.lock().unwrap().clone();
        for addr in want.iter_mut().filter(|a| a.port() == 0) {
            if let Some(p) = last.iter().find(|p| p.ip() == addr.ip()) {
                addr.set_port(p.port());
            }
        }
        self.install(self.open(&want).await?);
        tracing::info!(transport = %self.kind, bound = ?self.bound(), "bound again");
        self.rebound.send_modify(|n| *n += 1);
        Ok(())
    }

    fn install(&self, socket: Socket) {
        *self.last.lock().unwrap() = socket.addrs.clone();
        *self.socket.write().unwrap() = Some(Arc::new(socket));
    }

    /// True while a socket is bound and read.
    pub(crate) fn alive(&self) -> bool {
        self.current().is_some()
    }

    fn current(&self) -> Option<Arc<Socket>> {
        self.socket
            .read()
            .unwrap()
            .as_ref()
            .filter(|s| s.alive())
            .cloned()
    }

    /// Resolves when a socket's accept loop has ended since the last call.
    pub(crate) async fn ended(&self) {
        self.ended.notified().await;
    }

    /// Resolves at the next rebind.
    pub(crate) async fn changed(&self) {
        let mut rx = self.rebound.subscribe();
        let _ = rx.changed().await;
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub(crate) fn set_peers(&self, peers: &PeerList) {
        *self.trusted.write().unwrap() = peers.peers.values().map(|p| p.id).collect();
    }

    pub(crate) async fn connect(
        &self,
        addr: EndpointAddr,
        key: EndpointId,
    ) -> Result<Connection, ConnectError> {
        let socket = self
            .current()
            .ok_or_else(|| ConnectError::new("not bound; waiting for the address to come back"))?;
        let conn = socket
            .endpoint
            .connect(addr, ALPN)
            .await
            .map_err(|e| ConnectError::new(e.to_string()))?;
        // The handshake already proved the key; this guards the address.
        if conn.remote_id() != key {
            conn.close(VarInt::from_u32(close::SHUTDOWN), b"wrong key");
            return Err(ConnectError::new("the peer answered with another key"));
        }
        Ok(Arc::new(QuicConn {
            kind: self.kind,
            conn,
        }))
    }

    pub(crate) async fn accept(&self) -> Option<Connection> {
        self.incoming.lock().await.recv().await
    }

    /// The live socket's addresses; empty while none is bound.
    pub(crate) fn bound(&self) -> Vec<SocketAddr> {
        self.current().map(|s| s.addrs.clone()).unwrap_or_default()
    }

    pub(crate) async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.tx.lock().unwrap().take();
        self.ended.notify_one();
        let socket = self.socket.write().unwrap().take();
        if let Some(socket) = socket {
            socket.accept_task.abort();
            socket.endpoint.close().await;
        }
    }

    /// Closes the endpoint under the transport, as a vanished address
    /// does, without closing the transport. For tests.
    pub(crate) async fn kill_socket(&self) {
        let socket = self.socket.read().unwrap().clone();
        if let Some(socket) = socket {
            socket.endpoint.close().await;
        }
    }
}

/// Binds `bind` again whenever the socket dies, retrying with backoff
/// until the address is back. For a transport whose addresses never move.
pub(crate) async fn keep_bound(quic: std::sync::Weak<QuicEndpoint>, bind: Vec<SocketAddr>) {
    let mut wait = REBIND_BACKOFF.0;
    loop {
        let Some(q) = quic.upgrade() else { return };
        if q.is_closed() {
            return;
        }
        if q.alive() {
            wait = REBIND_BACKOFF.0;
            let ended = q.ended.clone();
            drop(q);
            ended.notified().await;
            continue;
        }
        if let Err(e) = q.rebind(&bind).await {
            tracing::warn!(transport = %q.kind, "cannot bind {bind:?} again: {e}");
            drop(q);
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(REBIND_BACKOFF.1);
        }
    }
}

/// Wait between attempts to bind a lost address again.
pub(crate) const REBIND_BACKOFF: (Duration, Duration) =
    (Duration::from_millis(250), Duration::from_secs(10));

/// Finishes each handshake on its own task, so a slow peer holds up no one.
/// Wakes `ended` when the endpoint stops accepting, closed or dead.
async fn accept_loop(
    kind: TransportKind,
    endpoint: Endpoint,
    tx: mpsc::Sender<Connection>,
    ended: Arc<Notify>,
) {
    while let Some(incoming) = endpoint.accept().await {
        let tx = tx.clone();
        tokio::spawn(async move {
            match incoming.await {
                Ok(conn) => {
                    let _ = tx.send(Arc::new(QuicConn { kind, conn })).await;
                }
                Err(e) => tracing::debug!(transport = %kind, "incoming connection failed: {e}"),
            }
        });
    }
    tracing::warn!(transport = %kind, "the endpoint stopped accepting");
    ended.notify_one();
}

struct QuicConn {
    kind: TransportKind,
    conn: ::iroh::endpoint::Connection,
}

fn closed(e: ConnectionError) -> Closed {
    match e {
        ConnectionError::ApplicationClosed(c) => Closed::ByPeer {
            code: u32::try_from(u64::from(c.error_code)).unwrap_or(u32::MAX),
            reason: String::from_utf8_lossy(&c.reason).into_owned(),
        },
        ConnectionError::LocallyClosed => Closed::Locally,
        other => Closed::Lost(other.to_string()),
    }
}

fn boxed(
    (send, recv): (::iroh::endpoint::SendStream, ::iroh::endpoint::RecvStream),
) -> (super::SendStream, super::RecvStream) {
    (Box::new(QuicSend(send)), Box::new(QuicRecv(recv)))
}

#[async_trait]
impl Conn for QuicConn {
    fn remote_key(&self) -> EndpointId {
        self.conn.remote_id()
    }

    fn transport(&self) -> TransportKind {
        self.kind
    }

    fn stable_id(&self) -> usize {
        self.conn.stable_id()
    }

    async fn open_bi(&self) -> Result<(super::SendStream, super::RecvStream), Closed> {
        self.conn.open_bi().await.map(boxed).map_err(closed)
    }

    async fn accept_bi(&self) -> Result<(super::SendStream, super::RecvStream), Closed> {
        self.conn.accept_bi().await.map(boxed).map_err(closed)
    }

    fn max_datagram_size(&self) -> Option<usize> {
        self.conn.max_datagram_size()
    }

    fn send_datagram(&self, data: Bytes) -> Result<(), String> {
        self.conn.send_datagram(data).map_err(|e| e.to_string())
    }

    async fn read_datagram(&self) -> Result<Bytes, Closed> {
        self.conn.read_datagram().await.map_err(closed)
    }

    fn close(&self, code: u32, reason: &[u8]) {
        self.conn.close(VarInt::from_u32(code), reason);
    }

    async fn closed(&self) -> Closed {
        closed(self.conn.closed().await)
    }

    fn close_reason(&self) -> Option<Closed> {
        self.conn.close_reason().map(closed)
    }
}

struct QuicSend(::iroh::endpoint::SendStream);

#[async_trait]
impl SendHalf for QuicSend {
    async fn write_all(&mut self, buf: &[u8]) -> Result<(), String> {
        self.0.write_all(buf).await.map_err(|e| e.to_string())
    }

    fn finish(&mut self) {
        let _ = self.0.finish();
    }

    fn reset(&mut self, code: u32) {
        let _ = self.0.reset(VarInt::from_u32(code));
    }

    fn stopped(&self) -> BoxFuture<'static, ()> {
        let stopped = self.0.stopped();
        Box::pin(async move {
            let _ = stopped.await;
        })
    }
}

struct QuicRecv(::iroh::endpoint::RecvStream);

#[async_trait]
impl RecvHalf for QuicRecv {
    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ReadError> {
        self.0.read_exact(buf).await.map_err(|e| match e {
            ReadExactError::FinishedEarly(n) => ReadError::FinishedEarly(n),
            other => ReadError::Broken(other.to_string()),
        })
    }
}
