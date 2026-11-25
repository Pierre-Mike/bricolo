//! The Tailscale backend: QUIC straight to a peer's tailnet IP on a fixed
//! port, with no relays and no discovery service of our own.
//!
//! - Discovery: `tailscale status --json` lists the tailnet's machines. A
//!   peer's [`PeerId`] is its MagicDNS machine name, which survives IP
//!   changes.
//! - Reachability: Tailscale routes the packets, encrypted with WireGuard.
//! - Authentication: Tailscale says which machine sent a packet, not which
//!   bricolo node. So the QUIC handshake still checks the node's key against
//!   `peers.toml`, exactly as the iroh backend does: a tailnet machine
//!   without a trusted key gets `refused`.
//!
//! The endpoint binds this device's tailnet address, so nothing outside
//! the tailnet can reach the port. That address goes away while Tailscale
//! is down or the Mac sleeps, and the socket on it dies, so the poller binds
//! again: on the same address once it is back, or on the new one when it
//! moved.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use ::iroh::endpoint::presets;
use ::iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey};
use async_trait::async_trait;
use serde::Deserialize;

use super::quic::{QuicEndpoint, REBIND_BACKOFF};
use super::{
    ConnectError, Connection, Discovered, PeerId, Transport, TransportKind, TransportStatus,
};
use crate::peers::{Peer, PeerList};

/// The UDP port every node listens on inside the tailnet, unless
/// `[mesh.tailscale] port` or a peer's `tailnet = "host:port"` says
/// otherwise.
pub const DEFAULT_PORT: u16 = 7787;

/// How long `tailscale status` may take.
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the transport rereads `tailscale status` to notice machines
/// that join, leave, change address or come online.
pub const DEFAULT_POLL: Duration = Duration::from_secs(10);

/// A connection attempt reuses a status read this recently.
const FRESH: Duration = Duration::from_secs(2);

/// Where the tailnet's machine list comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusSource {
    /// A `tailscale` CLI; runs `<cli> status --json`.
    Command(PathBuf),
    /// A fixed `tailscale status --json` answer, for tests.
    Fixed(String),
}

impl StatusSource {
    /// `tailscale` on `PATH`, else the CLI inside the macOS app.
    pub fn default_command() -> Self {
        let on_path = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("tailscale"))
                .find(|p| p.is_file())
        });
        let mac_app = PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale");
        Self::Command(on_path.unwrap_or(if mac_app.is_file() {
            mac_app
        } else {
            PathBuf::from("tailscale")
        }))
    }

    pub async fn read(&self) -> Result<Tailnet, String> {
        match self {
            Self::Fixed(json) => Tailnet::parse(json),
            Self::Command(cli) => {
                let run = tokio::process::Command::new(cli)
                    .args(["status", "--json"])
                    .kill_on_drop(true)
                    .output();
                let out = tokio::time::timeout(STATUS_TIMEOUT, run)
                    .await
                    .map_err(|_| format!("{} status took too long", cli.display()))?
                    .map_err(|e| format!("cannot run {}: {e}", cli.display()))?;
                if !out.status.success() {
                    return Err(format!(
                        "{} status failed: {}",
                        cli.display(),
                        String::from_utf8_lossy(&out.stderr).trim()
                    ));
                }
                Tailnet::parse(&String::from_utf8_lossy(&out.stdout))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailscaleConfig {
    /// UDP port to listen on and to dial.
    pub port: u16,
    pub status: StatusSource,
    /// Addresses to bind. Empty means this device's tailnet IPv4 address
    /// (IPv6 if it has none) on `port`.
    pub bind: Vec<SocketAddr>,
    /// How often to reread the status to watch the tailnet.
    pub poll: Duration,
}

impl Default for TailscaleConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            status: StatusSource::default_command(),
            bind: Vec::new(),
            poll: DEFAULT_POLL,
        }
    }
}

/// One machine on the tailnet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    /// The MagicDNS machine name, such as `mac-studio`.
    pub name: String,
    /// The OS host name, which may differ in case or spelling.
    pub host_name: String,
    pub ips: Vec<IpAddr>,
    pub online: bool,
}

impl Machine {
    /// Matches the machine name, the host name, or a full MagicDNS name.
    fn answers_to(&self, host: &str) -> bool {
        let label = host.split('.').next().unwrap_or_default();
        self.name.eq_ignore_ascii_case(host)
            || self.host_name.eq_ignore_ascii_case(host)
            || self.name.eq_ignore_ascii_case(label)
    }

    /// IPv4 first: every tailnet has it, and it is shorter in logs.
    fn sorted_ips(&self) -> Vec<IpAddr> {
        let mut ips = self.ips.clone();
        ips.sort_by_key(|ip| ip.is_ipv6());
        ips
    }
}

