# 06: events: app events, `system` and `files`

Read `.agents/specs/README.md` first. This track gives apps a way to react when something happens instead of only on a hotkey or a timer. It adds three event kinds, one function on `app`, and two capabilities: `system` and `files`.

## Contract

### `wit/types.wit`

Add before `variant event`:

```wit
  /// What happened to an app (`app.watch`).
  enum app-change {
    launched,
    terminated,
    /// It came to the front.
    activated,
  }

  record app-event {
    change: app-change,
    name: string,
    bundle-id: option<string>,
    pid: s32,
  }

  /// What happened to the device (`system.watch`).
  enum system-event {
    will-sleep,
    did-wake,
    screen-locked,
    screen-unlocked,
    /// A display was added or removed, or its arrangement changed.
    screens-changed,
  }

  /// What changed under a watched folder (`files.watch`).
  enum file-change {
    created,
    modified,
    removed,
  }

  record file-event {
    /// The id `files.watch` returned.
    watch: u64,
    path: string,
    change: file-change,
  }
```

Add to `variant event`, after `ui(ui-event)`:

```wit
    /// An app launched, quit or came to the front, after `app.watch(true)`.
    app(app-event),
    /// The device slept, woke, locked or unlocked, or its screens changed,
    /// after `system.watch(true)`.
    system(system-event),
    /// Something changed under a folder watched with `files.watch`.
    file(file-event),
```

### `wit/app.wit`

Add after `frontmost`:

```wit
  /// Starts or stops delivering `app` events to this plugin: launches,
  /// quits, and the app that comes to the front.
  watch: async func(on: bool) -> result<_, call-error>;
```

### `wit/system.wit`

```wit
/// The state of this device. Always runs on this node.
interface system {
  use types.{call-error};

  record power {
    /// 0 to 100, or none without a battery.
    battery: option<u8>,
    /// On mains power. Always true on a desktop.
    plugged-in: bool,
  }

  /// Seconds since the last key press, click or pointer move.
  idle-seconds: async func() -> result<u64, call-error>;
  power: async func() -> result<power, call-error>;
  /// Keeps the device from idle sleep while `on`, until `keep-awake(false)`
  /// or the plugin stops.
  keep-awake: async func(on: bool) -> result<_, call-error>;
  /// Starts or stops delivering `system` events to this plugin.
  watch: async func(on: bool) -> result<_, call-error>;
}
```

### `wit/files.wit`

```wit
/// Watches folders for changes. A plugin may watch only paths under its
/// `disk:` grants. Always runs on this node.
interface files {
  use types.{call-error};

  /// Watches `path` and everything under it, and returns an id. Each
  /// change arrives as a `file` event carrying that id. A missing path
  /// answers `failed`.
  watch: async func(path: string) -> result<u64, call-error>;
  /// Stops a watch. Stopping one that is already stopped does nothing.
  unwatch: async func(id: u64) -> result<_, call-error>;
}
```

## Rust

