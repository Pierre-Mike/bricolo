//! `mic`: records the microphone into the caller's data folder with an
//! ffmpeg child process. macOS reads `avfoundation`; Linux tries
//! PulseAudio (which PipeWire also serves), then ALSA. The file is a 16 kHz
//! mono 16-bit WAV in `<data>/<plugin>/recordings/`, which the plugin sees
//! at `/data/recordings/` and may pass to `speech.transcribe-file`.
//!
//! `node.toml`:
//!
//! ```toml
//! [mic]
//! device = "MacBook Pro Microphone"   # ffmpeg's input; default "default"
//! ffmpeg = "/opt/homebrew/bin/ffmpeg" # default: the first ffmpeg found
//! echo_cancel = true                  # default: remove the node's own speech
//! ```
//!
//! When `tts` spoke during a recording, `record-stop` subtracts that
//! speech from the file before it answers ([`crate::echo`]).
//!
//! A recording counts in [`Sensors`] as the caller's `mic` from the moment
//! ffmpeg starts until it stops, so `bricolo status`, the hub and the menu bar
//! show it.
//!
//! `mic` never runs on a peer. A remote microphone will use the mesh's
//! media streams instead of a file path.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

use crate::caller::Caller;
use crate::disk::{expand, home};
use crate::echo::EchoReference;
use crate::error::CallError;
use crate::sensors::{Sensor, Sensors};

/// The longest recording, and what `max_secs = 0` means.
pub const MAX_SECS: u32 = 600;
/// How many recordings each caller keeps; older ones are deleted when a
/// new one starts.
pub const KEEP: usize = 10;
/// `record-start` answers once ffmpeg writes audio, or after this long.
/// An input that makes ffmpeg exit before then counts as missing, and the
/// next one is tried.
const STARTUP: Duration = Duration::from_secs(3);
/// How long ffmpeg gets to finish the file after SIGINT (Ctrl+Break on
/// Windows).
const GRACE: Duration = Duration::from_secs(3);
/// How often a recording is checked for having stopped by itself.
const WATCH: Duration = Duration::from_millis(100);
/// Where ffmpeg is looked for after `PATH`. A node started by launchd has
/// no Homebrew folder on its `PATH`.
const FFMPEG_DIRS: [&str; 3] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFormat {
    /// WAV, 16 kHz, mono, signed 16-bit.
    Wav16kMono,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordOptions {
    pub max_secs: u32,
    pub format: AudioFormat,
}

impl Default for RecordOptions {
    fn default() -> Self {
        Self {
            max_secs: MAX_SECS,
            format: AudioFormat::Wav16kMono,
        }
    }
}

#[async_trait]
pub trait MicProvider: Send + Sync {
    /// Starts recording for the caller and returns the recording's id.
    async fn record_start(&self, caller: &Caller, options: RecordOptions)
    -> Result<u64, CallError>;
    /// Stops recording `id` and returns its file's path.
    async fn record_stop(&self, caller: &Caller, id: u64) -> Result<String, CallError>;
    /// Stops recording `id` and deletes its file.
    async fn record_cancel(&self, caller: &Caller, id: u64) -> Result<(), CallError>;
    /// Stops `plugin`'s recordings and deletes their files.
    fn release(&self, _plugin: &str) {}
    /// Stops every recording. Called when the node shuts down.
    async fn shutdown(&self) {}
}

/// No microphone on this node.
pub struct NoMic;

#[async_trait]
impl MicProvider for NoMic {
    async fn record_start(&self, _: &Caller, _: RecordOptions) -> Result<u64, CallError> {
        Err(CallError::Unavailable)
    }

    async fn record_stop(&self, _: &Caller, _: u64) -> Result<String, CallError> {
        Err(CallError::Unavailable)
    }

    async fn record_cancel(&self, _: &Caller, _: u64) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
}

/// `[mic]` in `node.toml`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MicConfig {
    /// The input device: an avfoundation audio device name or index on
    /// macOS, a PulseAudio source or ALSA device on Linux, a DirectShow
    /// device name on Windows (each one ffmpeg lists when unset).
    #[serde(default)]
    pub device: Option<String>,
    /// The ffmpeg program. `~/` expands.
    #[serde(default)]
    pub ffmpeg: Option<String>,
    /// Subtract the node's own speech (`tts`) from each recording, so the
    /// microphone does not pick up the agent's replies. Default on.
    #[serde(default)]
    pub echo_cancel: Option<bool>,
}

