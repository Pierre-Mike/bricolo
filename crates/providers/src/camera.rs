//! `camera`: one photo from the default camera, taken with an ffmpeg child
//! process the way `mic` records. macOS reads `avfoundation`; Linux reads
//! V4L2. The file is a JPEG in `<data>/<plugin>/camera/<ms>.jpg`, which
//! the plugin sees at `/data/camera/`. The newest [`KEEP`] photos stay.
//!
//! `node.toml`:
//!
//! ```toml
//! [camera]
//! device = "0"                         # avfoundation name or index; /dev/video0 on Linux
//! ffmpeg = "/opt/homebrew/bin/ffmpeg"  # default: the first ffmpeg found
//! ```
//!
//! A snapshot counts in [`Sensors`] as the caller's `camera` while ffmpeg
//! runs. `camera` never runs on a peer.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;

use crate::caller::Caller;
use crate::error::CallError;
use crate::mic::find_ffmpeg;
use crate::sensors::{Sensor, Sensors};

/// How many photos each caller keeps.
pub const KEEP: usize = 20;
/// ffmpeg is stopped after this long.
const TIMEOUT: Duration = Duration::from_secs(10);

#[async_trait]
pub trait CameraProvider: Send + Sync {
    /// Takes one photo for the caller and returns its path.
    async fn snapshot(&self, caller: &Caller) -> Result<String, CallError>;
}

/// No camera on this node.
pub struct NoCamera;

#[async_trait]
impl CameraProvider for NoCamera {
    async fn snapshot(&self, _: &Caller) -> Result<String, CallError> {
        Err(CallError::Unavailable)
    }
}

/// `[camera]` in `node.toml`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CameraConfig {
    /// An avfoundation video device name or index on macOS, a V4L2 device
    /// path on Linux, a DirectShow device name on Windows.
    #[serde(default)]
    pub device: Option<String>,
    /// The ffmpeg program. `~/` expands.
    #[serde(default)]
    pub ffmpeg: Option<String>,
}

/// The ffmpeg input arguments for this system.
pub fn default_input(device: Option<&str>) -> Vec<String> {
    let args = |items: &[&str]| items.iter().map(|s| s.to_string()).collect();
    if cfg!(target_os = "macos") {
        args(&[
            "-f",
            "avfoundation",
            "-framerate",
            "30",
            "-i",
            device.unwrap_or("0"),
        ])
    } else if cfg!(target_os = "linux") {
        args(&["-f", "v4l2", "-i", device.unwrap_or("/dev/video0")])
    } else if cfg!(target_os = "windows") {
        // DirectShow: the device named in node.toml, else the first camera
        // ffmpeg lists.
        crate::mic::dshow_inputs("video", device, || {
            crate::mic::dshow_listed()
                .video
                .into_iter()
                .take(1)
                .collect()
        })
        .into_iter()
        .next()
        .unwrap_or_default()
    } else {
        Vec::new()
    }
}

/// The ffmpeg arguments that save one frame of `input` to `out`.
pub fn ffmpeg_args(input: &[String], out: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error"]
        .map(String::from)
        .to_vec();
    args.extend(input.iter().cloned());
    args.extend(["-frames:v", "1", "-y"].map(String::from));
    args.push(out.to_string_lossy().into_owned());
    args
}

/// Deletes all but the newest `keep` photos in `dir`.
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".jpg"))
        .collect();
    // Names are millisecond timestamps; pad by length so they sort by age.
    names.sort_by(|a, b| (a.len(), a).cmp(&(b.len(), b)));
    let extra = names.len().saturating_sub(keep);
    for name in &names[..extra] {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

/// Clears the `camera` sensor when dropped, so a cancelled call does too.
struct InUse<'a> {
    sensors: &'a Sensors,
    plugin: &'a str,
}

impl Drop for InUse<'_> {
    fn drop(&mut self) {
        self.sensors.set(self.plugin, Sensor::Camera, false);
    }
}

/// Takes photos through ffmpeg into `<data_dir>/<plugin>/camera/`.
pub struct FfmpegCamera {
    data_dir: PathBuf,
    ffmpeg: Option<String>,
    input: Vec<String>,
    sensors: Arc<Sensors>,
}

impl FfmpegCamera {
    pub fn new(data_dir: PathBuf, config: &CameraConfig, sensors: Arc<Sensors>) -> Self {
        Self {
            data_dir,
            ffmpeg: config.ffmpeg.clone(),
            input: default_input(config.device.as_deref()),
            sensors,
        }
    }

