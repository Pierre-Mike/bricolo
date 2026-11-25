//! Two or three nodes in one process, on loopback, with relays and
//! discovery off. No test touches the internet.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bricolo_mesh::{
    IrohConfig, LinkState, MediaStream, Mesh, MeshConfig, Peer, PeerList, SecretKey, StreamHandler,
    TransportConfig, TransportKind,
};
use bricolo_providers::disk::DiskGrant;
use bricolo_providers::notify::NotifyProvider;
use bricolo_providers::speech::SpeechProvider;
use bricolo_providers::tts::{NoTts, TtsProvider, TtsStatus};
use bricolo_providers::{CallError, Caller, LocalOptions, ProviderEvent, Providers};
use bricolo_router::{Peers, Rules, route};
use tokio::sync::Notify;

const WAIT: Duration = Duration::from_secs(10);

/// Records what it was asked to say. "hang" never finishes, and counts
/// how often a hung call's future was dropped.
#[derive(Default)]
struct FakeTts {
    said: Mutex<Vec<(String, String)>>,
    dropped: Arc<AtomicUsize>,
    dropped_signal: Arc<Notify>,
}

struct OnDrop(Arc<AtomicUsize>, Arc<Notify>);

impl Drop for OnDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
        self.1.notify_waiters();
        self.1.notify_one();
    }
}

#[async_trait]
impl TtsProvider for FakeTts {
    async fn speak(
        &self,
        caller: &Caller,
        text: String,
        _: Option<String>,
    ) -> Result<(), CallError> {
        self.said
            .lock()
            .unwrap()
            .push((caller.plugin.clone(), text.clone()));
        if text == "hang" {
            let _guard = OnDrop(self.dropped.clone(), self.dropped_signal.clone());
            std::future::pending::<()>().await;
        }
        Ok(())
    }
    async fn stop(&self, _: &Caller) -> Result<(), CallError> {
        Ok(())
    }
    async fn status(&self, _: &Caller) -> Result<TtsStatus, CallError> {
        Ok(TtsStatus {
            speaking: false,
            queued: self.said.lock().unwrap().len() as u32,
            voice: Some("alba".into()),
        })
    }
}

impl FakeTts {
    async fn wait_dropped(&self, within: Duration) -> bool {
        let until = Instant::now() + within;
        while self.dropped.load(Ordering::SeqCst) == 0 {
            if Instant::now() >= until {
                return false;
            }
            let _ = tokio::time::timeout(Duration::from_millis(50), self.dropped_signal.notified())
                .await;
        }
        true
    }
}

/// Records (caller, title, body).
#[derive(Default)]
struct FakeNotify(Mutex<Vec<(String, String, String)>>);

#[async_trait]
impl NotifyProvider for FakeNotify {
    async fn show(&self, caller: &Caller, title: String, body: String) -> Result<(), CallError> {
        self.0
            .lock()
            .unwrap()
            .push((caller.plugin.clone(), title, body));
        Ok(())
    }
}

/// Answers `<format>:<bytes>`, and records each call's audio.
#[derive(Default)]
struct FakeSpeech(Mutex<Vec<Vec<u8>>>);

#[async_trait]
impl SpeechProvider for FakeSpeech {
    async fn transcribe(
        &self,
        _: &Caller,
        audio: Vec<u8>,
        format: String,
    ) -> Result<String, CallError> {
        let answer = format!("{format}:{}", String::from_utf8_lossy(&audio));
        self.0.lock().unwrap().push(audio);
        Ok(answer)
    }
}

/// Echoes every frame back, then finishes.
struct Echo;

#[async_trait]
impl StreamHandler for Echo {
    async fn serve(&self, _: Caller, _: String, mut stream: MediaStream) -> Result<(), CallError> {
        while let Some(chunk) = stream.rx.recv().await? {
            stream.tx.send(&chunk).await?;
        }
        stream.tx.finish();
        Ok(())
    }
}

