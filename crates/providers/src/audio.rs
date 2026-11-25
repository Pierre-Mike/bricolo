//! `audio`: plays sound files on this device's default output. A
//! [`Player`] keeps each caller's sounds and hands the playing itself to a
//! [`Backend`]: AVAudioPlayer on macOS (`macos/audio.rs`), and elsewhere an
//! ffmpeg child process per sound, through PulseAudio (which PipeWire also
//! serves), then ALSA.
//!
//! A caller may play [`MAX_SOUNDS`] sounds at once and reaches only its
//! own. Paths pass the same `disk:` check as `speech.transcribe-file`;
//! a plugin's own data folder is one of its grants. The node stops a
//! plugin's sounds when it stops, restarts or reloads.
//!
//! `audio` never runs on a peer: it plays on this device's speakers.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::caller::Caller;
use crate::disk::{authorize, home};
use crate::error::CallError;

/// How many sounds one caller may play at once.
pub const MAX_SOUNDS: usize = 16;
/// An ffmpeg output that stays up this long counts as working. One that
/// exits with an error before then is missing, and the next is tried.
const STARTUP: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayOptions {
    /// 0 to 100.
    pub volume: u8,
    pub looping: bool,
}

impl Default for PlayOptions {
    fn default() -> Self {
        Self {
            volume: 100,
            looping: false,
        }
    }
}

/// A sound that is playing, as `sounds` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sound {
    pub id: u64,
    pub path: String,
    pub volume: u8,
    pub looping: bool,
}

#[async_trait]
pub trait AudioProvider: Send + Sync {
    /// Starts playing `path` for the caller and returns the sound's id.
    async fn play(
        &self,
        caller: &Caller,
        path: String,
        options: PlayOptions,
    ) -> Result<u64, CallError>;
    /// Changes the volume of the caller's sound `id`.
    async fn set_volume(&self, caller: &Caller, id: u64, volume: u8) -> Result<(), CallError>;
    /// Stops the caller's sound `id`, if it still plays.
    async fn stop(&self, caller: &Caller, id: u64) -> Result<(), CallError>;
    /// The caller's sounds that still play.
    async fn sounds(&self, caller: &Caller) -> Result<Vec<Sound>, CallError>;
    /// Stops `plugin`'s sounds.
    fn release(&self, _plugin: &str) {}
    /// Stops every sound. Called when the node shuts down.
    async fn shutdown(&self) {}
}

/// No sound output on this node.
pub struct NoAudio;

#[async_trait]
impl AudioProvider for NoAudio {
    async fn play(&self, _: &Caller, _: String, _: PlayOptions) -> Result<u64, CallError> {
        Err(CallError::Unavailable)
    }

    async fn set_volume(&self, _: &Caller, _: u64, _: u8) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn stop(&self, _: &Caller, _: u64) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn sounds(&self, _: &Caller) -> Result<Vec<Sound>, CallError> {
        Err(CallError::Unavailable)
    }
}

/// One sound on the output. Dropping it stops it.
pub trait Voice: Send {
    fn set_volume(&mut self, volume: u8) -> Result<(), CallError>;
    /// False once a sound that does not loop has ended.
    fn playing(&mut self) -> bool;
}

/// Starts voices. Called on Tokio's blocking pool.
pub trait Backend: Send + Sync {
    fn start(&self, path: &Path, options: PlayOptions) -> Result<Box<dyn Voice>, CallError>;
}

struct Playing {
    plugin: String,
    sound: Sound,
    voice: Box<dyn Voice>,
}

/// The sounds of every caller, played by one backend.
pub struct Player {
    backend: Arc<dyn Backend>,
    next: AtomicU64,
    sounds: Mutex<HashMap<u64, Playing>>,
}

impl Player {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            next: AtomicU64::new(1),
            sounds: Mutex::default(),
        }
    }

    /// Drops the sounds that have ended.
    fn prune(sounds: &mut HashMap<u64, Playing>) {
        sounds.retain(|_, p| p.voice.playing());
    }
}