### `crates/providers/src/event.rs`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppChange { Launched, Terminated, Activated }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppEvent { pub change: AppChange, pub app: crate::app::AppInfo }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemEvent { WillSleep, DidWake, ScreenLocked, ScreenUnlocked, ScreensChanged }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileChange { Created, Modified, Removed }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEvent { pub watch: u64, pub path: String, pub change: FileChange }
```

`ProviderEvent` gains `App(AppEvent)`, `System(SystemEvent)` and `File(FileEvent)`. Export the new types from `lib.rs` next to `ProviderEvent`. The runtime converts them to the WIT `event`, and the Lua tier delivers them to the watcher functions below. `AppInfo` must derive `PartialEq, Eq` if it doesn't already.

### `AppProvider`

```rust
/// Starts or stops `app` events for the caller.
async fn watch(&self, caller: &Caller, on: bool) -> Result<(), CallError>;
/// Drops `plugin`'s watch.
fn release(&self, _plugin: &str) {}
```

`NoApps` answers `Unavailable`. Call `app.release` from `Providers::release`.

### `crates/providers/src/system.rs`

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Power { pub battery: Option<u8>, pub plugged_in: bool }

#[async_trait]
pub trait SystemProvider: Send + Sync {
    async fn idle_seconds(&self, caller: &Caller) -> Result<u64, CallError>;
    async fn power(&self, caller: &Caller) -> Result<Power, CallError>;
    async fn keep_awake(&self, caller: &Caller, on: bool) -> Result<(), CallError>;
    async fn watch(&self, caller: &Caller, on: bool) -> Result<(), CallError>;
    /// Ends `plugin`'s keep-awake and its watch.
    fn release(&self, plugin: &str);
}

/// No system state on this node.
pub struct NoSystem;

/// Parses `pmset -g batt`.
pub fn parse_pmset(text: &str) -> Power;
/// Parses `ioreg -c IOHIDSystem`: the first `"HIDIdleTime" = <ns>`, in
/// whole seconds.
pub fn parse_hid_idle(text: &str) -> Option<u64>;
```

### `crates/providers/src/files.rs`