/// `configured` with `~/` expanded, else the first ffmpeg on `PATH` or in
/// [`FFMPEG_DIRS`]. `audio` finds its ffmpeg the same way.
/// ffmpeg's file name in a `PATH` folder.
const FFMPEG: &str = if cfg!(windows) {
    "ffmpeg.exe"
} else {
    "ffmpeg"
};

pub(crate) fn find_ffmpeg(configured: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = configured {
        return Some(expand(p, &home()).unwrap_or_else(|| PathBuf::from(p)));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(FFMPEG_DIRS.iter().map(PathBuf::from))
        .map(|dir| dir.join(FFMPEG))
        .find(|p| p.is_file())
}

/// The ffmpeg input arguments to try, in order, for this system.
pub fn default_inputs(device: Option<&str>) -> Vec<Vec<String>> {
    if cfg!(target_os = "windows") {
        // DirectShow has no "default" device: without one in node.toml,
        // try each microphone ffmpeg lists.
        return dshow_inputs("audio", device, || dshow_listed().audio);
    }
    let device = device.unwrap_or("default");
    let input = |format: &str, name: String| vec!["-f".into(), format.into(), "-i".into(), name];
    if cfg!(target_os = "macos") {
        // avfoundation reads "<video>:<audio>"; an empty video part means
        // audio only.
        let name = if device.starts_with(':') {
            device.to_string()
        } else {
            format!(":{device}")
        };
        vec![input("avfoundation", name)]
    } else if cfg!(target_os = "linux") {
        vec![
            input("pulse", device.to_string()),
            input("alsa", device.to_string()),
        ]
    } else {
        Vec::new()
    }
}

/// The DirectShow devices `ffmpeg -list_devices true -f dshow -i dummy`
/// prints, in its order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DshowDevices {
    pub audio: Vec<String>,
    pub video: Vec<String>,
}

/// Reads the device list ffmpeg prints on stderr. ffmpeg 4.4 and later
/// mark each device `"Name" (audio)` or `"Name" (video)`; older versions
/// list them under "DirectShow video devices" and "DirectShow audio
/// devices" headings. The "Alternative name" lines are skipped.
pub fn parse_dshow_devices(text: &str) -> DshowDevices {
    let mut devices = DshowDevices::default();
    let mut section = "";
    for line in text.lines() {
        // Drop the "[dshow @ 000001…]" tag.
        let line = match line.trim_start().strip_prefix('[') {
            Some(rest) => rest.split_once(']').map_or("", |(_, l)| l),
            None => line,
        }
        .trim();
        if line.contains("DirectShow video devices") {
            section = "video";
            continue;
        }
        if line.contains("DirectShow audio devices") {
            section = "audio";
            continue;
        }
        let Some(rest) = line.strip_prefix('"') else {
            continue;
        };
        let Some((name, kind)) = rest.split_once('"') else {
            continue;
        };
        let kind = match kind.trim() {
            "" => section,
            k if k.contains("audio") => "audio",
            k if k.contains("video") => "video",
            _ => continue,
        };
        match kind {
            "audio" => devices.audio.push(name.to_string()),
            "video" => devices.video.push(name.to_string()),
            _ => {}
        }
    }
    devices
}

/// The DirectShow inputs to try for `kind` ("audio" or "video"): the
/// configured device, with or without its `audio=` prefix, else each
/// device `listed` gives.
pub fn dshow_inputs(
    kind: &str,
    device: Option<&str>,
    listed: impl FnOnce() -> Vec<String>,
) -> Vec<Vec<String>> {
    let input = |name: &str| {
        let prefix = format!("{kind}=");
        let name = if name.starts_with(&prefix) {
            name.to_string()
        } else {
            format!("{prefix}{name}")
        };
        vec!["-f".into(), "dshow".into(), "-i".into(), name]
    };
    match device {
        Some(device) => vec![input(device)],
        None => listed().iter().map(|name| input(name)).collect(),
    }
}

#[cfg(target_os = "windows")]
pub(crate) use crate::windows::dshow::listed as dshow_listed;

/// No DirectShow off Windows.
#[cfg(not(target_os = "windows"))]
pub(crate) fn dshow_listed() -> DshowDevices {
    DshowDevices::default()
}

