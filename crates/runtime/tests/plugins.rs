//! Loads real components through the real runtime: `plugins/hello`,
//! `plugins/window-tiler`, and the `fixtures/probe` and `fixtures/caps`
//! test plugins, built for wasm32-wasip2 (`just plugins`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bricolo_runtime::engine::WasmEngine;
use bricolo_runtime::permissions::{Grant, Permissions};
use bricolo_runtime::{Options, Paths, ProviderEvent, Runtime, State, install};
use tempfile::TempDir;

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The built component, building it first if it is missing.
fn component(name: &str) -> PathBuf {
    let root = workspace();
    let path = root.join(format!("target/wasm32-wasip2/release/{name}.wasm"));
    if !path.is_file() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args([
                "build",
                "-p",
                name,
                "--target",
                "wasm32-wasip2",
                "--release",
            ])
            .current_dir(&root)
            .status()
            .expect("cannot run cargo");
        assert!(
            status.success(),
            "building {name} failed; run `just plugins`"
        );
    }
    path
}

struct Node {
    _dir: TempDir,
    paths: Paths,
    root: PathBuf,
}

impl Node {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let paths = Paths::new(root.join("config"), root.join("data"));
        Self {
            _dir: dir,
            paths,
            root,
        }
    }

    /// Installs the plugin folder `src`, approving everything it asks for.
    fn install(&self, src: &Path) {
        let engine = WasmEngine::new().unwrap();
        let plan = install::plan(&engine, src).unwrap();
        install::apply(&self.paths, &plan).unwrap();
    }

    fn install_hello(&self) {
        component("hello");
        self.install(&workspace().join("plugins/hello"));
    }

    /// Writes a source folder for the probe with `manifest`, then installs it.
    fn install_probe(&self, manifest: &str) {
        let src = self.root.join("probe-src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::copy(component("probe"), src.join("plugin.wasm")).unwrap();
        std::fs::write(src.join("plugin.toml"), manifest).unwrap();
        self.install(&src);
    }

    /// Writes a source folder for `caps` with `manifest`, then installs it.
    fn install_caps(&self, manifest: &str) {
        let src = self.root.join("caps-src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::copy(component("caps"), src.join("plugin.wasm")).unwrap();
        std::fs::write(src.join("plugin.toml"), manifest).unwrap();
        self.install(&src);
    }

    async fn start(&self, watch: bool) -> Arc<Runtime> {
        Runtime::start(
            self.paths.clone(),
            Options {
                watch,
                ..Options::default()
            },
        )
        .await
        .unwrap()
    }
}

/// Waits until `check` holds, or fails after 30 s. On macOS, `window.list`
/// asks every running app over Accessibility, which took 13 s under a
/// loaded `just ci`.
async fn until(what: &str, mut check: impl AsyncFnMut() -> bool) {
    let start = Instant::now();
    while !check().await {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn logged(rt: &Runtime, plugin: &str, needle: &str) -> bool {
    rt.recent_logs(Some(plugin), 100)
        .iter()
        .any(|l| l.message.contains(needle))
}

fn count_logged(rt: &Runtime, plugin: &str, needle: &str) -> usize {
    rt.recent_logs(Some(plugin), 100)
        .iter()
        .filter(|l| l.message.contains(needle))
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn hello_runs_init_gets_a_timer_and_keeps_kv() {
    let node = Node::new();
    node.install_hello();
    std::fs::create_dir_all(&node.paths.config).unwrap();
    std::fs::write(
        node.paths.node_toml(),
        "[plugins.hello]\ngreeting = \"hi\"\n",
    )
    .unwrap();

    let rt = node.start(false).await;
    let status = rt.plugin_status("hello").await.unwrap();
    assert_eq!(status.state, State::Running, "{status:?}");
    // Settings: the default tick_ms, the overridden greeting.
    assert!(
        logged(&rt, "hello", "init: start 1, config "),
        "{:?}",
        rt.recent_logs(None, 10)
    );
    assert!(logged(&rt, "hello", r#""greeting":"hi""#));
    assert!(logged(&rt, "hello", r#""tick_ms":50"#));
    // The plugin logs the tick, then writes it to kv.
    let kv_file = node.paths.plugin_data("hello").join("kv.json");
    let kv = || std::fs::read_to_string(&kv_file).unwrap_or_default();
    until("a timer tick in kv", async || kv().contains("last-tick")).await;
    assert!(logged(&rt, "hello", "tick "));
    let status = rt.plugin_status("hello").await.unwrap();
    assert_eq!(status.restarts, 0, "{status:?}");
    assert!(kv().contains("\"starts\": \"1\""), "{}", kv());
    rt.shutdown().await;
    assert!(logged(&rt, "hello", "dispose"));
    assert_eq!(
        rt.plugin_status("hello").await.unwrap().state,
        State::Stopped
    );

    // kv survives a node restart.
    let rt = node.start(false).await;
    assert!(logged(&rt, "hello", "init: start 2"));
    rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ungranted_capability_is_refused() {
    let node = Node::new();
    node.install_hello();
    let mut perms = Permissions::default();
    perms.plugins.insert(
        "hello".into(),
        Grant {
            granted: vec!["log".into()],
        },
    );
    perms.save(&node.paths.permissions_toml()).unwrap();

    let rt = node.start(false).await;
    let status = rt.plugin_status("hello").await.unwrap();
    assert_eq!(status.state, State::Refused);
    let error = status.error.unwrap();
    assert!(error.contains("not granted: kv, timer"), "{error}");
    assert!(rt.recent_logs(Some("hello"), 10).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn import_missing_from_the_manifest_fails_the_check() {
    let node = Node::new();
    let src = node.root.join("hello-src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::copy(component("hello"), src.join("plugin.wasm")).unwrap();
    std::fs::write(
        src.join("plugin.toml"),
        "name = \"hello\"\nversion = \"0.1.0\"\nimports = [\"log\"]\n",
    )
    .unwrap();
    let engine = WasmEngine::new().unwrap();
    let err = install::plan(&engine, &src).unwrap_err().to_string();
    assert!(
        err.contains("imports kv, which plugin.toml does not list"),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn hash_mismatch_is_refused() {
    let node = Node::new();
    node.install_hello();
    let wasm = node.paths.plugin_dir("hello").join("plugin.wasm");
    let mut bytes = std::fs::read(&wasm).unwrap();
    bytes.push(0);
    std::fs::write(&wasm, bytes).unwrap();

    let rt = node.start(false).await;
    let status = rt.plugin_status("hello").await.unwrap();
    assert_eq!(status.state, State::Refused);
    assert!(status.error.unwrap().contains("plugins.lock expects"));
}

#[tokio::test(flavor = "multi_thread")]
async fn disk_outside_the_grant_is_denied() {
    let node = Node::new();
    let allowed = node.root.join("allowed");
    std::fs::create_dir_all(&allowed).unwrap();
    std::fs::write(allowed.join("a.txt"), "hi").unwrap();
    std::fs::write(node.root.join("secret.txt"), "no").unwrap();
    node.install_probe(&format!(
        "name = \"probe\"\nversion = \"0.1.0\"\nimports = [\"log\", \"disk\"]\npermissions = ['disk:read:{}']\n",
        allowed.display()
    ));

    let rt = node.start(false).await;
    let inside = allowed.join("a.txt");
    let outside = node.root.join("secret.txt");
    rt.fire(
        "probe",
        ProviderEvent::Hotkey(format!("read:{}", inside.display())),
    )
    .unwrap();
    rt.fire(
        "probe",
        ProviderEvent::Hotkey(format!("read:{}", outside.display())),
    )
    .unwrap();
    until("both reads", async || {
        count_logged(&rt, "probe", "read ") == 2
    })
    .await;
    let lines = rt.recent_logs(Some("probe"), 10);
    let has = |s: &str| lines.iter().any(|l| l.message.contains(s));
    assert!(
        has(&format!("read {}: 2 bytes", inside.display())),
        "{lines:?}"
    );
    assert!(
        has(&format!("read {}: CallError::Denied", outside.display())),
        "{lines:?}"
    );
    rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn timeout_restarts_once_then_a_trap_turns_it_off() {
    let node = Node::new();
    node.install_probe(
        "name = \"probe\"\nversion = \"0.1.0\"\nimports = [\"log\", \"disk\"]\npermissions = [\"disk:read:/tmp\"]\n",
    );
    let rt = node.start(false).await;
    assert_eq!(count_logged(&rt, "probe", "init"), 1);

    // A spinning handler runs past its 500 ms budget: restart once.
    rt.fire("probe", ProviderEvent::Hotkey("spin".into()))
        .unwrap();
    until("the restart", async || {
        count_logged(&rt, "probe", "init") == 2
    })
    .await;
    let status = rt.plugin_status("probe").await.unwrap();
    assert_eq!(status.state, State::Running);
    assert_eq!(status.restarts, 1);
    assert!(status.error.unwrap().contains("ran past its 500 ms budget"));

    // A second failure, a trap this time: off for good.
    rt.fire("probe", ProviderEvent::Hotkey("trap".into()))
        .unwrap();
    until("off", async || {
        rt.plugin_status("probe").await.unwrap().state == State::Off
    })
    .await;
    let status = rt.plugin_status("probe").await.unwrap();
    assert!(status.error.unwrap().contains("trapped in on-event"));
    assert_eq!(count_logged(&rt, "probe", "init"), 2);
    assert!(rt.fire("probe", ProviderEvent::Hotkey("x".into())).is_err());
    rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_plugin_reloads_and_a_bad_update_keeps_the_old_one() {
    let node = Node::new();
    node.install_hello();
    let rt = node.start(true).await;
    assert!(logged(&rt, "hello", "init: start 1"));

    // Reinstalling rewrites plugin.toml and plugin.wasm: one reload.
    node.install_hello();
    until("the reload", async || logged(&rt, "hello", "init: start 2")).await;
    assert!(
        logged(&rt, "hello", "dispose"),
        "{:?}",
        rt.plugin_status("hello").await
    );
    assert_eq!(
        rt.plugin_status("hello").await.unwrap().state,
        State::Running
    );

    // A wasm that does not match the lock: the old version keeps running.
    let wasm = node.paths.plugin_dir("hello").join("plugin.wasm");
    let mut bytes = std::fs::read(&wasm).unwrap();
    bytes.push(0);
    std::fs::write(&wasm, bytes).unwrap();
    until("the refused reload", async || {
        rt.plugin_status("hello")
            .await
            .unwrap()
            .error
            .is_some_and(|e| e.contains("reload refused"))
    })
    .await;
    let status = rt.plugin_status("hello").await.unwrap();
    assert_eq!(status.state, State::Running);
    assert_eq!(count_logged(&rt, "hello", "init: start"), 2);
    rt.fire("hello", ProviderEvent::Timer(99)).unwrap();
    until("the old version to answer", async || {
        logged(&rt, "hello", "tick 99")
    })
    .await;

    // Uninstalling removes it from the running node.
    install::uninstall(&node.paths, "hello").unwrap();
    until("removal", async || {
        rt.plugin_status("hello").await.is_none()
    })
    .await;
    rt.shutdown().await;
}

/// The first line `plugin` logged that starts with `prefix`, waiting for it.
async fn line(rt: &Runtime, plugin: &str, prefix: &str) -> String {
    until(prefix, async || {
        rt.recent_logs(Some(plugin), 100)
            .iter()
            .any(|l| l.message.starts_with(prefix))
    })
    .await;
    rt.recent_logs(Some(plugin), 100)
        .into_iter()
        .find(|l| l.message.starts_with(prefix))
        .unwrap()
        .message
}

/// A manifest for `caps`, which imports every capability it can call.
/// Absolute program paths for the caps fixture's `[shell] allow` list.
#[cfg(not(windows))]
const ECHO: &str = "/bin/echo";
#[cfg(not(windows))]
const SH: &str = "/bin/sh";
#[cfg(windows)]
const ECHO: &str = r"C:\Windows\System32\cmd.exe";
#[cfg(windows)]
const SH: &str = r"C:\Windows\System32\where.exe";

fn caps_manifest(shell: &[&str], http: &[String]) -> String {
    let quoted = |items: Vec<String>| {
        items
            .iter()
            .map(|i| format!("'{i}'"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        r#"name = "caps"
version = "0.1.0"
imports = ["log", "clipboard", "window", "app", "notify", "tts", "hotkey", "shell", "http", "ui", "mic", "speech", "audio", "system", "files", "mouse", "screen", "selection", "camera", "secrets"]
[shell]
allow = [{}]
[http]
allow = [{}]
"#,
        quoted(shell.iter().map(|s| s.to_string()).collect()),
        quoted(http.to_vec()),
    )
}

// Only the Unix tests below use it.
#[cfg(unix)]
/// Answers every request with 200 and "pong".
fn pong_server() -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\npong",
            );
        }
    });
    port
}

// Only the Unix tests below use it.
#[cfg(unix)]
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn shell_and_http_follow_the_manifest_allow_lists() {
    let node = Node::new();
    let port = pong_server();
    node.install_caps(&caps_manifest(&[ECHO], &[format!("127.0.0.1:{port}")]));
    // Approval recorded every allow-list entry.
    let perms = Permissions::load(&node.paths.permissions_toml()).unwrap();
    let granted = perms.granted("caps");
    assert!(
        granted.contains(&"shell:/bin/echo".to_string()),
        "{granted:?}"
    );
    assert!(
        granted.contains(&format!("http:127.0.0.1:{port}")),
        "{granted:?}"
    );

    let rt = node.start(false).await;
    let fire = |cmd: String| rt.fire("caps", ProviderEvent::Hotkey(cmd)).unwrap();
    fire("shell:/bin/echo".into());
    let out = line(&rt, "caps", "shell /bin/echo").await;
    assert_eq!(out, r#"shell /bin/echo: code Some(0) stdout "hi\n""#);
    fire("shell:/bin/ls".into());
    let out = line(&rt, "caps", "shell /bin/ls").await;
    assert_eq!(out, "shell /bin/ls: CallError::Denied");

    let allowed = format!("http://127.0.0.1:{port}/ping");
    fire(format!("http:{allowed}"));
    assert_eq!(
        line(&rt, "caps", &format!("http {allowed}")).await,
        format!("http {allowed}: 200 pong")
    );
    let other = format!("http://127.0.0.1:{}/ping", free_port());
    fire(format!("http:{other}"));
    assert_eq!(
        line(&rt, "caps", &format!("http {other}")).await,
        format!("http {other}: CallError::Denied")
    );
    rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_list_changes_need_a_new_approval() {
    let node = Node::new();
    let http = ["127.0.0.1:1".to_string()];
    node.install_caps(&caps_manifest(&[ECHO], &http));
    // An update that adds a program, with the old grants kept.
    let mut perms = Permissions::load(&node.paths.permissions_toml()).unwrap();
    let old = perms.plugins["caps"].clone();
    node.install_caps(&caps_manifest(&[ECHO, SH], &http));
    perms.plugins.insert("caps".into(), old);
    perms.save(&node.paths.permissions_toml()).unwrap();
    let rt = node.start(false).await;
    let status = rt.plugin_status("caps").await.unwrap();
    assert_eq!(status.state, State::Refused);
    assert!(
        status
            .error
            .as_deref()
            .unwrap()
            .contains(&format!("not granted: shell:{SH}")),
        "{status:?}"
    );
}

/// Whether the desktop capabilities answer without a main-thread loop: a
/// Linux display is set, or this is Windows (whose backends need no loop and
/// run on a desktop session, as on CI). These tests expect `unavailable`,
/// so they skip there; the `x11_` and `win_` tests cover that case.
fn linux_display() -> bool {
    let set = |v: &str| std::env::var_os(v).is_some_and(|v| !v.is_empty());
    cfg!(windows) || (cfg!(target_os = "linux") && (set("DISPLAY") || set("WAYLAND_DISPLAY")))
}

/// Whether a Linux D-Bus session bus may be running, where `secrets`
/// would reach the Secret Service.
fn linux_session_bus() -> bool {
    let set = |v: &str| std::env::var_os(v).is_some_and(|v| !v.is_empty());
    let runtime_bus = std::env::var_os("XDG_RUNTIME_DIR")
        .is_some_and(|d| std::path::Path::new(&d).join("bus").exists());
    cfg!(target_os = "linux") && (set("DBUS_SESSION_BUS_ADDRESS") || runtime_bus)
}

/// `screen` and `selection` go through the host. Tests have no main-thread
/// loop, so both answer with an error, and an empty area is refused first.
#[tokio::test(flavor = "multi_thread")]
async fn screen_and_selection_reach_the_host() {
    if linux_display() {
        eprintln!("skipped: a display is set");
        return;
    }
    let node = Node::new();
    node.install_caps(&caps_manifest(&[ECHO], &["127.0.0.1:1".into()]));
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

/// Without the main-thread loop (tests never have one), hotkeys and screens
/// are unavailable on macOS. On macOS apps answer, and windows answer or
/// are denied, depending on whether the terminal has Accessibility. On
/// Linux without a display, all three are unavailable.
#[tokio::test(flavor = "multi_thread")]
async fn desktop_capabilities_without_the_main_thread_loop() {
    if linux_display() {
        eprintln!("skipped: a display is set");
        return;
    }
    let node = Node::new();
    node.install_caps(&caps_manifest(&[ECHO], &["127.0.0.1:1".into()]));
    let rt = node.start(false).await;
    for cmd in ["apps", "windows", "screens"] {
        rt.fire("caps", ProviderEvent::Hotkey(cmd.into())).unwrap();
    }
    let apps = line(&rt, "caps", "apps:").await;
    let windows = line(&rt, "caps", "windows:").await;
    let screens = line(&rt, "caps", "screens:").await;
    assert_eq!(screens, "screens: CallError::Unavailable");
    if cfg!(target_os = "macos") {
        assert!(!apps.contains("CallError"), "{apps}");
        assert!(
            !windows.contains("CallError") || windows.ends_with("CallError::Denied"),
            "{windows}"
        );
    } else {
        assert_eq!(apps, "apps: CallError::Unavailable");
        assert_eq!(windows, "windows: CallError::Unavailable");
    }
    rt.shutdown().await;
}

/// The tiler binds its hotkeys in init. Tests have no main-thread loop, so
/// binding answers unavailable and the plugin goes off with that reason.
/// On Linux, the same happens without a display.
// Windows has a hotkey backend (`RegisterHotKey`), so the plugin may start
// there, and whether the chords bind depends on the CI session.
#[cfg(not(windows))]
#[tokio::test(flavor = "multi_thread")]
async fn window_tiler_needs_hotkeys() {
    if linux_display() {
        eprintln!("skipped: a display is set");
        return;
    }
    let node = Node::new();
    component("window-tiler");
    node.install(&workspace().join("plugins/window-tiler"));
    let rt = node.start(false).await;
    let status = rt.plugin_status("window-tiler").await.unwrap();
    assert_eq!(status.state, State::Off, "{status:?}");
    assert!(
        status
            .error
            .unwrap()
            .contains("cannot bind cmd+alt+left: CallError::Unavailable")
    );
}

/// Sets the real clipboard, reads it back and restores it. Ignored: it
/// touches the clipboard of whoever runs the tests, and needs a GUI
/// session. Run it with `cargo nextest run --run-ignored all clipboard`;
/// on Linux `just linux-test` runs it under Xvfb.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn clipboard_round_trip() {
    let node = Node::new();
    node.install_caps(&caps_manifest(&[ECHO], &["127.0.0.1:1".into()]));
    let rt = node.start(false).await;
    rt.fire("caps", ProviderEvent::Hotkey("clip-roundtrip".into()))
        .unwrap();
    assert_eq!(
        line(&rt, "caps", "clip-roundtrip").await,
        "clip-roundtrip: ok"
    );
    rt.shutdown().await;
}

/// The tiler end to end on X11: the press goes from the key grab through
/// the runtime to the plugin, which reads the focused window and the
/// screens and moves the window. Needs Xvfb at 1280x800 with openbox,
/// xterm and xdotool; `just linux-test` runs it.
#[cfg(target_os = "linux")]
#[ignore = "needs an X server; run by `just linux-test`"]
#[tokio::test(flavor = "multi_thread")]
async fn x11_window_tiler_tiles_the_focused_window() {
    use bricolo_providers::{Caller, LocalOptions, Providers};

    let node = Node::new();
    component("window-tiler");
    node.install(&workspace().join("plugins/window-tiler"));
    let rt = node.start(false).await;
    let status = rt.plugin_status("window-tiler").await.unwrap();
    assert_eq!(status.state, State::Running, "{status:?}");

    let title = format!("bricolo tiler {}", std::process::id());
    let mut xterm = Command::new("xterm")
        .args(["+sb", "-title", &title, "-geometry", "40x10+200+150"])
        .args(["-e", "sleep", "600"])
        .spawn()
        .unwrap();
    // Our own view of the desktop, to check where the window went.
    let p = Providers::local(
        LocalOptions::new(node.root.join("view")),
        Arc::new(|_, _| {}),
    );
    let c = Caller::new("test");
    let mut id = 0;
    until("the xterm to have focus", async || {
        match p.window.focused(&c).await {
            Ok(Some(w)) if w.title == title => {
                id = w.id;
                true
            }
            _ => false,
        }
    })
    .await;
    let xdotool = |keys: &str| {
        let status = Command::new("xdotool")
            .args(["key", keys])
            .status()
            .unwrap();
        assert!(status.success());
    };
    xdotool("super+alt+Left");
    until("the tiler to tile", async || {
        logged(&rt, "window-tiler", "left-half: XTerm window")
    })
    .await;
    let frame = |p: &Providers| {
        let (p, c) = (p.clone(), c.clone());
        async move {
            let list = p.window.list(&c).await.unwrap();
            list.into_iter().find(|w| w.id == id).unwrap().frame
        }
    };
    let mut left = frame(&p).await;
    until("the left half", async || {
        left = frame(&p).await;
        (left.x, left.y) == (0, 0)
    })
    .await;
    // xterm sizes in whole character cells.
    assert!(left.width.abs_diff(640) < 20, "{left:?}");
    assert!(left.height.abs_diff(800) < 30, "{left:?}");

    xdotool("super+alt+Right");
    until("the right half", async || frame(&p).await.x == 640).await;
    assert_eq!(count_logged(&rt, "window-tiler", "-half: XTerm window"), 2);
    let _ = xterm.kill();
    let _ = xterm.wait();
    rt.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_tiles_panels_and_events_reach_the_plugin() {
    use bricolo_providers::ui::{PanelKind, UiAction};
    let node = Node::new();
    node.install_caps(&caps_manifest(&[ECHO], &["127.0.0.1:1".to_string()]));
    let rt = node.start(false).await;
    let ui = rt.providers().ui.clone();
    let fire = |cmd: &str| rt.fire("caps", ProviderEvent::Hotkey(cmd.into())).unwrap();

    fire("tile:Caps");
    assert_eq!(line(&rt, "caps", "tile:").await, "tile: Ok(())");
    let tile = ui.snapshot().tiles["caps"].clone();
    assert_eq!(tile.title, "Caps");
    assert_eq!(tile.switches[0].id, "armed");
    ui.act(UiAction::Switch {
        owner: "caps".into(),
        id: "armed".into(),
        on: true,
    })
    .unwrap();
    assert_eq!(
        line(&rt, "caps", "ui: ").await,
        r#"ui: UiEvent::Switch(SwitchToggled { id: "armed", on: true })"#
    );
    assert!(ui.snapshot().tiles["caps"].switches[0].on);

    fire("widget:42");
    assert_eq!(line(&rt, "caps", "widget:").await, "widget: Ok(())");
    let widget = ui.snapshot().widgets["caps"].clone();
    assert_eq!(widget.title, "Caps");
    assert_eq!(widget.rows[0].value, "42");

    fire("review:draft text");
    let shown = line(&rt, "caps", "review:").await;
    let id = shown
        .strip_prefix("review: Ok(\"")
        .and_then(|r| r.strip_suffix("\")"))
        .unwrap_or_else(|| panic!("{shown}"))
        .to_string();
    let panels = ui.snapshot().panels;
    assert_eq!(panels.len(), 1);
    assert_eq!(panels[0].panel.kind, PanelKind::Review);
    assert_eq!(panels[0].owner, "caps");
    ui.act(UiAction::Review {
        panel: id.clone(),
        accepted: true,
        text: "edited".into(),
    })
    .unwrap();
    until("the review event", async || {
        rt.recent_logs(Some("caps"), 100)
            .iter()
            .any(|l| l.message.contains("ReviewDone"))
    })
    .await;
    assert!(ui.snapshot().panels.is_empty());

    // Stopping the plugin removes its tile and its widget.
    rt.shutdown().await;
    let snap = ui.snapshot();
    assert!(snap.tiles.is_empty() && snap.widgets.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_plugins_stay_off_until_enabled() {
    let node = Node::new();
    node.install_hello();
    let rt = Runtime::start(
        node.paths.clone(),
        Options {
            disabled: ["hello".to_string()].into(),
            ..Options::default()
        },
    )
    .await
    .unwrap();
    let state = async |rt: &Runtime| rt.plugin_status("hello").await.unwrap().state;
    assert_eq!(state(&rt).await, State::Disabled);
    assert_eq!(rt.plugin_status("hello").await.unwrap().version, "0.1.0");
    // A reload, as the file watcher does, keeps it off.
    rt.reload("hello").await;
    assert_eq!(state(&rt).await, State::Disabled);
    assert!(rt.fire("hello", ProviderEvent::Timer(1)).is_err());

    rt.set_enabled("hello", true).await.unwrap();
    assert_eq!(state(&rt).await, State::Running);
    assert!(rt.disabled().is_empty());
    rt.set_enabled("hello", false).await.unwrap();
    assert_eq!(state(&rt).await, State::Disabled);
    assert_eq!(rt.disabled(), ["hello".to_string()].into());
    assert!(rt.set_enabled("nope", false).await.is_err());
    rt.shutdown().await;
}

// Only the Unix tests below use it.
#[cfg(unix)]
/// Answers `POST /transcribe` with "heard <the posted path's file name>",
/// the way parakeet answers with the text of the file it was given.
fn echo_speech_server() -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            // Read the headers, then as many body bytes as they announce.
            let mut request = Vec::new();
            let mut buf = [0; 4096];
            let body_at = loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                request.extend_from_slice(&buf[..n]);
                if let Some(i) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(i + 4);
                }
                if n == 0 {
                    break None;
                }
            };
            let Some(body_at) = body_at else { continue };
            let head = String::from_utf8_lossy(&request[..body_at]).to_ascii_lowercase();
            let length: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            while request.len() < body_at + length {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }
            let path = String::from_utf8_lossy(&request[body_at..]).into_owned();
            let body = format!("heard {}", path.rsplit('/').next().unwrap_or_default());
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
}

/// A plugin records with `mic` (a fake ffmpeg here), gets a path in its
/// own data folder, and `speech.transcribe-file` accepts that path without
/// any `disk:` grant. `bricolo status` lists the plugin under `active_sensors`
/// while it records.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_recording_lands_in_the_plugins_folder_and_transcribes() {
    use std::os::unix::fs::PermissionsExt;
    let node = Node::new();
    let fake = node.root.join("ffmpeg");
    std::fs::write(
        &fake,
        "#!/bin/sh\ntrap 'exit 255' INT\nfor a in \"$@\"; do out=\"$a\"; done\n\
         { printf 'RIFF\\0\\0\\0\\0WAVEdata\\0\\0\\0\\0'; head -c 3200 /dev/zero; } > \"$out\"\nwhile :; do sleep 0.1; done\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(node.root.join("config")).unwrap();
    std::fs::write(
        node.paths.node_toml(),
        format!(
            "[mic]\nffmpeg = {:?}\n[speech]\nurl = \"http://127.0.0.1:{}\"\n",
            fake.display().to_string(),
            echo_speech_server()
        ),
    )
    .unwrap();
    node.install_caps(&caps_manifest(&[ECHO], &["127.0.0.1:1".into()]));
    let rt = node.start(false).await;
    let fire = |cmd: &str| rt.fire("caps", ProviderEvent::Hotkey(cmd.into())).unwrap();

    fire("mic-start");
    assert_eq!(line(&rt, "caps", "mic-start").await, "mic-start: 1");
    // A script written a moment ago can take seconds to start on a loaded
    // Mac, while macOS checks it: wait for the fake's file.
    let recordings = node.paths.plugin_data("caps").join("recordings");
    until("the fake ffmpeg's file", async || {
        std::fs::read_dir(&recordings)
            .is_ok_and(|d| d.flatten().any(|e| e.metadata().is_ok_and(|m| m.len() > 0)))
    })
    .await;
    let status = serde_json::to_value(rt.status().await).unwrap();
    assert_eq!(
        status["active_sensors"],
        serde_json::json!([{ "plugin": "caps", "sensor": "mic" }])
    );

    fire("mic-stop");
    let out = line(&rt, "caps", "mic-stop").await;
    assert!(
        out.starts_with("mic-stop: Ok(\"heard rec-") && out.ends_with("-1.wav\")"),
        "{out}"
    );
    let status = serde_json::to_value(rt.status().await).unwrap();
    assert_eq!(status["active_sensors"], serde_json::json!([]));
    assert_eq!(std::fs::read_dir(recordings).unwrap().count(), 1);
    rt.shutdown().await;
}

/// A plugin takes a photo through the host; the fake ffmpeg writes the file.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn camera_snapshot_through_a_plugin() {
    use std::os::unix::fs::PermissionsExt;
    let node = Node::new();
    let fake = node.root.join("ffmpeg");
    std::fs::write(
        &fake,
        "#!/bin/sh\nfor a in \"$@\"; do out=\"$a\"; done\necho jpeg > \"$out\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(node.root.join("config")).unwrap();
    std::fs::write(
        node.paths.node_toml(),
        format!("[camera]\nffmpeg = {:?}\n", fake.display().to_string()),
    )
    .unwrap();
    node.install_caps(&caps_manifest(&[ECHO], &["127.0.0.1:1".into()]));
    let rt = node.start(false).await;
    rt.fire("caps", ProviderEvent::Hotkey("snapshot".into()))
        .unwrap();
    let got = line(&rt, "caps", "snapshot:").await;
    assert!(got.starts_with("snapshot: Ok(\""), "{got}");
    let path = got
        .trim_start_matches("snapshot: Ok(\"")
        .trim_end_matches("\")");
    assert!(
        std::fs::read_to_string(path).unwrap().contains("jpeg"),
        "{got}"
    );
    rt.shutdown().await;
}

/// A plugin reaches `audio`: its own sounds list, and a file outside its
/// `disk:` grants is refused before anything plays.
#[tokio::test(flavor = "multi_thread")]
async fn audio_checks_the_plugins_grants() {
    let node = Node::new();
    node.install_caps(&caps_manifest(&[ECHO], &["127.0.0.1:1".into()]));
    let rt = node.start(false).await;
    let fire = |cmd: &str| rt.fire("caps", ProviderEvent::Hotkey(cmd.into())).unwrap();
    fire("sounds");
    assert_eq!(line(&rt, "caps", "sounds").await, "sounds: Ok([])");
    let outside = node.root.join("noise.wav");
    std::fs::write(&outside, "RIFF").unwrap();
    fire(&format!("play:{}", outside.display()));
    assert_eq!(
        line(&rt, "caps", "play").await,
        "play: Err(CallError::Denied)"
    );
    rt.shutdown().await;
}

/// The three new events reach a plugin as WIT events.
#[tokio::test(flavor = "multi_thread")]
async fn app_system_and_file_events_reach_a_plugin() {
    use bricolo_providers::app::AppInfo;
    use bricolo_providers::{AppChange, AppEvent, FileChange, FileEvent, SystemEvent};
    let node = Node::new();
    node.install_caps(&caps_manifest(&[ECHO], &["127.0.0.1:1".into()]));
    let rt = node.start(false).await;
    let app = AppInfo {
        name: "Zoom".into(),
        bundle_id: None,
        pid: 3,
    };
    rt.fire(
        "caps",
        ProviderEvent::App(AppEvent {
            change: AppChange::Launched,
            app,
        }),
    )
    .unwrap();
    assert_eq!(
        line(&rt, "caps", "app-event:").await,
        "app-event: AppChange::Launched Zoom"
    );
    rt.fire("caps", ProviderEvent::System(SystemEvent::ScreenLocked))
        .unwrap();
    assert_eq!(
        line(&rt, "caps", "system-event:").await,
        "system-event: SystemEvent::ScreenLocked"
    );
    let file = FileEvent {
        watch: 4,
        path: "/x/a.txt".into(),
        change: FileChange::Removed,
    };
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
    if linux_display() {
        eprintln!("skipped: a desktop session answers these calls");
        return;
    }
    let node = Node::new();
    node.install_caps(&caps_manifest(&[ECHO], &["127.0.0.1:1".into()]));
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

/// `mouse` goes through the host, and a bad click count is refused before
/// any provider runs.
#[tokio::test(flavor = "multi_thread")]
async fn mouse_reaches_the_host_and_checks_the_count() {
    if linux_display() {
        eprintln!("skipped: a desktop session answers these calls");
        return;
    }
    let node = Node::new();
    node.install_caps(&caps_manifest(&[ECHO], &["127.0.0.1:1".into()]));
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

/// `secrets` goes through the host. Tests have no main-thread loop, so the
/// node has no Keychain and the call answers unavailable. On Linux the call
/// goes to the Secret Service on the session bus, so it is skipped when
/// one may be there: this test must not reach a real keyring.
#[tokio::test(flavor = "multi_thread")]
async fn secrets_reach_the_host_without_the_keychain() {
    if cfg!(windows) {
        eprintln!("skipped: Windows has the Credential Manager");
        return;
    }
    if linux_session_bus() {
        eprintln!("skipped: a D-Bus session bus may be running");
        return;
    }
    let node = Node::new();
    node.install_caps(&caps_manifest(&[ECHO], &["127.0.0.1:1".into()]));
    let rt = node.start(false).await;
    rt.fire("caps", ProviderEvent::Hotkey("secret-get:token".into()))
        .unwrap();
    assert_eq!(
        line(&rt, "caps", "secret-get:").await,
        "secret-get: Err(CallError::Unavailable)"
    );
    rt.shutdown().await;
}

/// The new window and app calls go through the host. None of these targets
/// exist, so every call comes back as an error, whatever the permissions.
#[tokio::test(flavor = "multi_thread")]
async fn window_and_app_controls_reach_the_host() {
    let node = Node::new();
    node.install_caps(&caps_manifest(&[ECHO], &["127.0.0.1:1".into()]));
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