```rust
#[async_trait]
pub trait FilesProvider: Send + Sync {
    async fn watch(&self, caller: &Caller, path: String) -> Result<u64, CallError>;
    async fn unwatch(&self, caller: &Caller, id: u64) -> Result<(), CallError>;
    /// Stops all of `plugin`'s watches.
    fn release(&self, plugin: &str);
}

/// Watches with the `notify` crate (FSEvents on macOS, inotify on Linux),
/// and delivers `ProviderEvent::File` through `sink` to the plugin that
/// asked. Checks the path with `disk::authorize(path, &caller.disk, home,
/// false)` first.
pub struct NotifyFiles { /* ... */ }
impl NotifyFiles {
    pub fn new(sink: EventSink) -> Self;
}
```

Map notify's event kinds: `Create` to `Created`, `Remove` to `Removed`, and everything else to `Modified`. For a rename, report the old path as `Removed` and the new path as `Created`, when notify gives both. Report one event per path. Ids count up from 1 and are never reused. `files` works on every OS, in tests too. `Providers::local` always builds `NotifyFiles`.

## Backends

- macOS `app.watch`: `NSWorkspace`'s notification center (`didLaunchApplication`, `didTerminateApplication`, `didActivateApplication`), observed on the main-thread loop. `macos/main_loop.rs` and `macos/hotkey.rs` show how work gets onto that loop. Each notification goes to every plugin that is watching. Without a `Desktop`, `watch` answers `Unavailable`.
- macOS `system`: `MacSystem`, built only with a `Desktop` (README rule 5); without one, `NoSystem`.
  - `idle_seconds` runs `/usr/sbin/ioreg -c IOHIDSystem` and uses `parse_hid_idle`.
  - `power` runs `/usr/bin/pmset -g batt` and uses `parse_pmset`.
  - `keep_awake(true)` starts `/usr/bin/caffeinate -i` for that plugin, once; `false` or `release` kills it.
  - `watch` uses `NSWorkspace` `willSleep`/`didWake`, the distributed notifications `com.apple.screenIsLocked` and `com.apple.screenIsUnlocked`, and `NSApplicationDidChangeScreenParametersNotification` for `screens-changed`.
- Linux: `app.watch` and all of `system` answer `Unavailable`. `files` works.

## Lua

- `bricolo.app.watch(fn)` starts app events and calls `fn(event)`, where event is `{change = "launched" | "terminated" | "activated", name = .., bundle_id = .. or nil, pid = ..}`. `bricolo.app.watch(nil)` stops them.
- `bricolo.system.watch(fn)` calls `fn(name)`, where name is one of `"will_sleep"`, `"did_wake"`, `"screen_locked"`, `"screen_unlocked"` or `"screens_changed"`. `bricolo.system.watch(nil)` stops it.
- `bricolo.system.idle_seconds()` returns a number. `bricolo.system.power()` returns `{battery = 87 or nil, plugged_in = true}`. `bricolo.system.keep_awake(on)` returns `true`, or `nil, err`.
- `bricolo.files.watch(path, fn)` returns an id and calls `fn(event)`, where event is `{watch = id, path = .., change = "created" | "modified" | "removed"}`. `bricolo.files.unwatch(id)` returns `true`.
- All of these call the provider, then keep `fn` until the script stops or reloads. On reload the providers' `release` drops what the old script held.

## Tests

### Unit tests: `crates/providers/src/system.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pmset_reads_the_battery_and_the_source() {
        let on_ac = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=4653155)\t87%; charged; 0:00 remaining present: true\n";
        assert_eq!(parse_pmset(on_ac), Power { battery: Some(87), plugged_in: true });
        let on_battery = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=4653155)\t54%; discharging; 3:12 remaining present: true\n";
        assert_eq!(parse_pmset(on_battery), Power { battery: Some(54), plugged_in: false });
        let desktop = "Now drawing from 'AC Power'\n";
        assert_eq!(parse_pmset(desktop), Power { battery: None, plugged_in: true });
    }

    #[test]
    fn hid_idle_time_is_in_whole_seconds() {
        let text = "+-o IOHIDSystem  <class IOHIDSystem>\n    {\n      \"HIDIdleTime\" = 12999999999\n    }\n";
        assert_eq!(parse_hid_idle(text), Some(12));
        assert_eq!(parse_hid_idle("nothing here"), None);
    }
}
```

### Unit tests: `crates/providers/src/files.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::DiskGrant;
    use std::sync::Arc;
    use std::time::Duration;

    fn granted(root: &Path) -> Caller {
        let mut caller = Caller::new("w");
        caller.disk = vec![DiskGrant { root: root.to_path_buf(), write: false }];
        caller
    }

    #[tokio::test]
    async fn watching_needs_a_grant_and_an_existing_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let files = NotifyFiles::new(Arc::new(|_: &str, _: ProviderEvent| {}));
        let path = root.to_string_lossy().into_owned();
        assert_eq!(files.watch(&Caller::new("w"), path.clone()).await, Err(CallError::Denied));
        let caller = granted(&root);
        let missing = root.join("missing").to_string_lossy().into_owned();
        assert!(matches!(files.watch(&caller, missing).await, Err(CallError::Failed(_))));
        let first = files.watch(&caller, path.clone()).await.unwrap();
        let second = files.watch(&caller, path).await.unwrap();
        assert!(second > first);
        files.unwatch(&caller, first).await.unwrap();
        files.unwatch(&caller, first).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn changes_arrive_as_events_for_the_watching_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let files = NotifyFiles::new(Arc::new(move |plugin: &str, e: ProviderEvent| {
            let _ = tx.send((plugin.to_string(), e));
        }));
        let caller = granted(&root);
        let id = files.watch(&caller, root.to_string_lossy().into_owned()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::fs::write(root.join("a.txt"), "x").unwrap();
        let got = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some((plugin, ProviderEvent::File(e))) = rx.recv().await
                    && e.path.ends_with("a.txt")
                {
                    return (plugin, e);
                }
            }
        })
        .await
        .expect("no event for a.txt");
        assert_eq!(got.0, "w");
        assert_eq!(got.1.watch, id);
        assert_ne!(got.1.change, FileChange::Removed);
        files.release("w");
    }
}
```

### Lua: `crates/lua/tests/scripts.rs`

Add to `FakeApps`'s `AppProvider` impl:

```rust
    async fn watch(&self, caller: &Caller, on: bool) -> Result<(), CallError> {
        self.0.lock().unwrap().push(format!("watch {} {on}", caller.plugin));
        Ok(())
    }
