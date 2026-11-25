//! `speech`: speech to text through a server with the HTTP contract of
//! `parakeet_server.py`: `POST /transcribe` with the path of an audio file
//! as the body answers 200 and the text, or an error status with a body
//! starting `__ERROR__`. The server reads the file itself, so it must run on
//! this machine.
//!
//! `node.toml`:
//!
//! ```toml
//! [speech]
//! sidecar = "parakeet"     # a [sidecars.<name>]; or url = "http://127.0.0.1:8765"
//! ```

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::caller::Caller;
use crate::disk::{authorize, home};
use crate::error::CallError;
use crate::http::client;

/// The server's own limit is 60 s per batch job.
const TIMEOUT: Duration = Duration::from_secs(90);

#[async_trait]
pub trait SpeechProvider: Send + Sync {
    /// Transcribes `audio`, encoded as `format` ("wav", "m4a"…).
    async fn transcribe(
        &self,
        caller: &Caller,
        audio: Vec<u8>,
        format: String,
    ) -> Result<String, CallError>;
    /// Transcribes a file the caller may read under its `disk` grants.
    async fn transcribe_file(&self, _caller: &Caller, _path: String) -> Result<String, CallError> {
        Err(CallError::Unavailable)
    }
}

/// No speech to text on this node.
pub struct NoSpeech;

#[async_trait]
impl SpeechProvider for NoSpeech {
    async fn transcribe(&self, _: &Caller, _: Vec<u8>, _: String) -> Result<String, CallError> {
        Err(CallError::Unavailable)
    }
}

/// `[speech]` in `node.toml`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SpeechConfig {
    #[serde(default)]
    pub sidecar: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// A parakeet-style server at `url`. Audio passed as bytes is written to
/// `scratch` first, then deleted.
pub struct HttpSpeech {
    pub url: String,
    pub scratch: PathBuf,
    next: AtomicU64,
}

impl HttpSpeech {
    pub fn new(url: impl Into<String>, scratch: PathBuf) -> Self {
        Self {
            url: url.into(),
            scratch,
            next: AtomicU64::new(1),
        }
    }

    async fn post(&self, path: String) -> Result<String, CallError> {
        let endpoint = format!("{}/transcribe", self.url.trim_end_matches('/'));
        let answer =
            tokio::task::spawn_blocking(move || client::post(&endpoint, &[], &path, TIMEOUT))
                .await
                .map_err(CallError::failed)??;
        let body = answer.body.trim();
        match body.strip_prefix("__ERROR__") {
            Some(why) => Err(CallError::Failed(why.trim().to_string())),
            None if answer.status == 200 => Ok(body.to_string()),
            None => Err(CallError::Failed(format!(
                "speech server answered {}: {body}",
                answer.status
            ))),
        }
    }
}

