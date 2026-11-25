//! The same calls over each transport, and over both at once. The
//! Tailscale backend reads a fixed `tailscale status` that puts every
//! machine on loopback, so no test needs Tailscale or the network.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bricolo_mesh::transport::iroh::IrohTransport;
use bricolo_mesh::transport::tailscale::TailscaleTransport;
use bricolo_mesh::transport::{PeerId, Transport};
use bricolo_mesh::{
    IrohConfig, LinkState, Mesh, MeshConfig, Peer, PeerList, SecretKey, StatusSource,
    TailscaleConfig, TransportConfig, TransportKind,
};
use bricolo_providers::speech::SpeechProvider;
use bricolo_providers::{CallError, Caller, LocalOptions, ProviderEvent, Providers};
use bricolo_router::Peers;
use bytes::Bytes;

const WAIT: Duration = Duration::from_secs(10);

/// A `tailscale status --json` with this device and the machines `peers`,
/// all on loopback.
fn status(me: &str, peers: &[&str]) -> StatusSource {
    let machine = |name: &str| {
        format!(
            r#"{{"HostName":"{name}","DNSName":"{name}.tail0000.ts.net.","TailscaleIPs":["127.0.0.1"],"Online":true}}"#
        )
    };
    let peers: Vec<String> = peers
        .iter()
        .enumerate()
        .map(|(i, p)| format!(r#""nodekey:{i}":{}"#, machine(p)))
        .collect();
    StatusSource::Fixed(format!(
        r#"{{"BackendState":"Running","Self":{},"Peer":{{{}}}}}"#,
        machine(me),
        peers.join(",")
    ))
}

fn tailscale(me: &str, peers: &[&str]) -> TailscaleConfig {
    TailscaleConfig {
        port: 1,
        status: status(me, peers),
        bind: vec![SocketAddr::from(([127, 0, 0, 1], 0))],
        poll: Duration::from_secs(60),
    }
}

fn iroh() -> IrohConfig {
    IrohConfig::local_only()
}

/// The Mac: a speech sidecar, here a fake that answers `<format>:<bytes>`.
#[derive(Default)]
struct FakeSpeech(Mutex<Vec<String>>);

#[async_trait]
impl SpeechProvider for FakeSpeech {
    async fn transcribe(
        &self,
        caller: &Caller,
        audio: Vec<u8>,
        format: String,
    ) -> Result<String, CallError> {
        self.0.lock().unwrap().push(caller.plugin.clone());
        Ok(format!("{format}:{}", String::from_utf8_lossy(&audio)))
    }
}

fn serve_speech(mesh: &Mesh, speech: Arc<FakeSpeech>) {
    let dir = std::env::temp_dir();
    let sink: bricolo_providers::EventSink = Arc::new(|_: &str, _: ProviderEvent| {});
    let local = Providers::local(LocalOptions::new(dir), sink);
    mesh.serve(Providers { speech, ..local });
}

async fn start(transports: Vec<TransportConfig>, key: SecretKey) -> Mesh {
    let config = MeshConfig {
        transports,
        ..MeshConfig::local_only()
    };
    Mesh::start(key, PeerList::default(), config).await.unwrap()
}

/// `on` trusts `other` as `name`, with its iroh address and its tailnet
/// machine on whatever port it bound.
fn trust(on: &Mesh, name: &str, other: &Mesh, allow: &[&str]) {
    let mut peer = Peer::new(name, other.id(), allow);
    peer.addrs = other.bound_on(TransportKind::Iroh);
    if let Some(port) = other.bound_on(TransportKind::Tailscale).first() {
        peer.tailnet = Some(format!("{name}:{}", port.port()));
    }
    let mut list = PeerList::default();
    list.insert(peer);
    on.set_peers(list);
}

/// The acceptance scenario: the Linux node transcribes on the Mac. The
/// calling code is the same whatever `transports` holds.
async fn linux_uses_mac_speech(transports: impl Fn(&str, &str) -> Vec<TransportConfig>) {
    let mac = start(transports("mac", "linux"), SecretKey::generate()).await;
    let linux = start(transports("linux", "mac"), SecretKey::generate()).await;
    let speech = Arc::new(FakeSpeech::default());
    serve_speech(&mac, speech.clone());
    trust(&linux, "mac", &mac, &[]);
    trust(&mac, "linux", &linux, &["speech"]);
    linux.wait_connected("mac", WAIT).await.unwrap();

    let text = linux
        .speech("mac")
        .expect("mac is connected")
        .transcribe(&Caller::new("dictation"), b"hello".to_vec(), "wav".into())
        .await
        .unwrap();
    assert_eq!(text, "wav:hello");
    assert_eq!(*speech.0.lock().unwrap(), ["linux/dictation"]);
    let want = match transports("x", "y")[0] {
        TransportConfig::Iroh(_) => TransportKind::Iroh,
        TransportConfig::Tailscale(_) => TransportKind::Tailscale,
    };
    assert_eq!(linux.status().peers[0].via, Some(want));
    tokio::join!(linux.close(), mac.close());
}

#[tokio::test(flavor = "multi_thread")]
async fn speech_runs_on_the_mac_over_iroh() {
    linux_uses_mac_speech(|_, _| vec![TransportConfig::Iroh(iroh())]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn speech_runs_on_the_mac_over_tailscale() {
    linux_uses_mac_speech(|me, peer| vec![TransportConfig::Tailscale(tailscale(me, &[peer]))])
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_both_the_first_transport_that_reaches_the_peer_wins() {
    linux_uses_mac_speech(|me, peer| {
        vec![
            TransportConfig::Iroh(iroh()),
            TransportConfig::Tailscale(tailscale(me, &[peer])),
        ]
    })
    .await;
    linux_uses_mac_speech(|me, peer| {
        vec![
            TransportConfig::Tailscale(tailscale(me, &[peer])),
            TransportConfig::Iroh(iroh()),
        ]
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_iroh_cannot_reach_falls_to_tailscale() {
    let both = |me: &str, peer: &str| {
        vec![
            TransportConfig::Iroh(iroh()),
            TransportConfig::Tailscale(tailscale(me, &[peer])),
        ]
    };
    let mac = start(both("mac", "linux"), SecretKey::generate()).await;
    let linux = start(both("linux", "mac"), SecretKey::generate()).await;
    serve_speech(&mac, Arc::new(FakeSpeech::default()));
    trust(&linux, "mac", &mac, &[]);
    trust(&mac, "linux", &linux, &["speech"]);
    // Without relays and without addrs, iroh has no way to the Mac.
    let mut list = PeerList::default();
    let mut peer = Peer::new("mac", mac.id(), &[]);
    peer.tailnet = Some(format!(
        "mac:{}",
        mac.bound_on(TransportKind::Tailscale)[0].port()
    ));
    list.insert(peer);
    linux.set_peers(list);
    linux.wait_connected("mac", WAIT).await.unwrap();
    assert_eq!(linux.status().peers[0].via, Some(TransportKind::Tailscale));
    let status = linux.status();
    assert_eq!(status.transports.len(), 2);
    assert_eq!(status.transports[1].kind, TransportKind::Tailscale);
    tokio::join!(linux.close(), mac.close());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tailnet_machine_without_a_trusted_key_is_refused() {
    let ts = |me: &str, peer: &str| vec![TransportConfig::Tailscale(tailscale(me, &[peer]))];
    let mac = start(ts("mac", "stranger"), SecretKey::generate()).await;
    let linux = start(ts("linux", "mac"), SecretKey::generate()).await;
    let stranger = start(ts("stranger", "mac"), SecretKey::generate()).await;
    serve_speech(&mac, Arc::new(FakeSpeech::default()));
    trust(&mac, "linux", &linux, &["speech"]);
    // The stranger is on the tailnet and knows the Mac's key, but the Mac
    // does not list the stranger's key.
    trust(&stranger, "mac", &mac, &[]);
    assert_eq!(
        stranger.wait_connected("mac", WAIT).await,
        Err(CallError::Denied)
    );
    let status = stranger.status();
    assert_eq!(status.peers[0].state, LinkState::Refused);
    assert!(
        status.peers[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("not in peers.toml"),
        "{status:?}"
    );
    tokio::join!(mac.close(), linux.close(), stranger.close());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_machine_is_offline_with_a_reason() {
    let linux = start(
        vec![TransportConfig::Tailscale(tailscale("linux", &[]))],
        SecretKey::generate(),
    )
    .await;
    let mut list = PeerList::default();
    list.insert(Peer::new("nas", SecretKey::generate().public(), &[]));
    linux.set_peers(list);
    assert_eq!(
        linux
            .wait_connected("nas", Duration::from_millis(500))
            .await,
        Err(CallError::Unavailable)
    );
    let status = linux.status();
    let error = status.peers[0].last_error.clone().unwrap_or_default();
    assert!(error.contains("no machine named nas"), "{error}");
    linux.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tailscale_discovers_the_tailnet() {
    let linux = start(
        vec![TransportConfig::Tailscale(tailscale(
            "linux",
            &["mac", "nas"],
        ))],
        SecretKey::generate(),
    )
    .await;
    let found = linux.discover().await;
    assert_eq!(found.len(), 1);
    let (kind, machines) = &found[0];
    assert_eq!(*kind, TransportKind::Tailscale);
    let machines = machines.as_ref().unwrap();
    let ids: Vec<_> = machines.iter().map(|m| m.id.clone()).collect();
    assert_eq!(
        ids,
        [
            PeerId::Tailscale { host: "mac".into() },
            PeerId::Tailscale { host: "nas".into() }
        ]
    );
    assert_eq!(machines[0].online, Some(true));
    linux.close().await;
}

/// Reliable streams and unreliable datagrams, straight on each transport.
async fn streams_and_datagrams(
    a: Arc<dyn Transport>,
    b: Arc<dyn Transport>,
    a_key: SecretKey,
    b_peer: Peer,
) {
    let mut a_trusts = PeerList::default();
    a_trusts.insert(b_peer.clone());
    a.set_peers(&a_trusts);
    let mut b_trusts = PeerList::default();
    b_trusts.insert(Peer::new("a", a_key.public(), &[]));
    b.set_peers(&b_trusts);

    let id = a.locate(&b_peer).expect("a can reach b");
    let server = tokio::spawn({
        let b = b.clone();
        async move {
            let conn = b.accept().await.unwrap();
            let (mut send, mut recv) = conn.accept_bi().await.unwrap();
            let mut buf = [0u8; 4];
            recv.read_exact(&mut buf).await.unwrap();
            send.write_all(&buf).await.unwrap();
            send.finish();
            let datagram = conn.read_datagram().await.unwrap();
            conn.send_datagram(datagram).unwrap();
            conn.closed().await
        }
    });
    let conn = tokio::time::timeout(WAIT, a.connect(&id, b_peer.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(conn.remote_key(), b_peer.id);
    assert_eq!(conn.transport(), a.kind());
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    send.write_all(b"ping").await.unwrap();
    let mut buf = [0u8; 4];
    recv.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"ping");

    assert!(conn.max_datagram_size().unwrap() >= 1000);
    // Datagrams may be lost; send until one comes back.
    let echoed = tokio::time::timeout(WAIT, async {
        loop {
            conn.send_datagram(Bytes::from_static(b"frame")).unwrap();
            if let Ok(Ok(d)) =
                tokio::time::timeout(Duration::from_millis(200), conn.read_datagram()).await
            {
                return d;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(&echoed[..], b"frame");
    conn.close(0, b"done");
    server.await.unwrap();
    tokio::join!(a.close(), b.close());
}

#[tokio::test(flavor = "multi_thread")]
async fn iroh_carries_streams_and_datagrams() {
    let (a_key, b_key) = (SecretKey::generate(), SecretKey::generate());
    let a = IrohTransport::bind(a_key.clone(), iroh()).await.unwrap();
    let b = IrohTransport::bind(b_key.clone(), iroh()).await.unwrap();
    let mut b_peer = Peer::new("b", b_key.public(), &[]);
    b_peer.addrs = b.bound();
    streams_and_datagrams(Arc::new(a), Arc::new(b), a_key, b_peer).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tailscale_carries_streams_and_datagrams() {
    let (a_key, b_key) = (SecretKey::generate(), SecretKey::generate());
    let a = TailscaleTransport::bind(a_key.clone(), tailscale("a", &["b"]))
        .await
        .unwrap();
    let b = TailscaleTransport::bind(b_key.clone(), tailscale("b", &["a"]))
        .await
        .unwrap();
    let mut b_peer = Peer::new("b", b_key.public(), &[]);
    b_peer.tailnet = Some(format!("b:{}", b.bound()[0].port()));
    streams_and_datagrams(Arc::new(a), Arc::new(b), a_key, b_peer).await;
}

/// Collects every datagram it receives.
struct Inbox(tokio::sync::mpsc::UnboundedSender<(String, Bytes)>);

impl bricolo_mesh::DatagramHandler for Inbox {
    fn receive(&self, peer: &str, payload: Bytes) {
        let _ = self.0.send((peer.to_string(), payload));
    }
}

/// A store that holds one bundle.
struct OneBundle(String, Vec<u8>);

impl bricolo_mesh::PluginStore for OneBundle {
    fn bundle(&self, sha256: &str) -> Result<Vec<u8>, CallError> {
        if sha256 == self.0 {
            Ok(self.1.clone())
        } else {
            Err(CallError::Unavailable)
        }
    }
}

/// Datagrams and plugin sharing ride the same connections, whatever the
/// transport.
async fn datagrams_and_sharing(transports: impl Fn(&str, &str) -> Vec<TransportConfig>) {
    let mac = start(transports("mac", "linux"), SecretKey::generate()).await;
    let linux = start(transports("linux", "mac"), SecretKey::generate()).await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    mac.register_datagrams("pointer", Arc::new(Inbox(tx.clone())));
    mac.register_datagrams("secret", Arc::new(Inbox(tx)));
    let sha = "ab".repeat(32);
    // Bigger than one media frame, so it arrives in several.
    let bundle: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    mac.share_plugins(Arc::new(OneBundle(sha.clone(), bundle.clone())));
    trust(&linux, "mac", &mac, &[]);
    trust(&mac, "linux", &linux, &["pointer", "plugins"]);
    linux.wait_connected("mac", WAIT).await.unwrap();

    // Small: a real datagram, which may be lost, so send until one lands.
    let got = tokio::time::timeout(WAIT, async {
        loop {
            linux.send_datagram("mac", "pointer", b"x=1").await.unwrap();
            if let Ok(Some(d)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                return d;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(got, ("linux".to_string(), Bytes::from_static(b"x=1")));
    // Too big for a datagram: it goes on a stream and arrives whole.
    let big = vec![7u8; 100_000];
    linux.send_datagram("mac", "pointer", &big).await.unwrap();
    let (_, payload) = loop {
        let d = tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .unwrap();
        if d.1.len() == big.len() {
            break d;
        }
    };
    assert_eq!(&payload[..], &big[..]);
    // Not in linux's allow list on the Mac: dropped.
    linux.send_datagram("mac", "secret", &big).await.unwrap();
    linux.send_datagram("mac", "secret", b"s").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    while let Ok(d) = rx.try_recv() {
        assert_ne!(&d.1[..], b"s", "a secret datagram got through");
        assert_ne!(d.1.len(), big.len(), "a secret stream datagram got through");
    }

    let caller = Caller::new("bricolo-install");
    assert_eq!(
        linux.fetch_plugin("mac", &sha, &caller).await.unwrap(),
        bundle
    );
    assert_eq!(
        linux.fetch_plugin("mac", &"cd".repeat(32), &caller).await,
        Err(CallError::Unavailable)
    );
    assert!(matches!(
        linux.fetch_plugin("mac", "nope", &caller).await,
        Err(CallError::Failed(_))
    ));
    // The Mac may not fetch from linux: linux does not allow `plugins`.
    linux.share_plugins(Arc::new(OneBundle(sha.clone(), bundle.clone())));
    trust(&mac, "linux", &linux, &["pointer", "plugins"]);
    trust(&linux, "mac", &mac, &[]);
    mac.wait_connected("linux", WAIT).await.unwrap();
    assert_eq!(
        mac.fetch_plugin("linux", &sha, &caller).await,
        Err(CallError::Denied)
    );
    tokio::join!(linux.close(), mac.close());
}

#[tokio::test(flavor = "multi_thread")]
async fn datagrams_and_plugins_travel_over_iroh() {
    datagrams_and_sharing(|_, _| vec![TransportConfig::Iroh(iroh())]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn datagrams_and_plugins_travel_over_tailscale() {
    datagrams_and_sharing(|me, peer| vec![TransportConfig::Tailscale(tailscale(me, &[peer]))])
        .await;
}

/// A `tailscale` CLI whose `status --json` prints `file`.
#[cfg(unix)]
fn fake_cli(dir: &std::path::Path, file: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let cli = dir.join("tailscale");
    std::fs::write(&cli, format!("#!/bin/sh\ncat '{}'\n", file.display())).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    cli
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_machine_joining_the_tailnet_is_reached_without_waiting_out_the_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("status.json");
    let fixed = |s: StatusSource| match s {
        StatusSource::Fixed(json) => json,
        StatusSource::Command(_) => unreachable!(),
    };
    // At first the Mac is not on linux's tailnet.
    std::fs::write(&file, fixed(status("linux", &[]))).unwrap();
    let linux_config = TailscaleConfig {
        status: StatusSource::Command(fake_cli(dir.path(), &file)),
        poll: Duration::from_millis(100),
        ..tailscale("linux", &[])
    };
    let mac = start(
        vec![TransportConfig::Tailscale(tailscale("mac", &["linux"]))],
        SecretKey::generate(),
    )
    .await;
    let linux = start(
        vec![TransportConfig::Tailscale(linux_config)],
        SecretKey::generate(),
    )
    .await;
    serve_speech(&mac, Arc::new(FakeSpeech::default()));
    trust(&linux, "mac", &mac, &[]);
    trust(&mac, "linux", &linux, &["speech"]);

    // Offline long enough that the next attempt is seconds away: 0.5, 1
    // and 2 s pauses have passed, and the next is 4 s.
    tokio::time::sleep(Duration::from_millis(3700)).await;
    let error = linux.status().peers[0]
        .last_error
        .clone()
        .unwrap_or_default();
    assert!(error.contains("no machine named mac"), "{error}");

    std::fs::write(&file, fixed(status("linux", &["mac"]))).unwrap();
    let joined = std::time::Instant::now();
    linux.wait_connected("mac", WAIT).await.unwrap();
    assert!(
        joined.elapsed() < Duration::from_millis(1500),
        "took {:?}; the backoff was not cut short",
        joined.elapsed()
    );
    tokio::join!(linux.close(), mac.close());
}

/// Linux's socket dies under its transport, as when Tailscale goes down
/// and takes the tailnet address. The transport binds again on the same
/// port and the link comes back without restarting the node.
async fn link_comes_back_after_the_socket_dies(
    mac: Vec<TransportConfig>,
    transport: Arc<dyn Transport>,
    key: SecretKey,
    kill: impl std::future::Future<Output = ()>,
) {
    let mac = start(mac, SecretKey::generate()).await;
    let linux = Mesh::with_transports(
        key.public(),
        vec![transport.clone()],
        PeerList::default(),
        Duration::from_secs(30),
        false,
    )
    .unwrap();
    serve_speech(&mac, Arc::new(FakeSpeech::default()));
    trust(&linux, "mac", &mac, &[]);
    trust(&mac, "linux", &linux, &["speech"]);
    linux.wait_connected("mac", WAIT).await.unwrap();
    let bound = transport.bound();

    kill.await;
    tokio::time::timeout(WAIT, async {
        while linux.status().peers[0].state == LinkState::Connected {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the link never dropped");
    linux.wait_connected("mac", WAIT).await.unwrap();
    assert_eq!(transport.bound(), bound);
    assert_eq!(linux.status().transports[0].bound.len(), bound.len());
    let text = linux
        .speech("mac")
        .expect("mac is connected")
        .transcribe(&Caller::new("dictation"), b"again".to_vec(), "wav".into())
        .await
        .unwrap();
    assert_eq!(text, "wav:again");
    tokio::join!(linux.close(), mac.close());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dead_tailscale_socket_is_bound_again_and_the_link_comes_back() {
    let key = SecretKey::generate();
    let transport = Arc::new(
        TailscaleTransport::bind(key.clone(), tailscale("linux", &["mac"]))
            .await
            .unwrap(),
    );
    link_comes_back_after_the_socket_dies(
        vec![TransportConfig::Tailscale(tailscale("mac", &["linux"]))],
        transport.clone(),
        key,
        async move { transport.kill_socket().await },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dead_iroh_socket_is_bound_again_and_the_link_comes_back() {
    let key = SecretKey::generate();
    let transport = Arc::new(IrohTransport::bind(key.clone(), iroh()).await.unwrap());
    link_comes_back_after_the_socket_dies(
        vec![TransportConfig::Iroh(iroh())],
        transport.clone(),
        key,
        async move { transport.kill_socket().await },
    )
    .await;
}