/// What `tailscale status --json` says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tailnet {
    pub me: Machine,
    pub peers: Vec<Machine>,
}

#[derive(Deserialize)]
struct RawStatus {
    #[serde(rename = "BackendState", default)]
    backend_state: String,
    #[serde(rename = "Self")]
    me: Option<RawMachine>,
    #[serde(rename = "Peer", default)]
    peers: Option<HashMap<String, RawMachine>>,
}

#[derive(Deserialize)]
struct RawMachine {
    #[serde(rename = "HostName", default)]
    host_name: String,
    #[serde(rename = "DNSName", default)]
    dns_name: String,
    #[serde(rename = "TailscaleIPs", default)]
    ips: Option<Vec<IpAddr>>,
    #[serde(rename = "Online", default)]
    online: bool,
}

impl From<RawMachine> for Machine {
    fn from(raw: RawMachine) -> Self {
        let label = raw.dns_name.split('.').next().unwrap_or_default();
        let name = if label.is_empty() {
            raw.host_name.to_ascii_lowercase()
        } else {
            label.to_ascii_lowercase()
        };
        Self {
            name,
            host_name: raw.host_name,
            ips: raw.ips.unwrap_or_default(),
            online: raw.online,
        }
    }
}

impl Tailnet {
    pub fn parse(json: &str) -> Result<Self, String> {
        let raw: RawStatus =
            serde_json::from_str(json).map_err(|e| format!("bad tailscale status: {e}"))?;
        if raw.backend_state != "Running" {
            let state = if raw.backend_state.is_empty() {
                "not running"
            } else {
                &raw.backend_state
            };
            return Err(format!("tailscale is {state}; run `tailscale up`"));
        }
        let me = raw.me.ok_or("tailscale status has no Self entry")?.into();
        let mut peers: Vec<Machine> = raw
            .peers
            .unwrap_or_default()
            .into_values()
            .map(Machine::from)
            .collect();
        peers.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Self { me, peers })
    }

    pub fn find(&self, host: &str) -> Option<&Machine> {
        self.peers.iter().find(|m| m.answers_to(host))
    }
}

/// Splits a `peers.toml` `tailnet` value, `host` or `host:port`.
pub fn parse_tailnet(value: &str) -> Result<(String, Option<u16>), String> {
    let (host, port) = match value.rsplit_once(':') {
        Some((host, port)) => {
            let port = port
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| format!("tailnet {value:?}: bad port"))?;
            (host, Some(port))
        }
        None => (value, None),
    };
    let ok = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if !ok {
        return Err(format!(
            "tailnet {value:?} should be a machine name, like \"mac-studio\" or \"mac-studio:7787\""
        ));
    }
    Ok((host.to_string(), port))
}

/// The last `tailscale status`, shared by connection attempts, discovery
/// and the poller. A read that differs from the one before counts as a
/// change of the tailnet.
struct Watch {
    source: StatusSource,
    last: tokio::sync::Mutex<Option<(Instant, Result<Tailnet, String>)>>,
    changes: tokio::sync::watch::Sender<u64>,
}

impl Watch {
    async fn tailnet(&self, max_age: Duration) -> Result<Tailnet, String> {
        let mut last = self.last.lock().await;
        if let Some((at, read)) = &*last
            && at.elapsed() < max_age
        {
            return read.clone();
        }
        let read = self.source.read().await;
        let changed = last.as_ref().is_some_and(|(_, old)| *old != read);
        *last = Some((Instant::now(), read.clone()));
        if changed {
            tracing::debug!("the tailnet changed");
            self.changes.send_modify(|n| *n += 1);
        }
        read
    }
}

/// What the endpoint binds.
#[derive(Clone, Debug)]
enum Bind {
    /// This device's tailnet address, read from the status, on this port.
    Tailnet(u16),
    /// Addresses from the config, which never move.
    Fixed(Vec<SocketAddr>),
}

/// This device's tailnet IPv4 address (IPv6 if it has none) on `port`.
fn self_addr(tailnet: &Tailnet, port: u16) -> Result<SocketAddr, String> {
    let ips = tailnet.me.sorted_ips();
    let ip = ips
        .iter()
        .find(|ip| ip.is_ipv4())
        .or(ips.first())
        .ok_or("this device has no tailnet address")?;
    Ok(SocketAddr::new(*ip, port))
}

pub struct TailscaleTransport {
    quic: Arc<QuicEndpoint>,
    port: u16,
    watch: Arc<Watch>,
    /// Rereads the status and keeps the endpoint bound; see [`tend`].
    poller: tokio::task::JoinHandle<()>,
    /// Ports from `tailnet = "host:port"`, by lowercase host.
    ports: RwLock<HashMap<String, u16>>,
}