    /// For tests: this ffmpeg and this input.
    pub fn with_ffmpeg(data_dir: PathBuf, ffmpeg: PathBuf, input: Vec<String>) -> Self {
        Self {
            data_dir,
            ffmpeg: Some(ffmpeg.to_string_lossy().into_owned()),
            input,
            sensors: Arc::default(),
        }
    }
}

#[async_trait]
impl CameraProvider for FfmpegCamera {
    async fn snapshot(&self, caller: &Caller) -> Result<String, CallError> {
        let Some(program) = find_ffmpeg(self.ffmpeg.as_deref()) else {
            tracing::warn!(
                "camera: ffmpeg not found; install it or set [camera] ffmpeg in node.toml"
            );
            return Err(CallError::Unavailable);
        };
        let dir = self.data_dir.join(&caller.plugin).join("camera");
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| CallError::Failed(format!("cannot create {}: {e}", dir.display())))?;
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let path = dir.join(format!("{ms}.jpg"));
        let args = ffmpeg_args(&self.input, &path);

        self.sensors.set(&caller.plugin, Sensor::Camera, true);
        let _in_use = InUse {
            sensors: &self.sensors,
            plugin: &caller.plugin,
        };
        let child = Command::new(&program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| CallError::Failed(format!("cannot start {}: {e}", program.display())))?;
        // Dropping the future on timeout kills the child (`kill_on_drop`).
        let output = match tokio::time::timeout(TIMEOUT, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(CallError::Failed(format!("ffmpeg: {e}"))),
            Err(_) => {
                let _ = tokio::fs::remove_file(&path).await;
                return Err(CallError::Timeout);
            }
        };
        if !output.status.success() {
            let _ = tokio::fs::remove_file(&path).await;
            let stderr = String::from_utf8_lossy(&output.stderr);
            let why = stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map_or_else(
                    || format!("ffmpeg exited ({})", output.status),
                    |l| l.trim().to_string(),
                );
            return Err(CallError::Failed(why));
        }
        if !path.is_file() {
            return Err(CallError::Failed("ffmpeg wrote no photo".into()));
        }
        prune(&dir, KEEP);
        tracing::info!(plugin = %caller.plugin, "camera: photo {}", path.display());
        Ok(path.to_string_lossy().into_owned())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn ffmpeg_takes_one_frame() {
        let input = vec![
            "-f".to_string(),
            "avfoundation".into(),
            "-i".into(),
            "0".into(),
        ];
        assert_eq!(
            ffmpeg_args(&input, Path::new("/tmp/a.jpg")),
            [
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "avfoundation",
                "-i",
                "0",
                "-frames:v",
                "1",
                "-y",
                "/tmp/a.jpg"
            ]
        );
    }

    #[test]
    fn default_inputs_name_the_device() {
        if cfg!(target_os = "macos") {
            assert_eq!(
                default_input(None),
                ["-f", "avfoundation", "-framerate", "30", "-i", "0"]
            );
            assert_eq!(
                default_input(Some("1")),
                ["-f", "avfoundation", "-framerate", "30", "-i", "1"]
            );
        } else if cfg!(target_os = "linux") {
            assert_eq!(default_input(None), ["-f", "v4l2", "-i", "/dev/video0"]);
        }
    }

    #[tokio::test]
    async fn snapshot_runs_ffmpeg_and_returns_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let ok = script(
            dir.path(),
            "ok",
            "for last; do :; done\necho jpeg > \"$last\"",
        );
        let camera = FfmpegCamera::with_ffmpeg(dir.path().join("data"), ok, vec![]);
        let path = camera.snapshot(&Caller::new("cam")).await.unwrap();
        assert!(path.ends_with(".jpg"), "{path}");
        assert!(path.contains("/cam/camera/"), "{path}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "jpeg\n");
    }

    #[tokio::test]
    async fn a_failing_ffmpeg_is_reported_and_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let bad = script(dir.path(), "bad", "echo 'no camera found' >&2\nexit 1");
        let camera = FfmpegCamera::with_ffmpeg(dir.path().join("data"), bad, vec![]);
        let err = camera.snapshot(&Caller::new("cam")).await.unwrap_err();
        assert_eq!(err, CallError::Failed("no camera found".into()));
        let folder = dir.path().join("data/cam/camera");
        let left = std::fs::read_dir(&folder).map(|d| d.count()).unwrap_or(0);
        assert_eq!(left, 0);
    }
}
