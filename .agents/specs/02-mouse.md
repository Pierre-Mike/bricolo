# 02: mouse

Read `.agents/specs/README.md` first. This track adds the capability `mouse`.

## Contract: `wit/mouse.wit`

```wit
/// The mouse pointer of this device. Points use the same coordinates as
/// `window`. Moving and clicking need the Accessibility permission on
/// macOS; without it they answer `denied`. Always runs on this node.
interface mouse {
  use types.{call-error};

  record point {
    x: s32,
    y: s32,
  }

  enum button {
    left,
    right,
    middle,
  }

  /// Where the pointer is.
  position: async func() -> result<point, call-error>;
  /// Moves the pointer without clicking.
  move-to: async func(to: point) -> result<_, call-error>;
  /// Clicks `button` at the pointer, `count` times in a row: 2 is a double
  /// click. `count` is 1 to 3; anything else answers `failed`.
  click: async func(button: button, count: u8) -> result<_, call-error>;
  /// Scrolls by whole lines: positive `dy` scrolls down, positive `dx`
  /// scrolls right.
  scroll: async func(dx: s32, dy: s32) -> result<_, call-error>;
}
```

## Rust: `crates/providers/src/mouse.rs`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point { pub x: i32, pub y: i32 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton { Left, Right, Middle }

impl MouseButton {
    /// "left", "right" or "middle", in any case.
    pub fn parse(name: &str) -> Option<Self>;
}

/// `Failed("click count must be 1 to 3")` unless `count` is 1 to 3.
pub fn check_click_count(count: u8) -> Result<(), CallError>;

#[async_trait]
pub trait MouseProvider: Send + Sync {
    async fn position(&self, caller: &Caller) -> Result<Point, CallError>;
    async fn move_to(&self, caller: &Caller, to: Point) -> Result<(), CallError>;
    async fn click(&self, caller: &Caller, button: MouseButton, count: u8) -> Result<(), CallError>;
    async fn scroll(&self, caller: &Caller, dx: i32, dy: i32) -> Result<(), CallError>;
}

/// No mouse on this node.
pub struct NoMouse;
```

The host binding and the Lua binding both call `check_click_count` before `click`, so a bad count never reaches a provider.

## Backends

- macOS: CGEvent, as `macos/keyboard.rs` posts key events. Position comes from `CGEventCreate(NULL)` plus `CGEventGetLocation`. A click posts down and up events with `kCGMouseEventClickState` set to 1, 2 and 3 in turn. Scroll posts a line-unit scroll wheel event; positive `dy` scrolls down, so negate it for CGEvent. It needs a `Desktop`, per README rule 5.
- Linux X11: XTEST (`xtest_fake_input`), as `linux/keyboard.rs` does: motion, buttons 1, 2 and 3, and scrolling with buttons 4 and 5 (6 and 7 for `dx`). Position comes from `QueryPointer`. Wayland answers `Unavailable`.

## Lua

`bricolo.mouse.position()` returns `{x = .., y = ..}`. `bricolo.mouse.move_to(x, y)`. `bricolo.mouse.click([button[, count]])` defaults to `"left"` and `1`, and an unknown button name raises a Lua error. `bricolo.mouse.scroll(dx, dy)`.

## Tests

### Unit tests: in `crates/providers/src/mouse.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_counts_are_one_to_three() {
        assert!(check_click_count(1).is_ok());
        assert!(check_click_count(3).is_ok());
        for bad in [0, 4, 255] {
            assert_eq!(
                check_click_count(bad),
                Err(CallError::Failed("click count must be 1 to 3".into()))
            );
        }
    }

    #[test]
    fn buttons_parse_by_name() {
        assert_eq!(MouseButton::parse("left"), Some(MouseButton::Left));
        assert_eq!(MouseButton::parse("Right"), Some(MouseButton::Right));
        assert_eq!(MouseButton::parse("MIDDLE"), Some(MouseButton::Middle));
        assert_eq!(MouseButton::parse("thumb"), None);
    }
}
```

### Lua: `crates/lua/tests/scripts.rs`

Add a fake and a `mouse: Arc<FakeMouse>` field on `Node`, wired into `Providers`:

```rust
/// Records calls; the pointer starts at (5, 6).
struct FakeMouse(Mutex<Vec<String>>, Mutex<Point>);

impl Default for FakeMouse {
    fn default() -> Self {
        Self(Mutex::default(), Mutex::new(Point { x: 5, y: 6 }))
    }
}

#[async_trait]
impl MouseProvider for FakeMouse {
    async fn position(&self, _: &Caller) -> Result<Point, CallError> {
        Ok(*self.1.lock().unwrap())
    }
    async fn move_to(&self, _: &Caller, to: Point) -> Result<(), CallError> {
        *self.1.lock().unwrap() = to;
        self.0.lock().unwrap().push(format!("move {} {}", to.x, to.y));
        Ok(())
    }
    async fn click(&self, _: &Caller, button: MouseButton, count: u8) -> Result<(), CallError> {
        self.0.lock().unwrap().push(format!("click {button:?} {count}"));
        Ok(())
    }
    async fn scroll(&self, _: &Caller, dx: i32, dy: i32) -> Result<(), CallError> {
        self.0.lock().unwrap().push(format!("scroll {dx} {dy}"));
        Ok(())
    }
}
```

```rust
#[tokio::test(flavor = "multi_thread")]
async fn mouse_moves_clicks_and_scrolls() {
    let mut node = Node::new();
    node.write(
        "point.lua",
        r#"
local p = assert(bricolo.mouse.position())
assert(bricolo.mouse.move_to(100, 200))
local q = assert(bricolo.mouse.position())
assert(bricolo.mouse.click())
assert(bricolo.mouse.click("right", 2))
assert(bricolo.mouse.scroll(0, -3))
local _, err = bricolo.mouse.click("left", 4)
local bad = pcall(bricolo.mouse.click, "thumb")
bricolo.log.info("done", p.x, p.y, q.x, q.y, err, tostring(bad))
"#,
    );
    node.start(false).await;
    node.expect_line(
        "point",
        "done 5 6 100 200 failed: click count must be 1 to 3 false",
    )
    .await;
    assert_eq!(
        *node.mouse.0.lock().unwrap(),
        ["move 100 200", "click Left 1", "click Right 2", "scroll 0 -3"]
    );
}
```

### Runtime: `crates/runtime/tests/plugins.rs`

Add `caps` commands: `mouse-pos` logs `mouse-pos: {:?}` of `mouse::position()`, and `click:<n>` logs `click: {:?}` of `mouse::click(mouse::Button::Left, n)`.

```rust
/// `mouse` goes through the host, and a bad click count is refused before
/// any provider runs.
#[tokio::test(flavor = "multi_thread")]
async fn mouse_reaches_the_host_and_checks_the_count() {
    let node = Node::new();
    node.install_caps(&caps_manifest(&["/bin/echo"], &["127.0.0.1:1".into()]));
    let rt = node.start(false).await;
    let fire = |cmd: &str| rt.fire("caps", ProviderEvent::Hotkey(cmd.into())).unwrap();
    fire("mouse-pos");
    let pos = line(&rt, "caps", "mouse-pos:").await;
    assert!(pos.contains("Err(CallError::"), "{pos}");
    fire("click:4");
    assert_eq!(
        line(&rt, "caps", "click:").await,
        r#"click: Err(CallError::Failed("click count must be 1 to 3"))"#
    );
    rt.shutdown().await;
}
```

## Done when

The README's "Done when" commands pass.