impl Drop for TailscaleTransport {
    fn drop(&mut self) {
        self.poller.abort();
    }
}

/// Rereads `tailscale status` every `poll`, and binds the endpoint again
/// when its socket died or this device's tailnet address moved. While the
/// address is missing, it tries again with backoff. A socket dying wakes it
/// at once.
async fn tend(watch: Arc<Watch>, quic: Arc<QuicEndpoint>, bind: Bind, poll: Duration) {
    let mut wait = poll;
    let mut retry = REBIND_BACKOFF.0;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = quic.ended() => {}
        }
        let read = watch.tailnet(Duration::ZERO).await;
        if let Err(e) = &read {
            tracing::debug!("tailscale status: {e}");
        }
        let want = match (&bind, &read) {
            (Bind::Fixed(addrs), _) => Ok(addrs.clone()),
            (Bind::Tailnet(port), Ok(tailnet)) => self_addr(tailnet, *port).map(|a| vec![a]),
            (Bind::Tailnet(_), Err(e)) => Err(e.clone()),
        };
        let moved = matches!(bind, Bind::Tailnet(_))
            && read.as_ref().is_ok_and(|tailnet| {
                !quic
                    .bound()
                    .iter()
                    .any(|addr| tailnet.me.ips.contains(&addr.ip()))
            });
        if quic.alive() && !moved {
            wait = poll;
            retry = REBIND_BACKOFF.0;
            continue;
        }
        let bound = match want {
            Ok(addrs) => quic
                .rebind(&addrs)
                .await
                .map_err(|e| format!("{addrs:?}: {e}")),
            Err(e) => Err(e),
        };
        match bound {
            Ok(()) => {
                wait = poll;
                retry = REBIND_BACKOFF.0;
            }
            Err(e) => {
                if retry == REBIND_BACKOFF.0 {
                    tracing::warn!("tailscale: cannot bind, trying again until it can: {e}");
                } else {
                    tracing::debug!("tailscale: cannot bind yet: {e}");
                }
                wait = retry;
                retry = (retry * 2).min(REBIND_BACKOFF.1);
            }
        }
    }
}

impl TailscaleTransport {
    pub async fn bind(key: SecretKey, config: TailscaleConfig) -> Result<Self, String> {
        let watch = Arc::new(Watch {
            source: config.status.clone(),
            last: tokio::sync::Mutex::new(None),
            changes: tokio::sync::watch::Sender::new(0),
        });
        let bind = if config.bind.is_empty() {
            Bind::Tailnet(config.port)
        } else {
            Bind::Fixed(config.bind.clone())
        };
        let addrs = match &bind {
            Bind::Tailnet(port) => vec![self_addr(&watch.tailnet(Duration::ZERO).await?, *port)?],
            Bind::Fixed(addrs) => addrs.clone(),
        };
        // No relays and no address lookup: Tailscale does both.
        let builder =
            Box::new(|| Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled));
        let quic =
            Arc::new(QuicEndpoint::bind(TransportKind::Tailscale, builder, key, &addrs).await?);
        let poll = config.poll.max(Duration::from_millis(50));
        let poller = tokio::spawn(tend(watch.clone(), quic.clone(), bind, poll));
        Ok(Self {
            quic,
            port: config.port,
            watch,
            poller,
            ports: RwLock::default(),
        })
    }

    /// Closes the endpoint under the transport, as Tailscale going down
    /// does, and leaves the transport to notice. For tests.
    #[doc(hidden)]
    pub async fn kill_socket(&self) {
        self.quic.kill_socket().await;
    }

    fn host_of(peer: &Peer) -> String {
        peer.tailnet
            .as_deref()
            .and_then(|t| parse_tailnet(t).ok())
            .map(|(host, _)| host)
            .unwrap_or_else(|| peer.name.clone())
    }

    fn port_for(&self, host: &str) -> u16 {
        self.ports
            .read()
            .unwrap()
            .get(&host.to_ascii_lowercase())
            .copied()
            .unwrap_or(self.port)
    }
}