/// The ffmpeg arguments that record `input` into `path`.
pub fn ffmpeg_args(input: &[String], options: RecordOptions, path: &Path) -> Vec<String> {
    let secs = match options.max_secs {
        0 => MAX_SECS,
        n => n.min(MAX_SECS),
    };
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error", "-y"]
        .map(String::from)
        .to_vec();
    args.extend(input.iter().cloned());
    let AudioFormat::Wav16kMono = options.format;
    args.extend(
        [
            "-t",
            &secs.to_string(),
            "-ar",
            "16000",
            "-ac",
            "1",
            "-c:a",
            "pcm_s16le",
            // Write each packet at once, so the file shows when audio
            // starts arriving (see `started`).
            "-flush_packets",
            "1",
        ]
        .map(String::from),
    );
    args.push(path.to_string_lossy().into_owned());
    args
}

/// Recording files sort by name in the order they were made.
fn file_name(id: u64) -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("rec-{ms:015}-{id}.wav")
}

/// Deletes all but the newest `keep` recordings in `dir`.
pub fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("rec-") && n.ends_with(".wav"))
        .collect();
    names.sort();
    let extra = names.len().saturating_sub(keep);
    for name in &names[..extra] {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

struct Recording {
    plugin: String,
    child: Child,
    path: PathBuf,
    /// The end of ffmpeg's stderr.
    stderr: Arc<Mutex<String>>,
}

/// Records through ffmpeg into `<data_dir>/<plugin>/recordings/`.
pub struct FfmpegMic {
    data_dir: PathBuf,
    ffmpeg: Option<String>,
    inputs: Vec<Vec<String>>,
    next: AtomicU64,
    recordings: Arc<Mutex<HashMap<u64, Recording>>>,
    /// Callers whose `record-start` is still waiting for ffmpeg.
    starting: Mutex<HashSet<String>>,
    /// Where a recording shows as its caller's `mic`.
    sensors: Arc<Sensors>,
    /// The audio `tts` played, subtracted from each recording at stop.
    echo: Option<Arc<EchoReference>>,
}

impl FfmpegMic {
    pub fn new(data_dir: PathBuf, config: &MicConfig, sensors: Arc<Sensors>) -> Self {
        Self::with_inputs(
            data_dir,
            config.ffmpeg.clone(),
            default_inputs(config.device.as_deref()),
            sensors,
        )
    }

    /// With the ffmpeg input arguments to try, in order.
    pub fn with_inputs(
        data_dir: PathBuf,
        ffmpeg: Option<String>,
        inputs: Vec<Vec<String>>,
        sensors: Arc<Sensors>,
    ) -> Self {
        Self {
            data_dir,
            ffmpeg,
            inputs,
            next: AtomicU64::new(1),
            recordings: Arc::default(),
            starting: Mutex::default(),
            sensors,
            echo: None,
        }
    }

    /// Subtracts what `echo` holds from every recording at `record-stop`.
    pub fn with_echo(mut self, echo: Arc<EchoReference>) -> Self {
        self.echo = Some(echo);
        self
    }

    fn program(&self) -> Option<PathBuf> {
        find_ffmpeg(self.ffmpeg.as_deref())
    }

    /// Starts ffmpeg on the first input that works.
    async fn start(&self, caller: &Caller, options: RecordOptions) -> Result<u64, CallError> {
        if self.inputs.is_empty() {
            return Err(CallError::Unavailable);
        }
        let Some(program) = self.program() else {
            tracing::warn!("mic: ffmpeg not found; install it or set [mic] ffmpeg in node.toml");
            return Err(CallError::Unavailable);
        };
        let dir = self.data_dir.join(&caller.plugin).join("recordings");
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| CallError::Failed(format!("cannot create {}: {e}", dir.display())))?;
        prune(&dir, KEEP - 1);
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(file_name(id));
        let mut why = String::new();
        for input in &self.inputs {
            let args = ffmpeg_args(input, options, &path);
            let (mut child, stderr) = spawn(&program, &args)?;
            // An input that is missing or refused makes ffmpeg exit at once.
            if let Started::Exited(status) = started(&mut child, &path).await {
                // Let the stderr reader catch up.
                tokio::time::sleep(Duration::from_millis(20)).await;
                why = format!(
                    "ffmpeg {} exited ({status}): {}",
                    input.join(" "),
                    last_line(&stderr)
                );
                tracing::debug!(plugin = %caller.plugin, "mic: {why}");
                continue;
            }
            self.recordings.lock().unwrap().insert(
                id,
                Recording {
                    plugin: caller.plugin.clone(),
                    child,
                    path,
                    stderr,
                },
            );
            self.sensors.set(&caller.plugin, Sensor::Mic, true);
            tokio::spawn(watch(
                Arc::downgrade(&self.recordings),
                self.sensors.clone(),
                id,
            ));
            tracing::info!(plugin = %caller.plugin, "mic: recording {id} started");
            return Ok(id);
        }
        let _ = tokio::fs::remove_file(&path).await;
        Err(CallError::Failed(why))
    }

    /// Takes `id` out of the table if `caller` owns it.
    fn take(&self, caller: &Caller, id: u64) -> Result<Recording, CallError> {
        let mut recordings = self.recordings.lock().unwrap();
        match recordings.get(&id) {
            Some(r) if r.plugin == caller.plugin => {
                self.sensors.set(&caller.plugin, Sensor::Mic, false);
                Ok(recordings.remove(&id).expect("present"))
            }
            _ => Err(CallError::Failed(format!("no recording {id}"))),
        }
    }
}

