//! `tts`: text to speech, through one FIFO queue per node. `speak` splits
//! the text into sentence chunks and queues them; a worker speaks one chunk
//! at a time, so nothing talks over itself. `stop` drops the queue and cuts
//! off the chunk being spoken. This is the `:8790` queue of the Hammerspoon
//! setup (`apps/tts.lua`), moved into the node.
//!
//! Each chunk is spoken by one engine:
//!
//! - `Http`: POST the chunk to a pocket-tts server's `/speak` with
//!   `X-Format: path` and `X-Voice`; it answers with the path of a finished
//!   WAV, which the player (`afplay` on macOS; on Linux `pw-play`,
//!   `paplay` or `aplay`, whichever is installed first) plays.
//! - `Say`: macOS `say`, used when `node.toml` names no TTS server. On
//!   Linux `espeak-ng` or `espeak`, which take the same arguments.
//! - `SpdSay`: Speech Dispatcher's `spd-say`, on Linux without espeak.
//! - `Sapi`: the Windows speech API (SAPI 5) on a thread of the node's own
//!   (`windows/tts.rs`), used on Windows when `node.toml` names no server.
//!   The player there is `ffplay`, else PowerShell's `SoundPlayer`.
//!
//! `node.toml`:
//!
//! ```toml
//! [tts]
//! sidecar = "pocket-en"           # a [sidecars.<name>]; or url = "http://…"
//! voice = "alba"                  # default voice
//! player = ["/usr/bin/afplay"]    # the WAV path is appended
//! max_chunk = 220
//! [tts.profiles]                  # names a plugin may pass as the voice
//! alerts = "marius"
//! ```

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Notify, watch};

use crate::caller::Caller;
use crate::echo::EchoReference;
use crate::error::CallError;
use crate::http::client;

/// Chunks longer than this are split, as in `lib/tts_core.lua`.
pub const MAX_CHUNK: usize = 220;
/// Synthesis of one chunk, model load included.
const SYNTH_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TtsStatus {
    pub speaking: bool,
    pub queued: u32,
    /// The default voice, if one is set.
    pub voice: Option<String>,
}

#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Queues `text`; returns once it is queued, not once it is spoken.
    async fn speak(
        &self,
        caller: &Caller,
        text: String,
        voice: Option<String>,
    ) -> Result<(), CallError>;
    /// Stops the current speech and clears the queue.
    async fn stop(&self, caller: &Caller) -> Result<(), CallError>;
    async fn status(&self, _caller: &Caller) -> Result<TtsStatus, CallError> {
        Err(CallError::Unavailable)
    }
}

/// No text to speech on this node.
pub struct NoTts;

#[async_trait]
impl TtsProvider for NoTts {
    async fn speak(&self, _: &Caller, _: String, _: Option<String>) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn stop(&self, _: &Caller) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
}