#[async_trait]
impl Transport for TailscaleTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Tailscale
    }

    fn set_peers(&self, peers: &PeerList) {
        self.quic.set_peers(peers);
        *self.ports.write().unwrap() = peers
            .peers
            .values()
            .filter_map(|p| parse_tailnet(p.tailnet.as_deref()?).ok())
            .filter_map(|(host, port)| Some((host.to_ascii_lowercase(), port?)))
            .collect();
    }

    /// A peer's machine name is its `tailnet` entry, or else its name in
    /// `peers.toml`.
    fn locate(&self, peer: &Peer) -> Option<PeerId> {
        Some(PeerId::Tailscale {
            host: Self::host_of(peer),
        })
    }

    async fn discover(&self) -> Result<Vec<Discovered>, String> {
        let tailnet = self.watch.tailnet(FRESH).await?;
        Ok(tailnet
            .peers
            .iter()
            .map(|m| {
                let port = self.port_for(&m.name);
                Discovered {
                    id: PeerId::Tailscale {
                        host: m.name.clone(),
                    },
                    name: m.host_name.clone(),
                    online: Some(m.online),
                    addrs: m
                        .sorted_ips()
                        .into_iter()
                        .map(|ip| SocketAddr::new(ip, port))
                        .collect(),
                }
            })
            .collect())
    }

    async fn connect(&self, id: &PeerId, key: EndpointId) -> Result<Connection, ConnectError> {
        let PeerId::Tailscale { host } = id else {
            return Err(ConnectError::new(format!("tailscale cannot reach {id}")));
        };
        let tailnet = self.watch.tailnet(FRESH).await.map_err(ConnectError::new)?;
        let machine = tailnet
            .find(host)
            .ok_or_else(|| ConnectError::new(format!("no machine named {host} on the tailnet")))?;
        let port = self.port_for(host);
        // Tailscale's `Online` lags behind; dial anyway and let QUIC decide.
        let addr = machine
            .sorted_ips()
            .into_iter()
            .fold(EndpointAddr::new(key), |a, ip| {
                a.with_ip_addr(SocketAddr::new(ip, port))
            });
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
            kind: TransportKind::Tailscale,
            bound: self.bound().iter().map(|a| a.to_string()).collect(),
            detail: match &self.watch.source {
                StatusSource::Command(cli) => format!("peers from {} status", cli.display()),
                StatusSource::Fixed(_) => "peers from a fixed status".into(),
            },
        }
    }

    /// Resolves when the tailnet changed or the endpoint was bound again.
    async fn changed(&self) {
        let mut rx = self.watch.changes.subscribe();
        tokio::select! {
            _ = rx.changed() => {}
            _ = self.quic.changed() => {}
        }
    }

    async fn close(&self) {
        self.poller.abort();
        self.quic.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = r#"{
      "Version": "1.80.0",
      "BackendState": "Running",
      "Self": {
        "HostName": "Mac-Studio",
        "DNSName": "mac-studio.tail1234.ts.net.",
        "TailscaleIPs": ["100.101.1.1", "fd7a:115c:a1e0::1"],
        "Online": true
      },
      "Peer": {
        "nodekey:aa": {
          "HostName": "linux-box",
          "DNSName": "linux-box.tail1234.ts.net.",
          "TailscaleIPs": ["fd7a:115c:a1e0::2", "100.101.1.2"],
          "Online": true
        },
        "nodekey:bb": {
          "HostName": "Pixel 9",
          "DNSName": "pixel-9.tail1234.ts.net.",
          "TailscaleIPs": ["100.101.1.3"],
          "Online": false
        }
      }
    }"#;

    #[test]
    fn reads_tailscale_status() {
        let net = Tailnet::parse(STATUS).unwrap();
        assert_eq!(net.me.name, "mac-studio");
        assert_eq!(
            net.me.sorted_ips()[0],
            "100.101.1.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(net.peers.len(), 2);
        let linux = net.find("linux-box").unwrap();
        assert!(linux.online);
        assert_eq!(
            linux.sorted_ips()[0],
            "100.101.1.2".parse::<IpAddr>().unwrap()
        );
        assert_eq!(net.find("PIXEL-9").unwrap().host_name, "Pixel 9");
        assert_eq!(net.find("Pixel 9").unwrap().name, "pixel-9");
        assert!(net.find("nas").is_none());
        assert_eq!(net.find("linux-box.tail1234.ts.net").unwrap(), linux);
    }

    #[test]
    fn a_stopped_tailscale_is_an_error() {
        let err =
            Tailnet::parse(r#"{"BackendState":"Stopped","Self":null,"Peer":null}"#).unwrap_err();
        assert!(err.contains("Stopped"), "{err}");
        assert!(Tailnet::parse("not json").is_err());
    }

    #[test]
    fn tailnet_entries_name_a_machine_and_maybe_a_port() {
        assert_eq!(
            parse_tailnet("mac-studio").unwrap(),
            ("mac-studio".into(), None)
        );
        assert_eq!(
            parse_tailnet("mac-studio:9000").unwrap(),
            ("mac-studio".into(), Some(9000))
        );
        assert_eq!(
            parse_tailnet("mac-studio.tail1234.ts.net").unwrap().0,
            "mac-studio.tail1234.ts.net"
        );
        for bad in ["", ":7787", "mac:0", "mac:x", "two words", "a/b"] {
            assert!(parse_tailnet(bad).is_err(), "accepted {bad:?}");
        }
    }
}
