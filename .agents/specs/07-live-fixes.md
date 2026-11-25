# 07: fixes from the first test on a real Mac

Three problems showed up when the new capabilities ran on a real Mac. The
fixed test below covers the one a test can reach. The two macOS fixes can't
run in tests (no `Desktop`), so their acceptance is a live probe that the
reviewer runs after `just install-macos`.

## 1. `bricolo.mouse.move_to` and `bricolo.mouse.scroll` take any number

Today `bricolo.mouse.move_to(x + w / 2, y)` raises "error converting Lua number to
i32" when the value has a fraction. Scripts compute coordinates from frames,
so the Lua binding takes Lua numbers (`f64`) and rounds each to the nearest
integer (`f64::round`, half away from zero) before calling the provider. The
same for both arguments of `scroll`. A value outside the `i32` range, or NaN,
answers `nil, "failed: coordinate out of range"`. The WIT and the plugin
side do not change.

Add this test to `crates/lua/tests/scripts.rs`, next to
`mouse_moves_clicks_and_scrolls`, exactly as written:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn mouse_rounds_fractional_numbers() {
    let mut node = Node::new();
    node.write(
        "round.lua",
        r#"
assert(bricolo.mouse.move_to(100.6, 200.4))
assert(bricolo.mouse.scroll(0.4, -2.6))
local _, err = bricolo.mouse.move_to(1e12, 0)
bricolo.log.info("done", err)
"#,
    );
    node.start(false).await;
    node.expect_line("round", "done failed: coordinate out of range")
        .await;
    assert_eq!(
        *node.mouse.0.lock().unwrap(),
        ["move 101 200", "scroll 0 -3"]
    );
}
```

Document the rounding in the `bricolo.mouse.move_to` and `bricolo.mouse.scroll` rows of
docs/scripts.md, then run `just skill-refs`.

## 2. `app.set_hidden` uses Accessibility (macOS)

`NSRunningApplication.hide()` and `unhide()` return `NO` for every app when
the node calls them, so `bricolo.app.set_hidden` always answers "the app did not
accept the request". In `crates/providers/src/macos/app.rs`, keep `find` to
pick the app, then set the `AXHidden` attribute on
`AXUIElement::new_application(pid)` to a `CFBoolean`, as `window.rs` does for
`AXMinimized`. Map the result like `window.rs`'s `check`: success is `Ok(())`,
no Accessibility grant is `CallError::Denied`, anything else is
`Failed("the app did not accept the request")`. Reuse the AX helpers in
`macos/window.rs` (make them `pub(super)` if needed) rather than copying
them. `quit` stays on `terminate()`, which works.

## 3. A restored window comes back focused (macOS)

After `set_minimized(id, false)`, the window is back but not focused, so the
next `bricolo.window.focused()` answers `nil`. In `crates/providers/src/macos/window.rs`:

- `set_minimized(id, false)` sets `AXMinimized` to false, then focuses the
  window the same way `focus(id)` does (raise it and bring its app to the
  front). `set_minimized(id, true)` is unchanged.
- `focused()` falls back when the system-wide element has no answer: if
  `AXFocusedApplication` is missing, use the frontmost app's pid
  (`NSWorkspace` frontmost application) with `AXUIElement::new_application`;
  if `AXFocusedWindow` is missing, use `AXMainWindow`.

## Done

- `just lint`, `just test`, `just check-linux` pass. Report every `just test`
  run's summary line; the mic/audio/sensor tests that start fake ffmpeg
  scripts and `hello_runs_init_gets_a_timer_and_keeps_kv` are known to be
  flaky under load.
- Do not commit: this checkout holds the owner's uncommitted work.

Live acceptance (the reviewer runs this after `just install-macos`): a probe
script hides and shows TextEdit by bundle id and by name, each answering
`true` and visibly hiding; minimizing then restoring the focused window
leaves `bricolo.window.focused()` answering that window.
