# 03: screen capture and selected text

Read `.agents/specs/README.md` first. This track adds two capabilities: `screen` and `selection`.

## Contract

`wit/screen.wit`:

```wit
/// Pictures of this device's screens. Needs the Screen Recording
/// permission on macOS; without it calls answer `denied`. Always runs on
/// this node.
interface screen {
  use types.{call-error};
  use window.{rect};

  /// Saves a PNG of `area`, or of the whole main screen when none, in the
  /// plugin's data folder, and returns its path. An empty area answers
  /// `failed`.
  capture: async func(area: option<rect>) -> result<string, call-error>;
}
```

`wit/selection.wit`:

```wit
/// The text selected in the frontmost app. Needs the Accessibility
/// permission on macOS; without it calls answer `denied`. Always runs on
/// this node.
interface selection {
  use types.{call-error};

  /// The selected text, or none when nothing is selected or the app does
  /// not say.
  get-text: async func() -> result<option<string>, call-error>;
}
```

## Rust

`crates/providers/src/screen.rs`:

```rust
#[async_trait]
pub trait ScreenProvider: Send + Sync {
    /// Returns the path of the PNG it wrote.
    async fn capture(&self, caller: &Caller, area: Option<Rect>) -> Result<String, CallError>;
}

/// No screen capture on this node.
pub struct NoScreen;

/// `Failed("the area is empty")` when `width` or `height` is 0.
pub fn check_area(area: &Rect) -> Result<(), CallError>;

/// The arguments for macOS `/usr/sbin/screencapture`: no shutter sound
/// (`-x`), then `-m` (main screen only) or `-R x,y,w,h`, then `path`.
pub fn screencapture_args(area: Option<Rect>, path: &Path) -> Vec<String>;
```

`Rect` is `crate::window::Rect`. The host binding and the Lua binding call `check_area` before `capture`.

`crates/providers/src/selection.rs`:

```rust
#[async_trait]
pub trait SelectionProvider: Send + Sync {
    async fn get_text(&self, caller: &Caller) -> Result<Option<String>, CallError>;
}

/// No selection on this node.
pub struct NoSelection;
```

## Backends

- macOS `screen`: run `/usr/sbin/screencapture` with `screencapture_args`. Write the file in the plugin's data folder, in a `screenshots` subfolder, named after the current time in milliseconds (`<ms>.png`). Use the same data folder that `mic` uses for recordings: read how `mic` picks its folder and do the same. A non-zero exit, or a missing or empty file afterwards, answers `Failed` with screencapture's stderr. Without the Screen Recording permission, screencapture writes nothing or an empty file; answer `Denied` in that case.
- macOS `selection`: Accessibility. Take the system-wide element's `AXFocusedUIElement`, then its `AXSelectedText`. A missing attribute or an empty string answers `Ok(None)`. Read how `macos/window.rs` uses AX and does the permission check, and reuse its helpers.
- Linux `selection` (X11 and Wayland): the PRIMARY selection, through arboard's Linux extension (`GetExtLinux` with `LinuxClipboardKind::Primary`) on the clipboard thread `linux/clipboard.rs` already owns. Linux `screen` answers `Unavailable`.
- Both need a `Desktop` on macOS, per README rule 5.

## Lua

`bricolo.screen.capture([area])` takes `{x, y, w, h}`, where `width` and `height` also work, as `bricolo.window.set_frame` accepts. It returns the path, or `nil, err`. `bricolo.selection.get_text()` returns the text or `nil`.

## Tests