/// Collapses whitespace runs to one space and trims the ends.
pub fn sanitize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Splits text into chunks of at most `max` characters: at sentence ends
/// (`.`, `!` or `?` followed by a space or the end) first, merging short
/// sentences, then at word boundaries for sentences that are still too
/// long. A single word longer than `max` stays whole.
pub fn split_sentences(text: &str, max: usize) -> Vec<String> {
    let s = sanitize(text);
    if s.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut sentences = Vec::new();
    let mut buf = String::new();
    for (i, &c) in chars.iter().enumerate() {
        buf.push(c);
        if matches!(c, '.' | '!' | '?') && chars.get(i + 1).is_none_or(|n| *n == ' ') {
            sentences.push(buf.trim().to_string());
            buf.clear();
        }
    }
    if !buf.trim().is_empty() {
        sentences.push(buf.trim().to_string());
    }
    let len = |s: &str| s.chars().count();
    let mut chunks = Vec::new();
    let mut cur = String::new();
    for sentence in sentences {
        if len(&sentence) > max {
            if !cur.is_empty() {
                chunks.push(std::mem::take(&mut cur));
            }
            wrap_words(&sentence, max, &mut chunks);
        } else if cur.is_empty() {
            cur = sentence;
        } else if len(&cur) + 1 + len(&sentence) <= max {
            cur.push(' ');
            cur.push_str(&sentence);
        } else {
            chunks.push(std::mem::replace(&mut cur, sentence));
        }
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

fn wrap_words(sentence: &str, max: usize, out: &mut Vec<String>) {
    let mut cur = String::new();
    for word in sentence.split(' ') {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > max {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
}

/// `[tts]` in `node.toml`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TtsConfig {
    /// A sidecar serving the pocket-tts HTTP contract.
    #[serde(default)]
    pub sidecar: Option<String>,
    /// Or the base URL of such a server.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, String>,
    /// The player command; the WAV path is appended.
    #[serde(default)]
    pub player: Option<Vec<String>>,
    #[serde(default)]
    pub max_chunk: Option<usize>,
}

/// How one chunk becomes sound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Engine {
    /// pocket-tts at `url`, then `player <wav path>`.
    Http { url: String, player: Vec<String> },
    /// `say [-v voice]`, the text on stdin.
    Say { program: String },
    /// `spd-say -w [-y voice] -- <text>`.
    SpdSay { program: String },
    /// SAPI's default voice, or the installed voice [`match_voice`] picks.
    Sapi,
}

#[derive(Clone, Debug)]
struct Item {
    text: String,
    voice: Option<String>,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Item>,
    speaking: bool,
    /// Bumped by `stop`; the worker drops a chunk whose generation passed.
    generation: u64,
}

struct Inner {
    engine: Engine,
    voice: Option<String>,
    profiles: BTreeMap<String, String>,
    max_chunk: usize,
    /// Where each played WAV is noted as the echo reference.
    echo: Option<Arc<EchoReference>>,
    state: Mutex<State>,
    wake: Notify,
    stopped: watch::Sender<u64>,
}

/// The node's speech queue.
pub struct QueueTts {
    inner: Arc<Inner>,
}

impl QueueTts {
    /// Starts the worker. Must be called inside a Tokio runtime.
    pub fn new(engine: Engine, config: &TtsConfig) -> Self {
        Self::with_echo(engine, config, None)
    }

    /// With the echo reference that `mic` subtracts: every WAV the player
    /// starts is noted there.
    pub fn with_echo(engine: Engine, config: &TtsConfig, echo: Option<Arc<EchoReference>>) -> Self {
        let inner = Arc::new(Inner {
            engine,
            voice: config.voice.clone(),
            profiles: config.profiles.clone(),
            max_chunk: config.max_chunk.unwrap_or(MAX_CHUNK).max(20),
            echo,
            state: Mutex::default(),
            wake: Notify::new(),
            stopped: watch::channel(0).0,
        });
        tokio::spawn(work(inner.clone()));
        Self { inner }
    }

    /// `None` or `""` is the default voice, a profile name maps to its
    /// voice, anything else is a voice name used as is.
    pub fn resolve(&self, voice: Option<&str>) -> Option<String> {
        match voice.filter(|v| !v.is_empty()) {
            None => self.inner.voice.clone(),
            Some(v) => Some(
                self.inner
                    .profiles
                    .get(v)
                    .cloned()
                    .unwrap_or_else(|| v.into()),
            ),
        }
    }
}

#[async_trait]
impl TtsProvider for QueueTts {
    async fn speak(
        &self,
        caller: &Caller,
        text: String,
        voice: Option<String>,
    ) -> Result<(), CallError> {
        let chunks = split_sentences(&text, self.inner.max_chunk);
        if chunks.is_empty() {
            return Ok(());
        }
        let voice = self.resolve(voice.as_deref());
        let queued = {
            let mut state = self.inner.state.lock().unwrap();
            for text in chunks {
                state.queue.push_back(Item {
                    text,
                    voice: voice.clone(),
                });
            }
            state.queue.len()
        };
        tracing::debug!(plugin = %caller.plugin, queued, "tts queued");
        self.inner.wake.notify_one();
        Ok(())
    }

    async fn stop(&self, caller: &Caller) -> Result<(), CallError> {
        let generation = {
            let mut state = self.inner.state.lock().unwrap();
            state.generation += 1;
            state.queue.clear();
            state.speaking = false;
            state.generation
        };
        let _ = self.inner.stopped.send(generation);
        tracing::debug!(plugin = %caller.plugin, "tts stopped");
        Ok(())
    }

    async fn status(&self, _: &Caller) -> Result<TtsStatus, CallError> {
        let state = self.inner.state.lock().unwrap();
        Ok(TtsStatus {
            speaking: state.speaking,
            queued: state.queue.len() as u32,
            voice: self.inner.voice.clone(),
        })
    }
}

/// Speaks queued chunks one at a time, for as long as the node runs.
async fn work(this: Arc<Inner>) {
    loop {
        let next = {
            let mut state = this.state.lock().unwrap();
            let item = state.queue.pop_front();
            state.speaking = item.is_some();
            item.map(|i| (i, state.generation))
        };
        let Some((item, generation)) = next else {
            this.wake.notified().await;
            continue;
        };
        let mut stopped = this.stopped.subscribe();
        // A stop between the pop and the subscribe already counts.
        let mut cut = *stopped.borrow_and_update() != generation;
        if !cut {
            let speak = speak_one(&this.engine, &item, this.echo.as_deref());
            tokio::pin!(speak);
            cut = loop {
                tokio::select! {
                    result = &mut speak => {
                        if let Err(e) = result {
                            tracing::warn!("tts: skipped a chunk: {e}");
                        }
                        break false;
                    }
                    changed = stopped.changed() => {
                        if changed.is_err() || *stopped.borrow() != generation {
                            break true;
                        }
                    }
                }
            };
        }
        if cut {
            tracing::debug!("tts: chunk cut off by stop");
        }
        let mut state = this.state.lock().unwrap();
        if state.generation == generation {
            state.speaking = false;
        }
    }
}

/// Synthesizes and plays one chunk. Dropping the future kills the player.
async fn speak_one(
    engine: &Engine,
    item: &Item,
    echo: Option<&EchoReference>,
) -> Result<(), String> {
    match engine {
        Engine::Http { url, player } => {
            let wav = synthesize(url, &item.text, item.voice.as_deref()).await?;
            let (program, args) = player.split_first().ok_or("no player configured")?;
            if let Some(echo) = echo {
                echo.played(Path::new(&wav));
            }
            let status = tokio::process::Command::new(program)
                .args(args)
                .arg(&wav)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status()
                .await
                .map_err(|e| format!("cannot run {program}: {e}"))?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("{program} {wav}: {status}"))
            }
        }
        Engine::Say { program } => {
            let mut cmd = tokio::process::Command::new(program);
            if let Some(voice) = &item.voice {
                cmd.args(["-v", voice]);
            }
            let mut child = cmd
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| format!("cannot run {program}: {e}"))?;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(item.text.as_bytes()).await;
            }
            let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
            if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "{program}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        }
        Engine::SpdSay { program } => {
            let mut cmd = tokio::process::Command::new(program);
            cmd.arg("-w");
            if let Some(voice) = &item.voice {
                cmd.args(["-y", voice]);
            }
            let cancel = CancelSpeech(Some(program.clone()));
            let out = cmd
                .arg("--")
                .arg(&item.text)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output()
                .await
                .map_err(|e| format!("cannot run {program}: {e}"))?;
            cancel.disarm();
            if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "{program}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        }
        // Dropping this future, as `stop` does, purges SAPI's speech.
        #[cfg(target_os = "windows")]
        Engine::Sapi => crate::windows::tts::speak(&item.text, item.voice.as_deref()).await,
        #[cfg(not(target_os = "windows"))]
        Engine::Sapi => Err("SAPI speaks only on Windows".into()),
    }
}

