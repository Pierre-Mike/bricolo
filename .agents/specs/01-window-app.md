# 01: window and app controls

Read `.agents/specs/README.md` first. This track adds functions to two existing capabilities. It adds no new capability.

## Contract

Add to `wit/window.wit`, after `set-frame`:

```wit
  /// Closes the window, as its close button does. The app may ask to save
  /// first.
  close: async func(id: u64) -> result<_, call-error>;
  /// Minimizes the window to the Dock, or restores it when `minimized` is
  /// false.
  set-minimized: async func(id: u64, minimized: bool) -> result<_, call-error>;
```

Add to `wit/app.wit`, after `frontmost`:

```wit
  /// Asks the app to quit, as Cmd+Q does. `app` is a bundle id or a name.
  /// Answers `failed` when no such app runs.
  quit: async func(app: string) -> result<_, call-error>;
  /// Hides the app's windows, as Cmd+H does, or shows them again when
  /// `hidden` is false. Answers `failed` when no such app runs.
  set-hidden: async func(app: string, hidden: bool) -> result<_, call-error>;
```

## Rust

`WindowProvider` gains:

```rust
async fn close(&self, caller: &Caller, id: u64) -> Result<(), CallError>;
async fn set_minimized(&self, caller: &Caller, id: u64, minimized: bool) -> Result<(), CallError>;
```

`AppProvider` gains:

```rust
async fn quit(&self, caller: &Caller, app: String) -> Result<(), CallError>;
async fn set_hidden(&self, caller: &Caller, app: String, hidden: bool) -> Result<(), CallError>;
```

`NoWindows` and `NoApps` answer `Unavailable`.

## Backends

- macOS window: Accessibility, as the existing `macos/window.rs` does. Close presses the window's `AXCloseButton`. Minimize sets `AXMinimized`. An unknown id answers `Failed("no such window")`.
- macOS app: find the app by bundle id or name among running apps (`NSRunningApplication`), then `terminate` or `hide`/`unhide`. No match answers `Failed("no such app")`.
- Linux window (X11): close sends `_NET_CLOSE_WINDOW`. Minimize sends `WM_CHANGE_STATE` with `IconicState`; restoring sends `_NET_ACTIVE_WINDOW`. Follow the existing `linux/window.rs` and `linux/ewmh.rs`. Linux app quit and hide answer `Unavailable`.

## Lua

`bricolo.window.close(id)`, `bricolo.window.set_minimized(id, minimized)`, `bricolo.app.quit(app)`, `bricolo.app.set_hidden(app, hidden)`. Each returns `true`, or `nil, err`.

## Tests

### Lua: `crates/lua/tests/scripts.rs`

Change `FakeWindows` to record what it was asked: `struct FakeWindows(Mutex<WindowInfo>, Mutex<Vec<String>>);`, with `Mutex::default()` added in its `Default`. Add to its `WindowProvider` impl:

```rust
    async fn close(&self, _: &Caller, id: u64) -> Result<(), CallError> {
        self.focus(&Caller::default(), id).await?;
        self.1.lock().unwrap().push(format!("close {id}"));
        Ok(())
    }
    async fn set_minimized(&self, _: &Caller, id: u64, minimized: bool) -> Result<(), CallError> {
        self.focus(&Caller::default(), id).await?;
        self.1.lock().unwrap().push(format!("minimized {id} {minimized}"));
        Ok(())
    }
```

Add to the `AppProvider` impl of `FakeApps`:

```rust
    async fn quit(&self, _: &Caller, app: String) -> Result<(), CallError> {
        self.0.lock().unwrap().push(format!("quit {app}"));
        Ok(())
    }
    async fn set_hidden(&self, _: &Caller, app: String, hidden: bool) -> Result<(), CallError> {
        self.0.lock().unwrap().push(format!("hidden {app} {hidden}"));
        Ok(())
    }
```

The test:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn window_close_minimize_and_app_quit_hide() {
    let mut node = Node::new();
    node.write(
        "tidy.lua",
        r#"
assert(bricolo.window.set_minimized(7, true))
assert(bricolo.window.set_minimized(7, false))
assert(bricolo.window.close(7))
local _, missing = bricolo.window.close(8)
assert(bricolo.app.set_hidden("Notes", true))
assert(bricolo.app.quit("com.apple.Notes"))
local bad = pcall(bricolo.window.close, "seven")
bricolo.log.info("done", missing, tostring(bad))
"#,
    );
    node.start(false).await;
    node.expect_line("tidy", "done failed: no such window false").await;
    assert_eq!(
        *node.windows.1.lock().unwrap(),
        ["minimized 7 true", "minimized 7 false", "close 7"]
    );
    assert_eq!(
        *node.apps.0.lock().unwrap(),
        ["hidden Notes true", "quit com.apple.Notes"]
    );
}
```

### Runtime: `crates/runtime/tests/plugins.rs`

Add these `caps` commands: `close:<id>` logs `close: {:?}` of `window::close(id)`, `minimize:<id>` logs `minimize: {:?}` of `window::set_minimized(id, true)`, `quit:<app>` logs `quit: {:?}` of `app::quit(app)`, and `hide-app:<app>` logs `hide-app: {:?}` of `app::set_hidden(app, true)`. The command `hide:` is already taken, so use `hide-app:`.

```rust
/// The new window and app calls go through the host. None of these targets
/// exist, so every call comes back as an error, whatever the permissions.
#[tokio::test(flavor = "multi_thread")]
async fn window_and_app_controls_reach_the_host() {
    let node = Node::new();
    node.install_caps(&caps_manifest(&["/bin/echo"], &["127.0.0.1:1".into()]));
    let rt = node.start(false).await;
    let fire = |cmd: &str| rt.fire("caps", ProviderEvent::Hotkey(cmd.into())).unwrap();
    fire("close:999999");
    fire("minimize:999999");
    fire("quit:bricolo-test-no-such-app");
    fire("hide-app:bricolo-test-no-such-app");
    for prefix in ["close:", "minimize:", "quit:", "hide-app:"] {
        let got = line(&rt, "caps", prefix).await;
        assert!(got.contains("Err(CallError::"), "{got}");
    }
    rt.shutdown().await;
}
```

## Done when

The README's "Done when" commands pass, and `docs/apps.md` and `docs/scripts.md` list the four new calls.