/// Clears the `mic` sensor when recording `id` stops by itself, at its
/// `max_secs` or because ffmpeg died. Its file waits for `record-stop`.
/// Ends when the recording leaves the table: stop, cancel and release
/// clear the sensor themselves.
async fn watch(recordings: Weak<Mutex<HashMap<u64, Recording>>>, sensors: Arc<Sensors>, id: u64) {
    loop {
        tokio::time::sleep(WATCH).await;
        let Some(recordings) = recordings.upgrade() else {
            return;
        };
        let mut recordings = recordings.lock().unwrap();
        let Some(r) = recordings.get_mut(&id) else {
            return;
        };
        if !matches!(r.child.try_wait(), Ok(None)) {
            sensors.set(&r.plugin, Sensor::Mic, false);
            return;
        }
    }
}

/// Starts ffmpeg and keeps the end of its stderr.
fn spawn(program: &Path, args: &[String]) -> Result<(Child, Arc<Mutex<String>>), CallError> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Its own console process group, so Ctrl+Break reaches only ffmpeg.
    #[cfg(windows)]
    cmd.creation_flags(crate::windows::process::NEW_PROCESS_GROUP);
    let mut child = cmd
        .spawn()
        .map_err(|e| CallError::Failed(format!("cannot start {}: {e}", program.display())))?;
    let tail = Arc::new(Mutex::new(String::new()));
    if let Some(mut stderr) = child.stderr.take() {
        let tail = tail.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            while let Ok(n) = stderr.read(&mut buf).await {
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
    Ok((child, tail))
}

fn last_line(tail: &Mutex<String>) -> String {
    let t = tail.lock().unwrap();
    t.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no output")
        .trim()
        .to_string()
}

enum Started {
    /// Audio is reaching the file, or ffmpeg is still opening the input.
    Running,
    /// ffmpeg exited, with this status.
    Exited(String),
}

/// The bytes of samples in the WAV file at `path`: what follows the
/// header of its `data` chunk. ffmpeg's header is longer than the
/// classic 44 bytes, since it adds a `LIST` chunk, and it fills in the
/// chunk sizes only when it finishes, so the file length is what counts.
pub fn audio_bytes(path: &Path) -> u64 {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return 0;
    };
    let mut head = [0u8; 512];
    let Ok(n) = file.read(&mut head) else {
        return 0;
    };
    let len = file.metadata().map_or(0, |m| m.len());
    match head[..n].windows(4).position(|w| w == b"data") {
        Some(at) => len.saturating_sub(at as u64 + 8),
        None => 0,
    }
}

/// Waits for the first audio in `path`, or for ffmpeg to exit.
async fn started(child: &mut Child, path: &Path) -> Started {
    let deadline = tokio::time::Instant::now() + STARTUP;
    while tokio::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => return Started::Exited(status.to_string()),
            Err(e) => return Started::Exited(e.to_string()),
            Ok(None) => {}
        }
        if audio_bytes(path) > 0 {
            return Started::Running;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tracing::warn!(
        "mic: no audio from ffmpeg after {} s; is the input device sending any?",
        STARTUP.as_secs()
    );
    Started::Running
}