/// The index of the voice `wanted` names among installed voice `names`:
/// the same name in any case, else the first name that contains it, so
/// "zira" finds "Microsoft Zira Desktop".
pub fn match_voice(names: &[String], wanted: &str) -> Option<usize> {
    let wanted = wanted.trim().to_lowercase();
    if wanted.is_empty() {
        return None;
    }
    let names: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    names
        .iter()
        .position(|n| *n == wanted)
        .or_else(|| names.iter().position(|n| n.contains(&wanted)))
}

/// The player for a TTS server's WAV files on Windows: `ffplay` when there
/// is one, else Windows PowerShell's `System.Media.SoundPlayer`, which
/// every Windows has and which plays WAV only. PowerShell joins the
/// arguments after `-Command` into one script, so the script joins the
/// path's pieces back together.
pub fn windows_player(ffplay: Option<&Path>) -> Vec<String> {
    match ffplay {
        Some(ffplay) => vec![
            ffplay.to_string_lossy().into_owned(),
            "-nodisp".into(),
            "-autoexit".into(),
            "-loglevel".into(),
            "error".into(),
        ],
        None => vec![
            "powershell.exe".into(),
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-Command".into(),
            "& { (New-Object System.Media.SoundPlayer ($args -join ' ')).PlaySync() }".into(),
        ],
    }
}