#[async_trait]
impl AudioProvider for Player {
    async fn play(
        &self,
        caller: &Caller,
        path: String,
        options: PlayOptions,
    ) -> Result<u64, CallError> {
        let real = authorize(&path, &caller.disk, &home(), false)?;
        if !real.is_file() {
            return Err(CallError::Failed(format!("no file {path}")));
        }
        let options = PlayOptions {
            volume: options.volume.min(100),
            ..options
        };
        {
            let mut sounds = self.sounds.lock().unwrap();
            Self::prune(&mut sounds);
            let mine = sounds
                .values()
                .filter(|p| p.plugin == caller.plugin)
                .count();
            if mine >= MAX_SOUNDS {
                return Err(CallError::Failed(format!(
                    "already playing {MAX_SOUNDS} sounds"
                )));
            }
        }
        let backend = self.backend.clone();
        let file = real.clone();
        let voice = tokio::task::spawn_blocking(move || backend.start(&file, options))
            .await
            .map_err(CallError::failed)??;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.sounds.lock().unwrap().insert(
            id,
            Playing {
                plugin: caller.plugin.clone(),
                sound: Sound {
                    id,
                    path: real.to_string_lossy().into_owned(),
                    volume: options.volume,
                    looping: options.looping,
                },
                voice,
            },
        );
        tracing::info!(plugin = %caller.plugin, "audio: sound {id} playing {path}");
        Ok(id)
    }

    async fn set_volume(&self, caller: &Caller, id: u64, volume: u8) -> Result<(), CallError> {
        let volume = volume.min(100);
        let mut sounds = self.sounds.lock().unwrap();
        Self::prune(&mut sounds);
        match sounds.get_mut(&id) {
            Some(p) if p.plugin == caller.plugin => {
                p.voice.set_volume(volume)?;
                p.sound.volume = volume;
                Ok(())
            }
            _ => Err(CallError::Failed(format!("no sound {id} playing"))),
        }
    }

    async fn stop(&self, caller: &Caller, id: u64) -> Result<(), CallError> {
        let mut sounds = self.sounds.lock().unwrap();
        if sounds.get(&id).is_some_and(|p| p.plugin == caller.plugin) {
            sounds.remove(&id);
            tracing::info!(plugin = %caller.plugin, "audio: sound {id} stopped");
        }
        Ok(())
    }

    async fn sounds(&self, caller: &Caller) -> Result<Vec<Sound>, CallError> {
        let mut sounds = self.sounds.lock().unwrap();
        Self::prune(&mut sounds);
        let mut mine: Vec<Sound> = sounds
            .values()
            .filter(|p| p.plugin == caller.plugin)
            .map(|p| p.sound.clone())
            .collect();
        mine.sort_by_key(|s| s.id);
        Ok(mine)
    }

    fn release(&self, plugin: &str) {
        let mut sounds = self.sounds.lock().unwrap();
        let before = sounds.len();
        sounds.retain(|_, p| p.plugin != plugin);
        if sounds.len() < before {
            tracing::info!(plugin, "audio: {} sounds stopped", before - sounds.len());
        }
    }

    async fn shutdown(&self) {
        self.sounds.lock().unwrap().clear();
    }
}

/// The ffmpeg output arguments to try, in order, for this system.
pub fn default_outputs() -> Vec<Vec<String>> {
    let output = |format: &str, name: &str| vec!["-f".into(), format.into(), name.into()];
    if cfg!(target_os = "macos") {
        vec![output("audiotoolbox", "-")]
    } else if cfg!(target_os = "linux") {
        // The PulseAudio "file name" is the stream's name in the mixer.
        vec![output("pulse", "bricolo"), output("alsa", "default")]
    } else {
        // ffmpeg has no audio output device on Windows (no WASAPI or
        // DirectSound muxer; its SDL output shows video only), so `audio`
        // answers `unavailable` there.
        Vec::new()
    }
}

/// The ffmpeg arguments that play `path` on `output`.
pub fn ffmpeg_args(path: &Path, options: PlayOptions, output: &[String]) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error"]
        .map(String::from)
        .to_vec();
    if options.looping {
        args.extend(["-stream_loop".into(), "-1".into()]);
    }
    args.extend([
        "-i".into(),
        path.to_string_lossy().into_owned(),
        "-af".into(),
        format!("volume={:.2}", f32::from(options.volume.min(100)) / 100.0),
    ]);
    args.extend(output.iter().cloned());
    args
}

/// Plays each sound with its own ffmpeg child. Changing the volume
/// restarts the sound with the new one.
pub struct FfmpegBackend {
    ffmpeg: Option<PathBuf>,
    outputs: Vec<Vec<String>>,
    startup: Duration,
}