/// Sends SIGINT (on Windows Ctrl+Break to ffmpeg's console process
/// group), so ffmpeg finishes the file, and kills it if it has not exited
/// after [`GRACE`].
async fn finish(child: &mut Child) {
    if let Ok(None) = child.try_wait()
        && let Some(pid) = child.id()
    {
        #[cfg(unix)]
        // SAFETY: plain kill(2) on our own child.
        let asked = unsafe { libc::kill(pid as i32, libc::SIGINT) } == 0;
        #[cfg(windows)]
        let asked = crate::windows::process::ctrl_break(pid);
        #[cfg(not(any(unix, windows)))]
        let asked = {
            let _ = pid;
            false
        };
        if asked && tokio::time::timeout(GRACE, child.wait()).await.is_ok() {
            return;
        }
    }
    let _ = child.kill().await;
}

#[async_trait]
impl MicProvider for FfmpegMic {
    async fn record_start(
        &self,
        caller: &Caller,
        options: RecordOptions,
    ) -> Result<u64, CallError> {
        if let Some((id, _)) = self
            .recordings
            .lock()
            .unwrap()
            .iter()
            .find(|(_, r)| r.plugin == caller.plugin)
        {
            return Err(CallError::Failed(format!("already recording ({id})")));
        }
        if !self.starting.lock().unwrap().insert(caller.plugin.clone()) {
            return Err(CallError::Failed("already starting a recording".into()));
        }
        let result = self.start(caller, options).await;
        self.starting.lock().unwrap().remove(&caller.plugin);
        result
    }

    async fn record_stop(&self, caller: &Caller, id: u64) -> Result<String, CallError> {
        let mut r = self.take(caller, id)?;
        finish(&mut r.child).await;
        let size = audio_bytes(&r.path);
        tracing::info!(plugin = %caller.plugin, "mic: recording {id} stopped, {size} bytes of audio");
        if size == 0 {
            let _ = tokio::fs::remove_file(&r.path).await;
            return Err(CallError::Failed(format!(
                "ffmpeg recorded no audio: {}",
                last_line(&r.stderr)
            )));
        }
        if let Some(echo) = &self.echo {
            let (echo, path, ended) = (echo.clone(), r.path.clone(), crate::echo::now_ms());
            // Samples in memory and a filter: tens of milliseconds per
            // second of audio, off the async threads.
            match tokio::task::spawn_blocking(move || echo.cancel_file(&path, ended)).await {
                Ok(Ok(0)) => {}
                Ok(Ok(n)) => tracing::info!(
                    plugin = %caller.plugin,
                    "mic: recording {id}: removed the node's own speech from {n} samples"
                ),
                Ok(Err(e)) => tracing::warn!("mic: echo cancellation skipped: {e}"),
                Err(e) => tracing::warn!("mic: echo cancellation failed: {e}"),
            }
        }
        Ok(r.path.to_string_lossy().into_owned())
    }

    async fn record_cancel(&self, caller: &Caller, id: u64) -> Result<(), CallError> {
        let mut r = self.take(caller, id)?;
        let _ = r.child.kill().await;
        let _ = tokio::fs::remove_file(&r.path).await;
        tracing::info!(plugin = %caller.plugin, "mic: recording {id} cancelled");
        Ok(())
    }