/// A file extension: 1 to 8 ASCII letters or digits.
fn valid_format(format: &str) -> bool {
    (1..=8).contains(&format.len()) && format.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[async_trait]
impl SpeechProvider for HttpSpeech {
    async fn transcribe(
        &self,
        caller: &Caller,
        audio: Vec<u8>,
        format: String,
    ) -> Result<String, CallError> {
        let format = format.to_ascii_lowercase();
        if !valid_format(&format) {
            return Err(CallError::Failed(format!("bad audio format {format:?}")));
        }
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        // A peer's caller is "<peer>/<plugin>": keep the name one path part.
        let who: String = caller
            .plugin
            .chars()
            .map(|c| match c {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
                _ => '_',
            })
            .collect();
        let file = self
            .scratch
            .join(format!("speech-{who}-{}-{n}.{format}", std::process::id()));
        tokio::fs::create_dir_all(&self.scratch)
            .await
            .map_err(CallError::failed)?;
        tokio::fs::write(&file, audio)
            .await
            .map_err(CallError::failed)?;
        let result = self.post(file.to_string_lossy().into_owned()).await;
        let _ = tokio::fs::remove_file(&file).await;
        result
    }

    async fn transcribe_file(&self, caller: &Caller, path: String) -> Result<String, CallError> {
        let real = authorize(&path, &caller.disk, &home(), false)?;
        self.post(real.to_string_lossy().into_owned()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::DiskGrant;
    use crate::http::tests::fake_server;

    /// A parakeet stand-in: reads the file named in the body and answers
    /// with its contents, or `__ERROR__` for an empty file.
    fn parakeet() -> String {
        let (port, _) = fake_server(|r| match std::fs::read_to_string(&r.body) {
            Ok(text) if r.path == "/transcribe" && !text.is_empty() => (200, format!("{text}\n")),
            Ok(_) => (500, "__ERROR__ empty audio".into()),
            Err(e) => (500, format!("__ERROR__ {e}")),
        });
        format!("http://127.0.0.1:{port}")
    }

    #[tokio::test]
    async fn bytes_go_through_a_scratch_file_that_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let speech = HttpSpeech::new(parakeet(), dir.path().join("scratch"));
        let c = Caller::new("p");
        let text = speech
            .transcribe(&c, b"hello world".to_vec(), "wav".into())
            .await
            .unwrap();
        assert_eq!(text, "hello world");
        let left = std::fs::read_dir(dir.path().join("scratch"))
            .unwrap()
            .count();
        assert_eq!(left, 0);
        let err = speech.transcribe(&c, Vec::new(), "wav".into()).await;
        assert_eq!(err, Err(CallError::Failed("empty audio".into())));
        let err = speech.transcribe(&c, b"x".to_vec(), "../x".into()).await;
        assert!(matches!(err, Err(CallError::Failed(_))));
    }

    /// A peer's call names its caller "<peer>/<plugin>"; the slash must not
    /// turn into a folder under the scratch directory.
    #[tokio::test]
    async fn a_peer_caller_name_stays_one_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let speech = HttpSpeech::new(parakeet(), dir.path().join("scratch"));
        let text = speech
            .transcribe(
                &Caller::new("linux/bricolo-call"),
                b"from a peer".to_vec(),
                "wav".into(),
            )
            .await
            .unwrap();
        assert_eq!(text, "from a peer");
        let left = std::fs::read_dir(dir.path().join("scratch"))
            .unwrap()
            .count();
        assert_eq!(left, 0);
    }

    #[tokio::test]
    async fn files_need_a_disk_grant() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.wav"), "from a file").unwrap();
        let speech = HttpSpeech::new(parakeet(), root.join("scratch"));
        let file = root.join("a.wav").to_string_lossy().into_owned();
        let denied = speech
            .transcribe_file(&Caller::new("p"), file.clone())
            .await;
        assert_eq!(denied, Err(CallError::Denied));
        let c = Caller {
            disk: vec![DiskGrant {
                root: root.clone(),
                write: false,
            }],
            ..Caller::new("p")
        };
        assert_eq!(
            speech.transcribe_file(&c, file).await.unwrap(),
            "from a file"
        );
    }

    #[tokio::test]
    async fn a_server_that_is_down_is_unavailable() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let dir = tempfile::tempdir().unwrap();
        let speech = HttpSpeech::new(format!("http://127.0.0.1:{port}"), dir.path().into());
        let err = speech
            .transcribe(&Caller::new("p"), b"x".to_vec(), "wav".into())
            .await;
        assert_eq!(err, Err(CallError::Unavailable));
    }

    /// Against a real parakeet server, with audio made by macOS `say`.
    /// Ignored: needs the server. Run it with
    /// `BRICOLO_TEST_PARAKEET_URL=http://127.0.0.1:8765 cargo nextest run --run-ignored all real_parakeet`.
    #[cfg(target_os = "macos")]
    #[ignore]
    #[tokio::test]
    async fn real_parakeet_transcribes_say_output() {
        let url =
            std::env::var("BRICOLO_TEST_PARAKEET_URL").expect("set BRICOLO_TEST_PARAKEET_URL");
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("in.wav");
        let ok = std::process::Command::new("/usr/bin/say")
            .args(["-o", &wav.to_string_lossy(), "--data-format=LEI16@16000"])
            .arg("testing one two three")
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let speech = HttpSpeech::new(url, dir.path().join("scratch"));
        let text = speech
            .transcribe(
                &Caller::new("p"),
                std::fs::read(&wav).unwrap(),
                "wav".into(),
            )
            .await
            .unwrap();
        assert!(text.to_lowercase().contains("testing"), "{text}");
    }
}