### Unit tests: in `crates/providers/src/screen.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screencapture_takes_the_main_screen_or_an_area() {
        let path = Path::new("/tmp/shot.png");
        assert_eq!(screencapture_args(None, path), ["-x", "-m", "/tmp/shot.png"]);
        let area = Rect { x: 10, y: -20, width: 300, height: 200 };
        assert_eq!(
            screencapture_args(Some(area), path),
            ["-x", "-R", "10,-20,300,200", "/tmp/shot.png"]
        );
    }

    #[test]
    fn empty_areas_are_refused() {
        let area = Rect { x: 0, y: 0, width: 0, height: 10 };
        assert_eq!(
            check_area(&area),
            Err(CallError::Failed("the area is empty".into()))
        );
        assert!(check_area(&Rect { width: 1, height: 1, ..area }).is_ok());
    }
}
```

### Lua: `crates/lua/tests/scripts.rs`

Add fakes, plus `screen: Arc<FakeScreen>` and `selection: Arc<FakeSelection>` on `Node`, wired into `Providers`:

```rust
/// Records each area; always "writes" /tmp/shot.png.
#[derive(Default)]
struct FakeScreen(Mutex<Vec<Option<Rect>>>);

#[async_trait]
impl ScreenProvider for FakeScreen {
    async fn capture(&self, _: &Caller, area: Option<Rect>) -> Result<String, CallError> {
        self.0.lock().unwrap().push(area);
        Ok("/tmp/shot.png".into())
    }
}

/// "hello" is selected.
struct FakeSelection;

#[async_trait]
impl SelectionProvider for FakeSelection {
    async fn get_text(&self, _: &Caller) -> Result<Option<String>, CallError> {
        Ok(Some("hello".into()))
    }
}
```

```rust
#[tokio::test(flavor = "multi_thread")]
async fn screen_capture_and_selected_text() {
    let mut node = Node::new();
    node.write(
        "look.lua",
        r#"
local all = assert(bricolo.screen.capture())
local part = assert(bricolo.screen.capture({x = 10, y = 20, w = 300, h = 200}))
local _, err = bricolo.screen.capture({x = 0, y = 0, w = 0, h = 10})
bricolo.log.info("done", all, part, err, bricolo.selection.get_text())
"#,
    );
    node.start(false).await;
    node.expect_line(
        "look",
        "done /tmp/shot.png /tmp/shot.png failed: the area is empty hello",
    )
    .await;
    assert_eq!(
        *node.screen.0.lock().unwrap(),
        [
            None,
            Some(Rect { x: 10, y: 20, width: 300, height: 200 })
        ]
    );
}
```

### Runtime: `crates/runtime/tests/plugins.rs`

Add `caps` commands: `shot` logs `shot: {:?}` of `screen::capture(None)`, `shot-empty` logs `shot-empty: {:?}` of `screen::capture(Some(Rect { x: 0, y: 0, width: 0, height: 5 }))`, and `selection` logs `selection: {:?}` of `selection::get_text()`.

```rust
/// `screen` and `selection` go through the host. Tests have no main-thread
/// loop, so both answer with an error, and an empty area is refused first.
#[tokio::test(flavor = "multi_thread")]
async fn screen_and_selection_reach_the_host() {
    let node = Node::new();
    node.install_caps(&caps_manifest(&["/bin/echo"], &["127.0.0.1:1".into()]));
    let rt = node.start(false).await;
    let fire = |cmd: &str| rt.fire("caps", ProviderEvent::Hotkey(cmd.into())).unwrap();
    fire("shot");
    fire("selection");
    fire("shot-empty");
    for prefix in ["shot:", "selection:"] {
        let got = line(&rt, "caps", prefix).await;
        assert!(got.contains("Err(CallError::"), "{got}");
    }
    assert_eq!(
        line(&rt, "caps", "shot-empty:").await,
        r#"shot-empty: Err(CallError::Failed("the area is empty"))"#
    );
    rt.shutdown().await;
}
```

On Linux with a display, `selection` may answer `Ok`. Skip this test there with `linux_display()`, as `desktop_capabilities_without_the_main_thread_loop` does.

### macOS, ignored: in `crates/providers/src/screen.rs` tests

```rust
    /// Needs a GUI session and the Screen Recording permission for the test
    /// binary's terminal.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn macos_screencapture_writes_a_png() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        let status = std::process::Command::new("/usr/sbin/screencapture")
            .args(screencapture_args(None, &path))
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"\x89PNG");
    }
```

## Done when

The README's "Done when" commands pass.