    fn release(&self, plugin: &str) {
        let mut recordings = self.recordings.lock().unwrap();
        let ids: Vec<u64> = recordings
            .iter()
            .filter(|(_, r)| r.plugin == plugin)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(mut r) = recordings.remove(&id) {
                // SIGKILL before the file goes, so ffmpeg can't write it
                // again. Tokio reaps the child.
                let _ = r.child.start_kill();
                let _ = std::fs::remove_file(&r.path);
                self.sensors.set(plugin, Sensor::Mic, false);
                tracing::info!(plugin, "mic: recording {id} dropped");
            }
        }
    }

    async fn shutdown(&self) {
        let all: Vec<Recording> = self
            .recordings
            .lock()
            .unwrap()
            .drain()
            .map(|(_, r)| r)
            .collect();
        for mut r in all {
            self.sensors.set(&r.plugin, Sensor::Mic, false);
            let _ = r.child.kill().await;
            let _ = tokio::fs::remove_file(&r.path).await;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Sizes below this hold no samples: the shortest WAV header. The
    /// provider looks for the `data` chunk instead.
    const WAV_HEADER: u64 = 44;

    /// A stand-in for ffmpeg: logs its arguments next to itself, fails
    /// for the input "missing", writes a WAV with 3200 bytes of samples to
    /// its last argument,
    /// then waits for SIGINT, or for `-t` seconds.
    const FAKE: &str = r#"#!/bin/sh
trap 'exit 255' INT
echo "$@" >> "$(dirname "$0")/args.log"
for a in "$@"; do out="$a"; done
case " $* " in *" -i missing "*) echo "missing: No such device" >&2; exit 1 ;; esac
secs=600
prev=""
for a in "$@"; do [ "$prev" = "-t" ] && secs="$a"; prev="$a"; done
{ printf 'RIFF\0\0\0\0WAVEdata\0\0\0\0'; head -c 3200 /dev/zero; } > "$out"
i=0
while [ "$i" -lt "$((secs * 10))" ]; do sleep 0.1; i=$((i + 1)); done
exit 0
"#;

    struct Fixture {
        dir: tempfile::TempDir,
        mic: FfmpegMic,
        sensors: Arc<Sensors>,
    }

    impl Fixture {
        /// The callers whose `mic` shows in the sensors.
        fn active(&self) -> Vec<String> {
            self.sensors
                .active()
                .into_iter()
                .map(|a| a.plugin)
                .collect()
        }
    }

    fn fixture(inputs: &[&str]) -> Fixture {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("ffmpeg");
        std::fs::write(&fake, FAKE).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let inputs = inputs
            .iter()
            .map(|i| vec!["-f".into(), "test".into(), "-i".into(), i.to_string()])
            .collect();
        let sensors: Arc<Sensors> = Arc::default();
        let mic = FfmpegMic::with_inputs(
            dir.path().join("data"),
            Some(fake.to_string_lossy().into_owned()),
            inputs,
            sensors.clone(),
        );
        Fixture { dir, mic, sensors }
    }

    /// Waits up to 20 s for `check`. A script written a moment ago can
    /// take seconds to start on a loaded Mac, while macOS checks it.
    async fn until(what: &str, check: impl Fn() -> bool) {
        let start = std::time::Instant::now();
        while !check() {
            assert!(start.elapsed() < Duration::from_secs(20), "{what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Waits until the fake has written its file for `caller`.
    async fn recording(f: &Fixture, caller: &str) {
        let dir = f.dir.path().join("data").join(caller).join("recordings");
        until("the fake ffmpeg never wrote", || {
            std::fs::read_dir(&dir).is_ok_and(|d| {
                d.flatten()
                    .any(|e| e.metadata().is_ok_and(|m| m.len() > WAV_HEADER))
            })
        })
        .await;
    }

    fn opts(max_secs: u32) -> RecordOptions {
        RecordOptions {
            max_secs,
            format: AudioFormat::Wav16kMono,
        }
    }

    #[test]
    fn dshow_devices_are_read_from_both_list_formats() {
        let new = r#"[dshow @ 000001d2] "Integrated Camera" (video)
[dshow @ 000001d2]   Alternative name "@device_pnp_\\?\usb#vid_04f2"
[dshow @ 000001d2] "OBS Virtual Camera" (none)
[dshow @ 000001d2] "Microphone Array (Realtek(R) Audio)" (audio)
[dshow @ 000001d2]   Alternative name "@device_cm_{33D9A762}\wave_{1}"
[dshow @ 000001d2] "Headset (Jabra)" (audio)
dummy: Immediate exit requested"#;
        assert_eq!(
            parse_dshow_devices(new),
            DshowDevices {
                audio: vec![
                    "Microphone Array (Realtek(R) Audio)".into(),
                    "Headset (Jabra)".into()
                ],
                video: vec!["Integrated Camera".into()],
            }
        );
        let old = r#"[dshow @ 0x1] DirectShow video devices (some may be both video and audio devices)
[dshow @ 0x1]  "USB2.0 HD UVC WebCam"
[dshow @ 0x1]     Alternative name "@device_pnp_x"
[dshow @ 0x1] DirectShow audio devices
[dshow @ 0x1]  "Microphone (USB Audio)"
[dshow @ 0x1]     Alternative name "@device_cm_y""#;
        assert_eq!(
            parse_dshow_devices(old),
            DshowDevices {
                audio: vec!["Microphone (USB Audio)".into()],
                video: vec!["USB2.0 HD UVC WebCam".into()],
            }
        );
        assert_eq!(parse_dshow_devices(""), DshowDevices::default());
    }

    #[test]
    fn dshow_inputs_take_the_device_or_each_listed_one() {
        let listed = || vec!["Mic A".to_string(), "Mic B".to_string()];
        assert_eq!(
            dshow_inputs("audio", None, listed),
            [
                ["-f", "dshow", "-i", "audio=Mic A"],
                ["-f", "dshow", "-i", "audio=Mic B"]
            ]
        );
        let unused = || -> Vec<String> { panic!("no listing with a device set") };
        assert_eq!(
            dshow_inputs("audio", Some("Headset"), unused),
            [["-f", "dshow", "-i", "audio=Headset"]]
        );
        assert_eq!(
            dshow_inputs("video", Some("video=Cam"), Vec::new),
            [["-f", "dshow", "-i", "video=Cam"]]
        );
        assert!(dshow_inputs("audio", None, Vec::new).is_empty());
    }

    #[test]
    fn arguments_ask_for_16k_mono_wav() {
        let input = default_inputs(Some("Mic"));
        if cfg!(target_os = "macos") {
            assert_eq!(input, [["-f", "avfoundation", "-i", ":Mic"]]);
            assert_eq!(default_inputs(Some(":1"))[0][3], ":1");
        }
        if cfg!(target_os = "linux") {
            assert_eq!(input[0][1], "pulse");
            assert_eq!(input[1][1], "alsa");
        }
        if cfg!(target_os = "windows") {
            assert_eq!(input, [["-f", "dshow", "-i", "audio=Mic"]]);
        }
        let args = ffmpeg_args(&["-i".into(), "x".into()], opts(0), Path::new("/o.wav"));
        let text = args.join(" ");
        assert!(
            text.ends_with("-i x -t 600 -ar 16000 -ac 1 -c:a pcm_s16le -flush_packets 1 /o.wav"),
            "{text}"
        );
        let args = ffmpeg_args(&[], opts(9999), Path::new("/o.wav"));
        assert!(args.join(" ").contains("-t 600 "));
    }

    #[test]
    fn audio_bytes_count_what_follows_the_data_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        // ffmpeg's header with no samples: 78 bytes, a LIST chunk first.
        let mut header = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        header.extend([0u8; 20]);
        header.extend(b"LIST\x1a\0\0\0INFOISFT\x0e\0\0\0Lavf62.12.100\0");
        header.extend(b"data\0\0\0\0");
        assert_eq!(header.len(), 78);
        std::fs::write(&wav, &header).unwrap();
        assert_eq!(audio_bytes(&wav), 0);
        header.extend([0u8; 640]);
        std::fs::write(&wav, &header).unwrap();
        assert_eq!(audio_bytes(&wav), 640);
        assert_eq!(audio_bytes(&dir.path().join("missing.wav")), 0);
        std::fs::write(&wav, [0u8; 3200]).unwrap();
        assert_eq!(audio_bytes(&wav), 0);
    }

    #[test]
    fn prune_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("rec-{i:015}-{i}.wav")), "").unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        prune(dir.path(), 2);
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "notes.txt",
                "rec-000000000000003-3.wav",
                "rec-000000000000004-4.wav"
            ]
        );
    }

    #[tokio::test]
    async fn start_and_stop_write_a_file_in_the_callers_folder() {
        let f = fixture(&["default"]);
        let caller = Caller::new("dict");
        let id = f.mic.record_start(&caller, opts(60)).await.unwrap();
        recording(&f, "dict").await;
        assert_eq!(f.active(), ["dict"]);
        // One recording at a time per caller.
        let again = f.mic.record_start(&caller, opts(60)).await;
        assert!(
            matches!(&again, Err(CallError::Failed(m)) if m.contains("already")),
            "{again:?}"
        );
        // Only the owner can stop it.
        let other = f.mic.record_stop(&Caller::new("other"), id).await;
        assert!(other.is_err());
        let path = f.mic.record_stop(&caller, id).await.unwrap();
        let expected = f.dir.path().join("data/dict/recordings");
        assert!(Path::new(&path).starts_with(&expected), "{path}");
        assert_eq!(audio_bytes(Path::new(&path)), 3200);
        assert!(f.active().is_empty());
        // Stopped once; the id is gone.
        assert!(f.mic.record_stop(&caller, id).await.is_err());
        let log = std::fs::read_to_string(f.dir.path().join("args.log")).unwrap();
        assert!(log.contains("-t 60 -ar 16000 -ac 1"), "{log}");
    }

    #[tokio::test]
    async fn cancel_and_release_delete_the_file() {
        let f = fixture(&["default"]);
        let caller = Caller::new("dict");
        let id = f.mic.record_start(&caller, opts(60)).await.unwrap();
        f.mic.record_cancel(&caller, id).await.unwrap();
        let dir = f.dir.path().join("data/dict/recordings");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        f.mic.record_start(&caller, opts(60)).await.unwrap();
        f.mic.release("dict");
        assert!(f.active().is_empty());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        // Released, the caller can record again; shutdown stops it.
        f.mic.record_start(&caller, opts(60)).await.unwrap();
        f.mic.shutdown().await;
        assert!(f.active().is_empty());
    }

    #[tokio::test]
    async fn two_starts_at_once_give_one_recording() {
        let f = fixture(&["default"]);
        let caller = Caller::new("dict");
        let (a, b) = tokio::join!(
            f.mic.record_start(&caller, opts(60)),
            f.mic.record_start(&caller, opts(60))
        );
        assert!(a.is_ok() != b.is_ok(), "{a:?} {b:?}");
        f.mic.shutdown().await;
    }

    #[tokio::test]
    async fn max_secs_stops_the_recording_by_itself() {
        let f = fixture(&["default"]);
        let caller = Caller::new("dict");
        let id = f.mic.record_start(&caller, opts(1)).await.unwrap();
        recording(&f, "dict").await;
        until("the recording never stopped", || f.active().is_empty()).await;
        let path = f.mic.record_stop(&caller, id).await.unwrap();
        assert!(Path::new(&path).is_file());
    }

    #[tokio::test]
    async fn a_missing_input_falls_back_to_the_next() {
        let f = fixture(&["missing", "default"]);
        let caller = Caller::new("dict");
        let id = f.mic.record_start(&caller, opts(60)).await.unwrap();
        recording(&f, "dict").await;
        f.mic.record_stop(&caller, id).await.unwrap();
        let log = std::fs::read_to_string(f.dir.path().join("args.log")).unwrap();
        assert_eq!(log.lines().count(), 2, "{log}");

        let f = fixture(&["missing"]);
        let err = f.mic.record_start(&caller, opts(60)).await;
        assert!(
            matches!(&err, Err(CallError::Failed(m)) if m.contains("No such device")),
            "{err:?}"
        );
        assert!(f.active().is_empty());
    }

    #[tokio::test]
    async fn no_ffmpeg_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let mic = FfmpegMic::with_inputs(dir.path().into(), None, Vec::new(), Arc::default());
        let r = mic.record_start(&Caller::new("p"), opts(1)).await;
        assert_eq!(r, Err(CallError::Unavailable));
    }

    #[tokio::test]
    async fn old_recordings_are_pruned() {
        let f = fixture(&["default"]);
        let caller = Caller::new("dict");
        let dir = f.dir.path().join("data/dict/recordings");
        for n in 0..KEEP + 2 {
            let id = f.mic.record_start(&caller, opts(60)).await.unwrap();
            let count = (n + 1).min(KEEP);
            until("the fake ffmpeg never wrote", || {
                std::fs::read_dir(&dir).is_ok_and(|d| {
                    d.flatten()
                        .filter(|e| e.metadata().is_ok_and(|m| m.len() > WAV_HEADER))
                        .count()
                        == count
                })
            })
            .await;
            f.mic.record_stop(&caller, id).await.unwrap();
        }
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), KEEP);
    }

    /// Records two seconds from the real microphone. Needs ffmpeg, a
    /// microphone, and on macOS the microphone permission for the app
    /// running the test (the terminal).
    #[ignore = "needs a microphone and ffmpeg"]
    #[tokio::test]
    async fn real_mic_records_two_seconds_of_16k_mono() {
        let dir = tempfile::tempdir().unwrap();
        let mic = FfmpegMic::new(dir.path().into(), &MicConfig::default(), Arc::default());
        let caller = Caller::new("real");
        let id = mic.record_start(&caller, opts(10)).await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        let path = mic.record_stop(&caller, id).await.unwrap();
        let wav = std::fs::read(&path).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1, "channels");
        let rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        assert_eq!(rate, 16000);
        // About two seconds of 16-bit samples, give or take the startup.
        let secs = (wav.len() as f64 - 44.0) / 32000.0;
        assert!((1.0..3.0).contains(&secs), "{secs} s");
    }
}