impl FfmpegBackend {
    /// `ffmpeg` is the program; `None` finds it like `mic` does.
    pub fn new(ffmpeg: Option<PathBuf>, outputs: Vec<Vec<String>>) -> Self {
        Self {
            ffmpeg,
            outputs,
            startup: STARTUP,
        }
    }

    /// How long an output must stay up to count as working, instead of
    /// [`STARTUP`].
    pub fn with_startup(self, startup: Duration) -> Self {
        Self { startup, ..self }
    }

    fn program(&self) -> Option<PathBuf> {
        self.ffmpeg
            .clone()
            .or_else(|| crate::mic::find_ffmpeg(None))
    }
}

impl Backend for FfmpegBackend {
    fn start(&self, path: &Path, options: PlayOptions) -> Result<Box<dyn Voice>, CallError> {
        if self.outputs.is_empty() {
            return Err(CallError::Unavailable);
        }
        let Some(program) = self.program() else {
            tracing::warn!("audio: ffmpeg not found; install it");
            return Err(CallError::Unavailable);
        };
        let mut why = String::new();
        for output in &self.outputs {
            let mut voice = FfmpegVoice {
                program: program.clone(),
                path: path.to_path_buf(),
                options,
                output: output.clone(),
                child: None,
                stderr: Arc::default(),
            };
            voice.spawn()?;
            match voice.wait_started(self.startup) {
                Ok(()) => return Ok(Box::new(voice)),
                Err(e) => {
                    tracing::debug!("audio: {e}");
                    why = e;
                }
            }
        }
        Err(CallError::Failed(why))
    }
}

struct FfmpegVoice {
    program: PathBuf,
    path: PathBuf,
    options: PlayOptions,
    output: Vec<String>,
    child: Option<Child>,
    /// The end of ffmpeg's stderr.
    stderr: Arc<Mutex<String>>,
}