```

Add this fake, and a `system: Arc<FakeSystem>` field on `Node`, wired into `Providers`. Wire the real `files: Arc::new(NotifyFiles::new(mailbox.sink()))` too.

```rust
/// Idle 42 s, 87% on mains; records keep-awake, watch and release.
#[derive(Default)]
struct FakeSystem(Mutex<Vec<String>>);

#[async_trait]
impl SystemProvider for FakeSystem {
    async fn idle_seconds(&self, _: &Caller) -> Result<u64, CallError> {
        Ok(42)
    }
    async fn power(&self, _: &Caller) -> Result<Power, CallError> {
        Ok(Power { battery: Some(87), plugged_in: true })
    }
    async fn keep_awake(&self, caller: &Caller, on: bool) -> Result<(), CallError> {
        self.0.lock().unwrap().push(format!("awake {} {on}", caller.plugin));
        Ok(())
    }
    async fn watch(&self, caller: &Caller, on: bool) -> Result<(), CallError> {
        self.0.lock().unwrap().push(format!("watch {} {on}", caller.plugin));
        Ok(())
    }
    fn release(&self, plugin: &str) {
        self.0.lock().unwrap().push(format!("release {plugin}"));
    }
}
```

```rust
#[tokio::test(flavor = "multi_thread")]
async fn app_and_system_events_reach_watchers() {
    let mut node = Node::new();
    node.write(
        "watcher.lua",
        r#"
assert(bricolo.app.watch(function(e)
  bricolo.log.info("app", e.change, e.name, e.bundle_id, e.pid)
end))
assert(bricolo.system.watch(function(e) bricolo.log.info("system", e) end))
bricolo.log.info("ready")
"#,
    );
    node.start(false).await;
    node.expect_line("watcher", "ready").await;
    assert!(node.apps.0.lock().unwrap().contains(&"watch watcher.lua true".to_string()));
    assert!(node.system.0.lock().unwrap().contains(&"watch watcher.lua true".to_string()));
    let sink = node.mailbox.sink();
    sink(
        "watcher.lua",
        ProviderEvent::App(AppEvent {
            change: AppChange::Activated,
            app: AppInfo {
                name: "Zoom".into(),
                bundle_id: Some("us.zoom.xos".into()),
                pid: 9,
            },
        }),
    );
    node.expect_line("watcher", "app activated Zoom us.zoom.xos 9").await;
    sink("watcher.lua", ProviderEvent::System(SystemEvent::DidWake));
    node.expect_line("watcher", "system did_wake").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn system_state_and_keep_awake_until_reload() {
    let mut node = Node::new();
    node.write(
        "awake.lua",
        r#"
local p = assert(bricolo.system.power())
assert(bricolo.system.keep_awake(true))
bricolo.log.info("state", bricolo.system.idle_seconds(), p.battery, p.plugged_in)
"#,
    );
    node.start(true).await;
    node.expect_line("awake", "state 42 87 true").await;
    node.write("awake.lua", r#"bricolo.log.info("again")"#);
    node.expect_line("awake", "again").await;
    let calls = node.system.0.lock().unwrap().clone();
    assert!(calls.contains(&"awake awake.lua true".to_string()), "{calls:?}");
    assert!(calls.contains(&"release awake.lua".to_string()), "{calls:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn files_watch_reports_created_and_removed() {
    let mut node = Node::new();
    let inbox = node.root.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    node.write(
        "inbox.lua",
        &format!(
            r#"
local id
id = assert(bricolo.files.watch("{}", function(e)
  if e.path:match("note%.txt$") then
    bricolo.log.info("file", e.change, e.watch == id)
  end
end))
bricolo.log.info("watching")
"#,
            inbox.display()
        ),
    );
    node.start(false).await;
    node.expect_line("inbox", "watching").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(inbox.join("note.txt"), "hi").unwrap();
    let seen = node
        .wait_line("inbox", "file created true", Duration::from_secs(10))
        .await
        || node
            .wait_line("inbox", "file modified true", Duration::from_secs(1))
            .await;
    assert!(seen, "{:?}", node.lines("inbox"));
    std::fs::remove_file(inbox.join("note.txt")).unwrap();
    assert!(
        node.wait_line("inbox", "file removed true", Duration::from_secs(10))
            .await,
        "{:?}",
        node.lines("inbox")
    );
}
```

### Runtime: `crates/runtime/tests/plugins.rs`

In `caps`'s `on_event`, before the `_ => return` arm, add:

```rust
            Event::App(e) => return say(format!("app-event: {:?} {}", e.change, e.name)),
            Event::System(e) => return say(format!("system-event: {e:?}")),
            Event::File(e) => {
                return say(format!("file-event: {} {} {:?}", e.watch, e.path, e.change));
            }
```

Add the `caps` commands `app-watch` (logs `app-watch: {:?}` of `app::watch(true)`), `idle` (logs `idle: {:?}` of `system::idle_seconds()`) and `files-watch:<path>` (logs `files-watch: {:?}` of `files::watch(path)`).

```rust
/// The three new events reach a plugin as WIT events.
#[tokio::test(flavor = "multi_thread")]
async fn app_system_and_file_events_reach_a_plugin() {
    use bricolo_providers::app::AppInfo;
    use bricolo_providers::{AppChange, AppEvent, FileChange, FileEvent, SystemEvent};
    let node = Node::new();
    node.install_caps(&caps_manifest(&["/bin/echo"], &["127.0.0.1:1".into()]));
    let rt = node.start(false).await;
    let app = AppInfo { name: "Zoom".into(), bundle_id: None, pid: 3 };
    rt.fire("caps", ProviderEvent::App(AppEvent { change: AppChange::Launched, app }))
        .unwrap();
    assert_eq!(
        line(&rt, "caps", "app-event:").await,
        "app-event: AppChange::Launched Zoom"
    );
    rt.fire("caps", ProviderEvent::System(SystemEvent::ScreenLocked)).unwrap();
    assert_eq!(
        line(&rt, "caps", "system-event:").await,
        "system-event: SystemEvent::ScreenLocked"
    );
    let file = FileEvent { watch: 4, path: "/x/a.txt".into(), change: FileChange::Removed };
    rt.fire("caps", ProviderEvent::File(file)).unwrap();
    assert_eq!(
        line(&rt, "caps", "file-event:").await,
        "file-event: 4 /x/a.txt FileChange::Removed"
    );
    rt.shutdown().await;
}

/// The new calls go through the host. Tests have no main-thread loop, so
/// `app.watch` and `system` answer with an error; `caps` has no `disk:`
/// grant, so watching any folder is denied.
#[tokio::test(flavor = "multi_thread")]
async fn event_sources_reach_the_host() {
    let node = Node::new();
    node.install_caps(&caps_manifest(&["/bin/echo"], &["127.0.0.1:1".into()]));
    let rt = node.start(false).await;
    let fire = |cmd: &str| rt.fire("caps", ProviderEvent::Hotkey(cmd.into())).unwrap();
    fire("app-watch");
    fire("idle");
    fire(&format!("files-watch:{}", node.root.display()));
    for prefix in ["app-watch:", "idle:"] {
        let got = line(&rt, "caps", prefix).await;
        assert!(got.contains("Err(CallError::"), "{got}");
    }
    assert_eq!(
        line(&rt, "caps", "files-watch:").await,
        "files-watch: Err(CallError::Denied)"
    );
    rt.shutdown().await;
}
```

## Done when

The README's "Done when" commands pass, including `just deny` for the `notify` crate. Document `bricolo.app.watch`, `bricolo.system` and `bricolo.files` in `docs/scripts.md`, and the event kinds in `docs/apps.md`.