fn providers(dir: &std::path::Path, tts: Arc<dyn TtsProvider>) -> Providers {
    let sink: bricolo_providers::EventSink = Arc::new(|_: &str, _: ProviderEvent| {});
    let local = Providers::local(LocalOptions::new(dir.to_path_buf()), sink);
    Providers { tts, ..local }
}

async fn node(config: MeshConfig) -> Mesh {
    Mesh::start(SecretKey::generate(), PeerList::default(), config)
        .await
        .unwrap()
}

fn addr(mesh: &Mesh) -> SocketAddr {
    mesh.bound()[0]
}

/// `on` trusts `other` under `name` and lets it call `allow`.
fn trust(on: &Mesh, name: &str, other: &Mesh, allow: &[&str]) {
    let mut list = PeerList::default();
    let mut peer = Peer::new(name, other.id(), allow);
    peer.addrs = vec![addr(other)];
    list.insert(peer);
    on.set_peers(list);
}

fn caller() -> Caller {
    Caller::new("test")
}

#[tokio::test(flavor = "multi_thread")]
async fn allowed_calls_run_on_the_peer() {
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().canonicalize().unwrap();
    std::fs::write(shared.join("note.txt"), "from b").unwrap();
    let grant = format!("disk:{}", shared.display());

    let a = node(MeshConfig::local_only()).await;
    let b = node(MeshConfig::local_only()).await;
    let tts = Arc::new(FakeTts::default());
    b.serve(providers(dir.path(), tts.clone()));
    trust(&a, "b", &b, &[]);
    trust(&b, "a", &a, &["tts", &grant]);
    a.wait_connected("b", WAIT).await.unwrap();

    let remote_tts = a.tts("b").expect("b is connected");
    remote_tts
        .speak(&caller(), "hello".into(), Some("alba".into()))
        .await
        .unwrap();
    assert_eq!(
        *tts.said.lock().unwrap(),
        [("a/test".to_string(), "hello".to_string())]
    );

    // The calling plugin needs its own grant for the path, and the peer
    // applies the grant it gave in its peers.toml.
    let path = shared.join("note.txt").to_string_lossy().into_owned();
    let with_grant = Caller {
        plugin: "test".into(),
        disk: vec![DiskGrant {
            root: shared.clone(),
            write: true,
        }],
        ..Caller::default()
    };
    let disk = a.disk("b").unwrap();
    assert_eq!(
        disk.read(&with_grant, path.clone()).await.unwrap(),
        b"from b"
    );
    assert_eq!(
        disk.read(&caller(), path.clone()).await,
        Err(CallError::Denied),
        "a plugin without the grant is stopped on its own node"
    );
    let outside = Caller {
        plugin: "test".into(),
        disk: vec![DiskGrant {
            root: "/".into(),
            write: false,
        }],
        ..Caller::default()
    };
    assert_eq!(
        disk.read(&outside, "/etc/hosts".into()).await,
        Err(CallError::Denied),
        "the peer refuses paths outside the grant it gave"
    );

    assert_eq!(a.status().peers[0].via, Some(TransportKind::Iroh));
    let status = b.status();
    assert_eq!(status.peers[0].name, "a");
    assert_eq!(status.peers[0].incoming, 1);
    assert!(status.peers[0].last_seen.is_some());
    // Close together, so each side acknowledges the other's close.
    tokio::join!(a.close(), b.close());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unlisted_peer_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let a = node(MeshConfig::local_only()).await;
    let b = node(MeshConfig::local_only()).await;
    let stranger = node(MeshConfig::local_only()).await;
    let tts = Arc::new(FakeTts::default());
    b.serve(providers(dir.path(), tts.clone()));
    // b trusts only a. The stranger lists b and tries anyway.
    trust(&b, "a", &a, &["tts"]);
    trust(&stranger, "b", &b, &[]);

    assert_eq!(
        stranger.wait_connected("b", WAIT).await,
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
    assert!(stranger.tts("b").is_none());
    assert_eq!(b.status().peers[0].incoming, 0);
    assert!(tts.said.lock().unwrap().is_empty());
    tokio::join!(a.close(), b.close(), stranger.close());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_outside_the_allow_list_is_denied() {
    let dir = tempfile::tempdir().unwrap();
    let a = node(MeshConfig::local_only()).await;
    let b = node(MeshConfig::local_only()).await;
    let tts = Arc::new(FakeTts::default());
    b.serve(providers(dir.path(), tts.clone()));
    trust(&a, "b", &b, &[]);
    trust(&b, "a", &a, &["tts"]);
    a.wait_connected("b", WAIT).await.unwrap();

    let clip = a.clipboard("b").unwrap();
    assert_eq!(clip.get(&caller()).await, Err(CallError::Denied));
    let speech = a.speech("b").unwrap();
    assert_eq!(
        speech.transcribe(&caller(), vec![1, 2], "wav".into()).await,
        Err(CallError::Denied)
    );
    let any = Caller {
        plugin: "test".into(),
        disk: vec![DiskGrant {
            root: "/".into(),
            write: true,
        }],
        ..Caller::default()
    };
    assert_eq!(
        a.disk("b").unwrap().list(&any, "/".into()).await,
        Err(CallError::Denied)
    );
    assert!(matches!(
        a.open_stream("b", "stream-test", "echo", &caller()).await,
        Err(CallError::Denied)
    ));
    // The allowed capability still works on the same connection.
    a.tts("b")
        .unwrap()
        .speak(&caller(), "ok".into(), None)
        .await
        .unwrap();
    tokio::join!(a.close(), b.close());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unavailable_or_offline_peer_falls_back_to_local() {
    let dir = tempfile::tempdir().unwrap();
    let a = node(MeshConfig::local_only()).await;
    let b = node(MeshConfig::local_only()).await;
    // b has no text to speech: it answers unavailable.
    b.serve(providers(dir.path(), Arc::new(NoTts)));
    trust(&a, "b", &b, &[]);
    trust(&b, "a", &a, &["tts"]);
    a.wait_connected("b", WAIT).await.unwrap();

    let local_tts = Arc::new(FakeTts::default());
    let rules = Rules::parse("timeout_secs = 5\n[routes]\ntts = [\"b\", \"local\"]").unwrap();
    let routed = route(
        &rules,
        providers(dir.path(), local_tts.clone()),
        Arc::new(a.clone()),
    );
    routed
        .tts
        .speak(&caller(), "first".into(), None)
        .await
        .unwrap();
    assert_eq!(local_tts.said.lock().unwrap().len(), 1);

    // b goes away: a notices, and the router skips b at once.
    b.close().await;
    let until = Instant::now() + WAIT;
    while a.tts("b").is_some() {
        assert!(Instant::now() < until, "a never noticed b leaving");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let started = Instant::now();
    routed
        .tts
        .speak(&caller(), "second".into(), None)
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(local_tts.said.lock().unwrap().len(), 2);
    a.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deadline_produces_timeout_and_stops_the_peer() {
    let dir = tempfile::tempdir().unwrap();
    let a = node(MeshConfig {
        call_timeout: Duration::from_millis(300),
        ..MeshConfig::local_only()
    })
    .await;
    let b = node(MeshConfig::local_only()).await;
    let tts = Arc::new(FakeTts::default());
    b.serve(providers(dir.path(), tts.clone()));
    trust(&a, "b", &b, &[]);
    trust(&b, "a", &a, &["tts"]);
    a.wait_connected("b", WAIT).await.unwrap();

    let started = Instant::now();
    let result = a
        .tts("b")
        .unwrap()
        .speak(&caller(), "hang".into(), None)
        .await;
    assert_eq!(result, Err(CallError::Timeout));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(tts.wait_dropped(WAIT).await, "b kept running the call");
    tokio::join!(a.close(), b.close());
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_call_cancels_it_on_the_peer() {
    let dir = tempfile::tempdir().unwrap();
    let a = node(MeshConfig::local_only()).await;
    let b = node(MeshConfig::local_only()).await;
    let tts = Arc::new(FakeTts::default());
    b.serve(providers(dir.path(), tts.clone()));
    trust(&a, "b", &b, &[]);
    trust(&b, "a", &a, &["tts"]);
    a.wait_connected("b", WAIT).await.unwrap();

    let remote = a.tts("b").unwrap();
    let c = caller();
    let call = remote.speak(&c, "hang".into(), None);
    // The call is still running on b when the caller gives up.
    assert!(
        tokio::time::timeout(Duration::from_millis(300), call)
            .await
            .is_err()
    );
    assert_eq!(tts.said.lock().unwrap().len(), 1, "the call reached b");
    assert!(tts.wait_dropped(WAIT).await, "b kept running the call");
    tokio::join!(a.close(), b.close());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_stream_does_not_block_short_calls() {
    let dir = tempfile::tempdir().unwrap();
    let a = node(MeshConfig::local_only()).await;
    let b = node(MeshConfig::local_only()).await;
    let tts = Arc::new(FakeTts::default());
    b.serve(providers(dir.path(), tts.clone()));
    b.register_stream("stream-test", Arc::new(Echo));
    trust(&a, "b", &b, &[]);
    trust(&b, "a", &a, &["tts", "stream-test"]);
    a.wait_connected("b", WAIT).await.unwrap();

    let MediaStream { mut tx, mut rx } = a
        .open_stream("b", "stream-test", "echo", &caller())
        .await
        .unwrap();
    const FRAMES: usize = 400;
    const SIZE: usize = 64 * 1024;
    let writer = tokio::spawn(async move {
        for i in 0..FRAMES {
            tx.send(&vec![(i % 251) as u8; SIZE]).await.unwrap();
        }
        tx.finish();
    });
    let reader = tokio::spawn(async move {
        let mut frames = 0;
        while let Some(chunk) = rx.recv().await.unwrap() {
            assert_eq!(chunk.len(), SIZE);
            assert!(chunk.iter().all(|&b| b == (frames % 251) as u8));
            frames += 1;
            // A slow consumer, so the stream stays busy during the calls.
            if frames % 50 == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        frames
    });

    let remote = a.tts("b").unwrap();
    let mut slowest = Duration::ZERO;
    for i in 0..20 {
        let started = Instant::now();
        remote
            .speak(&caller(), format!("call {i}"), None)
            .await
            .unwrap();
        slowest = slowest.max(started.elapsed());
    }
    assert!(
        !reader.is_finished(),
        "the stream ended before the calls did; make it longer"
    );
    assert!(
        slowest < Duration::from_millis(500),
        "slowest call {slowest:?}"
    );
    writer.await.unwrap();
    assert_eq!(reader.await.unwrap(), FRAMES);
    assert_eq!(tts.said.lock().unwrap().len(), 20);
    tokio::join!(a.close(), b.close());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_peer_is_reconnected() {
    let dir = tempfile::tempdir().unwrap();
    let a = node(MeshConfig::local_only()).await;
    let b_key = SecretKey::generate();
    let b = Mesh::start(b_key.clone(), PeerList::default(), MeshConfig::local_only())
        .await
        .unwrap();
    let b_addr = addr(&b);
    trust(&a, "b", &b, &[]);
    trust(&b, "a", &a, &["tts"]);
    a.wait_connected("b", WAIT).await.unwrap();

    b.close().await;
    drop(b);
    let until = Instant::now() + WAIT;
    while a.status().peers[0].state == LinkState::Connected {
        assert!(Instant::now() < until, "a never noticed b leaving");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The same node comes back on the same address, once the old socket
    // is released.
    let config = MeshConfig {
        transports: vec![TransportConfig::Iroh(IrohConfig {
            relays: false,
            bind: vec![b_addr],
        })],
        ..MeshConfig::local_only()
    };
    let b = loop {
        match Mesh::start(b_key.clone(), PeerList::default(), config.clone()).await {
            Ok(b) => break b,
            Err(e) => {
                assert!(Instant::now() < until, "cannot bind {b_addr} again: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };
    b.serve(providers(dir.path(), Arc::new(FakeTts::default())));
    trust(&b, "a", &a, &["tts"]);
    a.wait_connected("b", WAIT).await.unwrap();
    a.tts("b")
        .unwrap()
        .speak(&caller(), "back".into(), None)
        .await
        .unwrap();
    tokio::join!(a.close(), b.close());
}

#[tokio::test(flavor = "multi_thread")]
async fn notify_tts_status_and_files_for_speech_reach_the_peer() {
    let dir = tempfile::tempdir().unwrap();
    let here = dir.path().canonicalize().unwrap();
    std::fs::write(here.join("clip.m4a"), "audio").unwrap();

    let a = node(MeshConfig::local_only()).await;
    let b = node(MeshConfig::local_only()).await;
    let tts = Arc::new(FakeTts::default());
    let notify = Arc::new(FakeNotify::default());
    let speech = Arc::new(FakeSpeech::default());
    b.serve(Providers {
        notify: notify.clone(),
        speech: speech.clone(),
        ..providers(dir.path(), tts.clone())
    });
    trust(&a, "b", &b, &[]);
    trust(&b, "a", &a, &["tts", "speech", "notify"]);
    a.wait_connected("b", WAIT).await.unwrap();

    a.notify("b")
        .unwrap()
        .show(&caller(), "Title".into(), "Body".into())
        .await
        .unwrap();
    assert_eq!(
        *notify.0.lock().unwrap(),
        [(
            "a/test".to_string(),
            "Title".to_string(),
            "Body".to_string()
        )]
    );

    let remote_tts = a.tts("b").unwrap();
    remote_tts
        .speak(&caller(), "one".into(), None)
        .await
        .unwrap();
    assert_eq!(
        remote_tts.status(&caller()).await.unwrap(),
        TtsStatus {
            speaking: false,
            queued: 1,
            voice: Some("alba".into())
        }
    );

    // The file is read on the calling node, inside the plugin's own grant;
    // the peer only sees bytes, so it gets no `disk:` grant here.
    let remote_speech = a.speech("b").unwrap();
    let path = here.join("clip.m4a").to_string_lossy().into_owned();
    let reader = Caller {
        plugin: "test".into(),
        disk: vec![DiskGrant {
            root: here.clone(),
            write: false,
        }],
        ..Caller::default()
    };
    assert_eq!(
        remote_speech.transcribe_file(&reader, path.clone()).await,
        Ok("m4a:audio".to_string())
    );
    assert_eq!(
        remote_speech.transcribe_file(&caller(), path).await,
        Err(CallError::Denied)
    );
    assert_eq!(speech.0.lock().unwrap().len(), 1);
    tokio::join!(a.close(), b.close());
}

#[tokio::test(flavor = "multi_thread")]
async fn json_calls_and_method_lists_follow_the_allow_list() {
    let dir = tempfile::tempdir().unwrap();
    let a = node(MeshConfig::local_only()).await;
    let b = node(MeshConfig::local_only()).await;
    let tts = Arc::new(FakeTts::default());
    b.serve(providers(dir.path(), tts.clone()));
    trust(&a, "b", &b, &[]);
    trust(&b, "a", &a, &["tts"]);
    a.wait_connected("b", WAIT).await.unwrap();

    // b tells a what a may call: tts only.
    let methods = a.peer_methods("b").await.unwrap();
    assert!(methods.contains(&("tts".into(), "speak".into())));
    assert!(methods.iter().all(|(cap, _)| cap == "tts"), "{methods:?}");

    let out = a
        .call_json("b", "tts", "speak", &caller(), br#"["hi", null]"#)
        .await
        .unwrap();
    assert_eq!(out, b"null");
    assert_eq!(tts.said.lock().unwrap()[0].1, "hi");
    let out = a
        .call_json("b", "tts", "status", &caller(), b"[]")
        .await
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(status["queued"], 1);

    assert_eq!(
        a.call_json("b", "clipboard", "get", &caller(), b"[]").await,
        Err(CallError::Denied)
    );
    assert_eq!(
        a.call_json("nobody", "tts", "stop", &caller(), b"[]").await,
        Err(CallError::Unavailable)
    );
}