impl FfmpegVoice {
    fn spawn(&mut self) -> Result<(), CallError> {
        let args = ffmpeg_args(&self.path, self.options, &self.output);
        let mut child = Command::new(&self.program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                CallError::Failed(format!("cannot start {}: {e}", self.program.display()))
            })?;
        if let Some(mut stderr) = child.stderr.take() {
            let tail = self.stderr.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                while let Ok(n) = stderr.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut t = tail.lock().unwrap();
                    t.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if t.len() > 4096 {
                        let cut = t.len() - 2048;
                        let cut = (cut..t.len()).find(|i| t.is_char_boundary(*i)).unwrap_or(0);
                        t.drain(..cut);
                    }
                }
            });
        }
        self.child = Some(child);
        Ok(())
    }

    /// Waits `startup` for ffmpeg to fail on its output. Ending well
    /// counts as started: a short sound may be over by then.
    fn wait_started(&mut self, startup: Duration) -> Result<(), String> {
        let deadline = Instant::now() + startup;
        while Instant::now() < deadline {
            let Some(child) = self.child.as_mut() else {
                return Err("ffmpeg is not running".into());
            };
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => {
                    // Let the stderr reader catch up.
                    std::thread::sleep(Duration::from_millis(20));
                    let tail = self.stderr.lock().unwrap();
                    let last = tail.lines().rev().find(|l| !l.trim().is_empty());
                    return Err(format!(
                        "ffmpeg {} exited ({status}): {}",
                        self.output.join(" "),
                        last.unwrap_or("no output").trim()
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(())
    }

    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Voice for FfmpegVoice {
    fn set_volume(&mut self, volume: u8) -> Result<(), CallError> {
        if self.options.volume == volume {
            return Ok(());
        }
        self.kill();
        self.options.volume = volume;
        self.spawn()
    }

    fn playing(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }
}

impl Drop for FfmpegVoice {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::DiskGrant;

    /// Voices that play until stopped, or end at once for a path with
    /// "short" in it. Records every volume they were given.
    #[derive(Default)]
    struct FakeBackend {
        volumes: Arc<Mutex<Vec<(String, u8)>>>,
        live: Arc<Mutex<Vec<String>>>,
    }

    struct FakeVoice {
        name: String,
        ends: bool,
        volumes: Arc<Mutex<Vec<(String, u8)>>>,
        live: Arc<Mutex<Vec<String>>>,
    }

    impl Backend for FakeBackend {
        fn start(&self, path: &Path, o: PlayOptions) -> Result<Box<dyn Voice>, CallError> {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            self.volumes.lock().unwrap().push((name.clone(), o.volume));
            self.live.lock().unwrap().push(name.clone());
            Ok(Box::new(FakeVoice {
                ends: name.contains("short"),
                name,
                volumes: self.volumes.clone(),
                live: self.live.clone(),
            }))
        }
    }

    impl Voice for FakeVoice {
        fn set_volume(&mut self, volume: u8) -> Result<(), CallError> {
            self.volumes
                .lock()
                .unwrap()
                .push((self.name.clone(), volume));
            Ok(())
        }

        fn playing(&mut self) -> bool {
            !self.ends
        }
    }

    impl Drop for FakeVoice {
        fn drop(&mut self) {
            self.live.lock().unwrap().retain(|n| n != &self.name);
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        player: Player,
        volumes: Arc<Mutex<Vec<(String, u8)>>>,
        live: Arc<Mutex<Vec<String>>>,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        for name in ["noise.wav", "beep-short.wav", "other.wav"] {
            std::fs::write(dir.path().join(name), "RIFF").unwrap();
        }
        let backend = FakeBackend::default();
        let (volumes, live) = (backend.volumes.clone(), backend.live.clone());
        Fixture {
            dir,
            player: Player::new(Arc::new(backend)),
            volumes,
            live,
        }
    }

    impl Fixture {
        /// A caller whose only grant is the fixture's folder.
        fn caller(&self, name: &str) -> Caller {
            Caller {
                disk: vec![DiskGrant {
                    root: self.dir.path().to_path_buf(),
                    write: false,
                }],
                ..Caller::new(name)
            }
        }

        fn path(&self, name: &str) -> String {
            self.dir.path().join(name).to_string_lossy().into_owned()
        }

        fn live(&self) -> Vec<String> {
            self.live.lock().unwrap().clone()
        }
    }

    fn looping(volume: u8) -> PlayOptions {
        PlayOptions {
            volume,
            looping: true,
        }
    }

    #[tokio::test]
    async fn play_set_volume_and_stop_one_sound() {
        let f = fixture();
        let c = f.caller("noise");
        let id = f
            .player
            .play(&c, f.path("noise.wav"), looping(40))
            .await
            .unwrap();
        let sounds = f.player.sounds(&c).await.unwrap();
        assert_eq!(sounds.len(), 1);
        assert_eq!(
            (sounds[0].id, sounds[0].volume, sounds[0].looping),
            (id, 40, true)
        );
        assert!(sounds[0].path.ends_with("noise.wav"), "{}", sounds[0].path);

        f.player.set_volume(&c, id, 250).await.unwrap();
        assert_eq!(f.player.sounds(&c).await.unwrap()[0].volume, 100);
        assert_eq!(
            *f.volumes.lock().unwrap(),
            [
                ("noise.wav".to_string(), 40),
                ("noise.wav".to_string(), 100)
            ]
        );

        f.player.stop(&c, id).await.unwrap();
        assert!(f.live().is_empty());
        assert!(f.player.sounds(&c).await.unwrap().is_empty());
        // Stopping again does nothing; the volume of a stopped sound fails.
        f.player.stop(&c, id).await.unwrap();
        assert!(f.player.set_volume(&c, id, 10).await.is_err());
    }

    #[tokio::test]
    async fn callers_reach_only_their_own_sounds() {
        let f = fixture();
        let (a, b) = (f.caller("a"), f.caller("b"));
        let id = f
            .player
            .play(&a, f.path("noise.wav"), looping(50))
            .await
            .unwrap();
        f.player
            .play(&b, f.path("other.wav"), looping(50))
            .await
            .unwrap();
        assert!(f.player.set_volume(&b, id, 10).await.is_err());
        f.player.stop(&b, id).await.unwrap();
        assert_eq!(f.live().len(), 2, "b stopped a's sound");
        assert_eq!(f.player.sounds(&b).await.unwrap().len(), 1);

        f.player.release("a");
        assert_eq!(f.live(), ["other.wav"]);
        f.player.shutdown().await;
        assert!(f.live().is_empty());
    }

    #[tokio::test]
    async fn paths_outside_the_grants_are_denied() {
        let f = fixture();
        let c = f.caller("p");
        let r = f.player.play(&c, "/etc/hosts".into(), looping(10)).await;
        assert_eq!(r, Err(CallError::Denied));
        let r = f
            .player
            .play(&Caller::new("none"), f.path("noise.wav"), looping(10))
            .await;
        assert_eq!(r, Err(CallError::Denied));
        let r = f.player.play(&c, f.path("missing.wav"), looping(10)).await;
        assert!(
            matches!(&r, Err(CallError::Failed(m)) if m.contains("no file")),
            "{r:?}"
        );
        assert!(f.live().is_empty());
    }

    #[tokio::test]
    async fn ended_sounds_leave_the_list_and_the_limit() {
        let f = fixture();
        let c = f.caller("p");
        for _ in 0..MAX_SOUNDS + 2 {
            f.player
                .play(&c, f.path("beep-short.wav"), PlayOptions::default())
                .await
                .unwrap();
        }
        assert!(f.player.sounds(&c).await.unwrap().is_empty());
        for _ in 0..MAX_SOUNDS {
            f.player
                .play(&c, f.path("noise.wav"), looping(5))
                .await
                .unwrap();
        }
        let r = f.player.play(&c, f.path("noise.wav"), looping(5)).await;
        assert!(
            matches!(&r, Err(CallError::Failed(m)) if m.contains("already")),
            "{r:?}"
        );
    }

    #[test]
    fn ffmpeg_arguments_loop_and_scale_the_volume() {
        let out = ["-f".to_string(), "pulse".into(), "bricolo".into()];
        let args = ffmpeg_args(Path::new("/n.wav"), looping(40), &out).join(" ");
        assert!(
            args.ends_with("-stream_loop -1 -i /n.wav -af volume=0.40 -f pulse bricolo"),
            "{args}"
        );
        let args = ffmpeg_args(Path::new("/n.wav"), PlayOptions::default(), &out).join(" ");
        assert!(
            args.ends_with("-i /n.wav -af volume=1.00 -f pulse bricolo"),
            "{args}"
        );
        if cfg!(target_os = "linux") {
            assert_eq!(default_outputs()[0][1], "pulse");
            assert_eq!(default_outputs()[1][1], "alsa");
        }
    }

    /// A stand-in for ffmpeg: logs its arguments next to itself, fails for
    /// the output "missing", and plays until killed.
    #[cfg(unix)]
    const FAKE: &str = r#"#!/bin/sh
echo "$@" >> "$(dirname "$0")/args.log"
case " $* " in *" missing "*) echo "missing: No such device" >&2; exit 1 ;; esac
while true; do sleep 0.1; done
"#;

    #[cfg(unix)]
    fn fake_ffmpeg(dir: &Path, outputs: &[&str]) -> FfmpegBackend {
        use std::os::unix::fs::PermissionsExt;
        let fake = dir.join("ffmpeg");
        std::fs::write(&fake, FAKE).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let outputs = outputs
            .iter()
            .map(|o| vec!["-f".into(), "test".into(), o.to_string()])
            .collect();
        // A script written a moment ago can take seconds to start on a
        // loaded Mac, while macOS checks it.
        FfmpegBackend::new(Some(fake), outputs).with_startup(Duration::from_secs(5))
    }

    #[cfg(unix)]
    #[test]
    fn ffmpeg_falls_back_to_the_next_output_and_restarts_for_volume() {
        let dir = tempfile::tempdir().unwrap();
        let backend = fake_ffmpeg(dir.path(), &["missing", "speakers"]);
        let mut voice = backend.start(Path::new("/n.wav"), looping(40)).unwrap();
        assert!(voice.playing());
        voice.set_volume(70).unwrap();
        assert!(voice.playing());
        // The restarted fake may take a moment to write its line.
        let read = || std::fs::read_to_string(dir.path().join("args.log")).unwrap_or_default();
        let start = Instant::now();
        while read().lines().count() < 3 && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let log = read();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3, "{log}");
        assert!(lines[0].ends_with("-f test missing"), "{log}");
        assert!(lines[1].ends_with("volume=0.40 -f test speakers"), "{log}");
        assert!(lines[2].ends_with("volume=0.70 -f test speakers"), "{log}");
        drop(voice);

        let backend = fake_ffmpeg(dir.path(), &["missing"]);
        let err = backend.start(Path::new("/n.wav"), looping(40)).err();
        assert!(
            matches!(&err, Some(CallError::Failed(m)) if m.contains("No such device")),
            "{:?}",
            err.map(|e| e.to_string())
        );
        let none = FfmpegBackend::new(None, Vec::new()).start(Path::new("/n.wav"), looping(1));
        assert!(matches!(none, Err(CallError::Unavailable)));
    }
}
