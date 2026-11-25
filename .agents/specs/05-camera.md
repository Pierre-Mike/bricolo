# 05: camera

Read `.agents/specs/README.md` first. This track adds the capability `camera`: one photo from the default camera, taken with an ffmpeg child process, the way `mic` records.

## Contract: `wit/camera.wit`

```wit
/// Photos from this device's camera, taken with ffmpeg like `mic`. Needs
/// the Camera permission on macOS; without it calls answer `failed`.
/// Always runs on this node.
interface camera {
  use types.{call-error};

  /// Takes one photo with the default camera, saves it as a JPEG in the
  /// plugin's data folder and returns its path.
  snapshot: async func() -> result<string, call-error>;
}
```

## Rust: `crates/providers/src/camera.rs`

```rust
#[async_trait]
pub trait CameraProvider: Send + Sync {
    async fn snapshot(&self, caller: &Caller) -> Result<String, CallError>;
}

/// No camera on this node.
pub struct NoCamera;

/// `[camera]` in `node.toml`: `device` (an avfoundation video device name
/// or index on macOS, a V4L2 device path on Linux) and `ffmpeg`, as
/// `MicConfig` has.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CameraConfig { pub device: Option<String>, pub ffmpeg: Option<String> }

/// The ffmpeg input for this system: `-f avfoundation -framerate 30 -i
/// <device or 0>` on macOS, `-f v4l2 -i <device or /dev/video0>` on Linux.
pub fn default_input(device: Option<&str>) -> Vec<String>;

/// The whole ffmpeg command line after the program: quiet, the input,
/// one frame, overwrite `out`.
pub fn ffmpeg_args(input: &[String], out: &Path) -> Vec<String>;

/// Takes photos into `<data_dir>/<plugin>/camera/<ms>.jpg`.
pub struct FfmpegCamera { /* ... */ }
impl FfmpegCamera {
    pub fn new(data_dir: PathBuf, config: &CameraConfig, sensors: Arc<Sensors>) -> Self;
    /// For tests: this ffmpeg and this input.
    pub fn with_ffmpeg(data_dir: PathBuf, ffmpeg: PathBuf, input: Vec<String>) -> Self;
}
```

- Find ffmpeg as `mic` does (`mic::find_ffmpeg`). No ffmpeg answers `Unavailable`.
- While a snapshot runs, record the plugin as using the `camera` sensor in `Sensors`, as `mic` does for the microphone.
- An ffmpeg that exits non-zero answers `Failed` with the last line of its stderr, and leaves no file. Stop ffmpeg after 10 seconds and answer `Timeout`.
- Keep the newest 20 photos per plugin and delete older ones, as `mic` prunes recordings.
- Read `[camera]` from `node.toml` the way `[mic]` is read. Document it next to `[mic]` in the phase 2 implementation notes of `docs/design.md`.
- `Providers::local` builds `FfmpegCamera` the way it builds `FfmpegMic`. That is safe in tests because the runtime test points `[camera] ffmpeg` at a fake script, as the mic runtime test does.
- macOS needs `NSCameraUsageDescription` in the `bricolo` Info.plist that `crates/node/build.rs` embeds, next to the microphone usage string. Add one: "bricolo takes a photo when an app you installed asks for one."

## Lua

`bricolo.camera.snapshot()` returns the path, or `nil, err`.

## Tests

### Unit tests: in `crates/providers/src/camera.rs`

```rust
#[cfg(test)]
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
        let input = vec!["-f".to_string(), "avfoundation".into(), "-i".into(), "0".into()];
        assert_eq!(
            ffmpeg_args(&input, Path::new("/tmp/a.jpg")),
            [
                "-hide_banner", "-loglevel", "error", "-f", "avfoundation", "-i", "0",
                "-frames:v", "1", "-y", "/tmp/a.jpg"
            ]
        );
    }

    #[test]
    fn default_inputs_name_the_device() {
        if cfg!(target_os = "macos") {
            assert_eq!(default_input(None), ["-f", "avfoundation", "-framerate", "30", "-i", "0"]);
            assert_eq!(default_input(Some("1")), ["-f", "avfoundation", "-framerate", "30", "-i", "1"]);
        } else if cfg!(target_os = "linux") {
            assert_eq!(default_input(None), ["-f", "v4l2", "-i", "/dev/video0"]);
        }
    }

    #[tokio::test]
    async fn snapshot_runs_ffmpeg_and_returns_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let ok = script(dir.path(), "ok", "for last; do :; done\necho jpeg > \"$last\"");
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
```

Gate the module's tests with `#[cfg(all(test, unix))]` if `PermissionsExt` breaks a non-Unix build.

### Lua: `crates/lua/tests/scripts.rs`

```rust
struct FakeCamera;

#[async_trait]
impl CameraProvider for FakeCamera {
    async fn snapshot(&self, _: &Caller) -> Result<String, CallError> {
        Ok("/tmp/cam.jpg".into())
    }
}
```

Wire `camera: Arc::new(FakeCamera)` into the test `Providers`.

```rust
#[tokio::test(flavor = "multi_thread")]
async fn camera_snapshot() {
    let mut node = Node::new();
    node.write("cam.lua", r#"bricolo.log.info("cam", assert(bricolo.camera.snapshot()))"#);
    node.start(false).await;
    node.expect_line("cam", "cam /tmp/cam.jpg").await;
}
```

### Runtime: `crates/runtime/tests/plugins.rs`

Add the `caps` command `snapshot`, which logs `snapshot: {:?}` of `camera::snapshot()`. Write the test after the mic runtime test, configuring `[camera] ffmpeg` in `node.toml` the same way that test configures `[mic] ffmpeg`. The fake ffmpeg writes `jpeg` to its last argument.

```rust
/// A plugin takes a photo through the host; the fake ffmpeg writes the file.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn camera_snapshot_through_a_plugin() {
    // Set up the node, a fake ffmpeg and node.toml as the mic test does.
    // Then:
    //   rt.fire("caps", ProviderEvent::Hotkey("snapshot".into())).unwrap();
    //   let got = line(&rt, "caps", "snapshot:").await;
    //   assert!(got.starts_with("snapshot: Ok(\""), "{got}");
    //   the path between the quotes exists and contains "jpeg"
}
```

Only the setup in this test may follow the mic test. The assertions are the ones in the comment.

## Done when

The README's "Done when" commands pass.