/// Killing `spd-say` leaves Speech Dispatcher talking. Dropped before
/// [`CancelSpeech::disarm`], as when `stop` cuts a chunk off, this runs
/// `spd-say -C` to cancel the speech too.
struct CancelSpeech(Option<String>);

impl CancelSpeech {
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for CancelSpeech {
    fn drop(&mut self) {
        if let Some(program) = self.0.take() {
            std::thread::spawn(move || {
                let _ = std::process::Command::new(program)
                    .arg("-C")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            });
        }
    }
}

/// POSTs one chunk to a pocket-tts server and returns the WAV path.
pub async fn synthesize(url: &str, text: &str, voice: Option<&str>) -> Result<String, String> {
    let endpoint = format!("{}/speak", url.trim_end_matches('/'));
    let (text, voice) = (text.to_string(), voice.map(str::to_string));
    let answer = tokio::task::spawn_blocking(move || {
        let mut headers = vec![("X-Format", "path")];
        if let Some(v) = voice.as_deref() {
            headers.push(("X-Voice", v));
        }
        client::post(&endpoint, &headers, &text, SYNTH_TIMEOUT)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("synthesis: {e}"))?;
    let body = answer.body.trim();
    if answer.status != 200 || body.is_empty() || body.starts_with("__ERROR__") {
        return Err(format!("synthesis answered {}: {body}", answer.status));
    }
    Ok(body.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::http::tests::{Seen, fake_server};

    #[test]
    fn sanitize_collapses_whitespace() {
        assert_eq!(sanitize("  a \n\t b  "), "a b");
        assert_eq!(sanitize(" \n "), "");
    }

    #[test]
    fn splits_at_sentence_ends_and_merges_short_ones() {
        assert_eq!(
            split_sentences("Hi there. How are you? Fine!", 220),
            ["Hi there. How are you? Fine!"]
        );
        assert_eq!(
            split_sentences("One two. Three four. Five.", 10),
            ["One two.", "Three", "four.", "Five."]
        );
        // Not a sentence end: no space after the dot.
        assert_eq!(
            split_sentences("Version 1.2 is out", 220),
            ["Version 1.2 is out"]
        );
        assert!(split_sentences("   ", 220).is_empty());
    }

    #[test]
    fn long_sentences_wrap_at_words_and_long_words_stay_whole() {
        let chunks = split_sentences("aaa bbb ccc ddd", 7);
        assert_eq!(chunks, ["aaa bbb", "ccc ddd"]);
        let chunks = split_sentences("short incomprehensibilities end", 10);
        assert_eq!(chunks, ["short", "incomprehensibilities", "end"]);
        for c in split_sentences(&"word ".repeat(200), 50) {
            assert!(c.chars().count() <= 50);
        }
    }

    #[test]
    fn counts_characters_not_bytes() {
        let text = "é".repeat(10);
        assert_eq!(split_sentences(&text, 10), std::slice::from_ref(&text));
    }

    /// A pocket-tts stand-in that answers `/speak` with a fake WAV path.
    fn pocket() -> (String, Arc<Mutex<Vec<Seen>>>) {
        let (port, seen) = fake_server(|r| {
            if r.body.contains("FAIL") {
                (500, "__ERROR__ boom".into())
            } else {
                (200, format!("/tmp/{}.wav\n", r.body.len()))
            }
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    /// A player that appends its argument to `log`, after `pause` seconds.
    fn player(log: &std::path::Path, pause: &str) -> Vec<String> {
        vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("sleep {pause}; echo \"$0\" >> {}", log.display()),
        ]
    }

    async fn until(what: &str, mut check: impl FnMut() -> bool) {
        let start = Instant::now();
        while !check() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn played(log: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    #[tokio::test]
    async fn chunks_are_synthesized_and_played_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("played");
        let (url, seen) = pocket();
        let config = TtsConfig {
            voice: Some("alba".into()),
            profiles: BTreeMap::from([("alerts".into(), "marius".into())]),
            max_chunk: Some(25),
            ..TtsConfig::default()
        };
        let tts = QueueTts::new(
            Engine::Http {
                url,
                player: player(&log, "0"),
            },
            &config,
        );
        let c = Caller::new("p");
        tts.speak(&c, "First one. Second sentence here.".into(), None)
            .await
            .unwrap();
        tts.speak(&c, "Third.".into(), Some("alerts".into()))
            .await
            .unwrap();
        until("three chunks", || played(&log).len() == 3).await;
        let seen = seen.lock().unwrap().clone();
        let texts: Vec<&str> = seen.iter().map(|r| r.body.as_str()).collect();
        assert_eq!(texts, ["First one.", "Second sentence here.", "Third."]);
        let header = |r: &Seen, k: &str| {
            r.headers
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(header(&seen[0], "x-voice").as_deref(), Some("alba"));
        assert_eq!(header(&seen[2], "x-voice").as_deref(), Some("marius"));
        assert_eq!(header(&seen[0], "x-format").as_deref(), Some("path"));
        assert!(
            seen.iter()
                .all(|r| r.path == "/speak" && r.method == "POST")
        );
        assert_eq!(played(&log), ["/tmp/10.wav", "/tmp/21.wav", "/tmp/6.wav"]);
        until("idle", || !tts.inner.state.lock().unwrap().speaking).await;
    }

    #[tokio::test]
    async fn a_failed_chunk_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("played");
        let (url, _) = pocket();
        let tts = QueueTts::new(
            Engine::Http {
                url,
                player: player(&log, "0"),
            },
            &TtsConfig::default(),
        );
        let c = Caller::new("p");
        tts.speak(&c, "FAIL here.".into(), None).await.unwrap();
        tts.speak(&c, "Fine.".into(), None).await.unwrap();
        until("the second chunk", || played(&log).len() == 1).await;
        assert_eq!(played(&log), ["/tmp/5.wav"]);
    }

    #[tokio::test]
    async fn stop_clears_the_queue_and_cuts_the_player() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("played");
        let (url, seen) = pocket();
        let tts = QueueTts::new(
            Engine::Http {
                url,
                player: player(&log, "5"),
            },
            &TtsConfig {
                max_chunk: Some(20),
                ..TtsConfig::default()
            },
        );
        let c = Caller::new("p");
        tts.speak(&c, "One is here. Two is here. Three is here.".into(), None)
            .await
            .unwrap();
        until("the first chunk to play", || {
            !seen.lock().unwrap().is_empty() && tts.inner.state.lock().unwrap().speaking
        })
        .await;
        assert_eq!(tts.status(&c).await.unwrap().queued, 2);
        tts.stop(&c).await.unwrap();
        let status = tts.status(&c).await.unwrap();
        assert_eq!((status.speaking, status.queued), (false, 0));
        // The player was killed before it wrote, and nothing else ran.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(played(&log).is_empty());
        assert_eq!(seen.lock().unwrap().len(), 1);
        // The queue still works after a stop.
        tts.speak(&c, "Again.".into(), None).await.unwrap();
        until("a new request", || seen.lock().unwrap().len() == 2).await;
        tts.stop(&c).await.unwrap();
    }

    /// An executable shell script at `dir/name`.
    fn script(dir: &std::path::Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn say_engines_get_the_voice_flag_and_the_text_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("said");
        // Stands in for `say` and `espeak-ng`.
        let program = script(
            dir.path(),
            "espeak-ng",
            &format!("echo \"$* | $(cat)\" >> {}", log.display()),
        );
        let tts = QueueTts::new(Engine::Say { program }, &TtsConfig::default());
        let c = Caller::new("p");
        tts.speak(&c, "Hello there.".into(), Some("en-us".into()))
            .await
            .unwrap();
        tts.speak(&c, "Bye.".into(), None).await.unwrap();
        until("two chunks", || played(&log).len() == 2).await;
        assert_eq!(played(&log), ["-v en-us | Hello there.", " | Bye."]);
    }

    #[tokio::test]
    async fn spd_say_gets_the_text_as_an_argument_and_stop_cancels_it() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("said");
        let program = script(
            dir.path(),
            "spd-say",
            &format!(
                "echo \"$*\" >> {log}; [ \"$1\" = -C ] || sleep 5",
                log = log.display()
            ),
        );
        let tts = QueueTts::new(Engine::SpdSay { program }, &TtsConfig::default());
        let c = Caller::new("p");
        tts.speak(&c, "-dash first.".into(), Some("Alba".into()))
            .await
            .unwrap();
        until("the chunk to start", || played(&log).len() == 1).await;
        assert_eq!(played(&log), ["-w -y Alba -- -dash first."]);
        tts.stop(&c).await.unwrap();
        until("the cancel", || played(&log).len() == 2).await;
        assert_eq!(played(&log)[1], "-C");
    }

    #[test]
    fn sapi_voices_match_by_name_then_by_part() {
        let names: Vec<String> = [
            "Microsoft David Desktop",
            "Microsoft Zira Desktop",
            "Microsoft Zira",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(match_voice(&names, "microsoft zira"), Some(2));
        assert_eq!(match_voice(&names, "Zira"), Some(1));
        assert_eq!(match_voice(&names, " david "), Some(0));
        assert_eq!(match_voice(&names, "Hazel"), None);
        assert_eq!(match_voice(&names, ""), None);
    }

    #[test]
    fn windows_player_prefers_ffplay() {
        assert_eq!(
            windows_player(Some(Path::new("C:/ffmpeg/bin/ffplay.exe"))),
            [
                "C:/ffmpeg/bin/ffplay.exe",
                "-nodisp",
                "-autoexit",
                "-loglevel",
                "error"
            ]
        );
        let fallback = windows_player(None);
        assert_eq!(fallback[0], "powershell.exe");
        assert!(
            fallback
                .last()
                .unwrap()
                .contains("SoundPlayer ($args -join ' ')")
        );
    }

    #[test]
    fn profiles_resolve_to_voices() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let tts = QueueTts::new(
            Engine::Say {
                program: "/usr/bin/true".into(),
            },
            &TtsConfig {
                voice: Some("alba".into()),
                profiles: BTreeMap::from([("code".into(), "cosette".into())]),
                ..TtsConfig::default()
            },
        );
        assert_eq!(tts.resolve(None).as_deref(), Some("alba"));
        assert_eq!(tts.resolve(Some("")).as_deref(), Some("alba"));
        assert_eq!(tts.resolve(Some("code")).as_deref(), Some("cosette"));
        assert_eq!(tts.resolve(Some("jean")).as_deref(), Some("jean"));
    }

    /// Against a real pocket-tts server: one chunk becomes a WAV file.
    /// Ignored: needs the server. Run it with
    /// `BRICOLO_TEST_POCKET_URL=http://127.0.0.1:8791 cargo nextest run --run-ignored all real_pocket`.
    #[ignore]
    #[tokio::test]
    async fn real_pocket_tts_writes_a_wav() {
        let url = std::env::var("BRICOLO_TEST_POCKET_URL").expect("set BRICOLO_TEST_POCKET_URL");
        let path = synthesize(&url, "Hello from the node.", None)
            .await
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"RIFF", "{path}");
    }
}
