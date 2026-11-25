//! The Linux desktop providers against a real X server.
//!
//! The `x11_` tests need Xvfb, an EWMH window manager (openbox), xterm,
//! xdotool and xclip, the notification test a D-Bus session with dunst,
//! and the secrets test gnome-keyring, unlocked in that session, and
//! secret-tool. They are ignored by default. `just linux-test` runs them in a
//! container that has all of that (scripts/linux-test/inside.sh), one at a
//! time, since they share the screen, the keyboard and the clipboard:
//!
//! ```sh
//! cargo nextest run --run-ignored only --test-threads 1 -E 'test(/^x11_/)'
//! ```
#![cfg(target_os = "linux")]

use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bricolo_providers::window::{Rect, WindowInfo};
use bricolo_providers::{CallError, Caller, EventSink, LocalOptions, ProviderEvent, Providers};

type Events = Arc<Mutex<Vec<(String, ProviderEvent)>>>;

fn providers(data: &std::path::Path) -> (Providers, Events) {
    let events: Events = Arc::default();
    let seen = events.clone();
    let sink: EventSink = Arc::new(move |plugin, event| {
        seen.lock().unwrap().push((plugin.to_string(), event));
    });
    (
        Providers::local(LocalOptions::new(data.to_path_buf()), sink),
        events,
    )
}

async fn until<T>(what: &str, mut check: impl AsyncFnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(found) = check().await {
            return found;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn run(program: &str, args: &[&str]) -> String {
    let out = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run {program}: {e}"));
    assert!(out.status.success(), "{program} {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// An xterm running `sleep`, closed on drop.
struct Xterm(Child);

impl Xterm {
    fn open(title: &str, geometry: &str) -> Self {
        let child = Command::new("xterm")
            .args(["+sb", "-title", title, "-geometry", geometry])
            .args(["-e", "sleep", "600"])
            .spawn()
            .expect("cannot start xterm");
        Self(child)
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Xterm {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn window(p: &Providers, title: &str) -> WindowInfo {
    let c = Caller::new("test");
    until(&format!("the window {title:?}"), async || {
        let list = p.window.list(&c).await.unwrap();
        list.into_iter().find(|w| w.title == title)
    })
    .await
}

fn unique(what: &str) -> String {
    format!("bricolo {what} {}", std::process::id())
}

#[ignore = "needs an X server; run by `just linux-test`"]
#[tokio::test(flavor = "multi_thread")]
async fn x11_clipboard_round_trip_with_other_programs() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let c = Caller::new("test");
    let ours = unique("clipboard");
    p.clipboard.set(&c, ours.clone()).await.unwrap();
    assert_eq!(p.clipboard.get(&c).await.unwrap().as_deref(), Some(&*ours));
    // Another program reads what we set.
    assert_eq!(run("xclip", &["-o", "-selection", "clipboard"]), ours);
    // And we read what another program sets. xclip stays in the background
    // to serve the selection until someone else takes it.
    let mut xclip = Command::new("xclip")
        .args(["-i", "-selection", "clipboard"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = xclip.stdin.take().unwrap();
        stdin.write_all(b"from xclip").unwrap();
    }
    xclip.wait().unwrap();
    let theirs = until("xclip's text", async || {
        let text = p.clipboard.get(&c).await.unwrap()?;
        (text == "from xclip").then_some(text)
    })
    .await;
    assert_eq!(theirs, "from xclip");
    p.clipboard.set(&c, ours.clone()).await.unwrap();
}

#[ignore = "needs an X server; run by `just linux-test`"]
#[tokio::test(flavor = "multi_thread")]
async fn x11_windows_list_focus_and_set_frame() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let c = Caller::new("test");
    let (ta, tb) = (unique("window a"), unique("window b"));
    let _a = Xterm::open(&ta, "40x10+40+50");
    let _b = Xterm::open(&tb, "40x10+300+200");
    let a = window(&p, &ta).await;
    let b = window(&p, &tb).await;
    assert_eq!(a.app, "XTerm");
    assert_ne!(a.id, b.id);

    for target in [&a, &b, &a] {
        p.window.focus(&c, target.id).await.unwrap();
        until("the focus to move", async || {
            let focused = p.window.focused(&c).await.unwrap()?;
            (focused.id == target.id).then_some(())
        })
        .await;
    }

    let want = Rect {
        x: 120,
        y: 90,
        width: 500,
        height: 300,
    };
    p.window.set_frame(&c, a.id, want).await.unwrap();
    // The corner lands exactly; xterm sizes in whole character cells, so
    // the size may come out up to a cell smaller. The window manager may
    // move the window before it resizes it, so this waits for both.
    let landed = |f: Rect| {
        (f.x, f.y) == (want.x, want.y)
            && want.width.abs_diff(f.width) < 20
            && want.height.abs_diff(f.height) < 30
    };
    until("the new frame", async || {
        let w = window(&p, &ta).await;
        landed(w.frame).then_some(())
    })
    .await;

    // Ids that are not managed windows are refused.
    let root = p.window.focus(&c, u64::from(u32::MAX)).await;
    assert_eq!(root, Err(CallError::Failed("no such window".into())));
}

#[ignore = "needs an X server; run by `just linux-test`"]
#[tokio::test(flavor = "multi_thread")]
async fn x11_screens_come_from_randr() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let screens = p.window.screens(&Caller::new("test")).await.unwrap();
    assert_eq!(screens.len(), 1, "{screens:?}");
    let s = &screens[0];
    assert!(s.main);
    // Xvfb in inside.sh runs at 1280x800; openbox adds no panels.
    assert_eq!((s.frame.width, s.frame.height), (1280, 800), "{s:?}");
    assert_eq!(s.visible_frame, s.frame);
}

/// Reads a PNG's width and height from its IHDR chunk.
fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    assert_eq!(&bytes[12..16], b"IHDR");
    let word = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (word(16), word(20))
}

#[ignore = "needs an X server; run by `just linux-test`"]
#[tokio::test(flavor = "multi_thread")]
async fn x11_screen_capture_writes_a_png() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let c = Caller::new("test");
    let area = Rect {
        x: 40,
        y: 30,
        width: 200,
        height: 120,
    };
    let path = p.screen.capture(&c, Some(area)).await.unwrap();
    assert!(
        path.starts_with(&*dir.path().join("test/screenshots").to_string_lossy()),
        "{path}"
    );
    assert_eq!(png_size(&std::fs::read(&path).unwrap()), (200, 120));
    // The whole root window: Xvfb in inside.sh runs at 1280x800.
    let whole = p.screen.capture(&c, None).await.unwrap();
    assert_eq!(png_size(&std::fs::read(&whole).unwrap()), (1280, 800));
    // An area hanging off the edge is clipped; one off the screen fails.
    let edge = Rect {
        x: 1200,
        y: 700,
        width: 200,
        height: 200,
    };
    let clipped = p.screen.capture(&c, Some(edge)).await.unwrap();
    assert_eq!(png_size(&std::fs::read(&clipped).unwrap()), (80, 100));
    let off = Rect { x: 5000, ..edge };
    assert!(matches!(
        p.screen.capture(&c, Some(off)).await,
        Err(CallError::Failed(_))
    ));
}

#[ignore = "needs an X server; run by `just linux-test`"]
#[tokio::test(flavor = "multi_thread")]
async fn x11_hotkey_presses_reach_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    let (p, events) = providers(dir.path());
    let c = Caller::new("plugin");
    let chord = "ctrl+alt+shift+k";
    p.hotkey.bind(&c, "k".into(), chord.into()).await.unwrap();
    assert_eq!(p.hotkey.bound("plugin"), [("k".into(), chord.into())]);
    let pressed = |n: usize| {
        let events = events.clone();
        async move || (events.lock().unwrap().len() >= n).then_some(())
    };

    run("xdotool", &["key", "ctrl+alt+shift+k"]);
    until("the press", pressed(1)).await;
    assert_eq!(
        events.lock().unwrap()[0],
        ("plugin".into(), ProviderEvent::Hotkey("k".into()))
    );

    // Num Lock and Caps Lock don't get in the way.
    run("xdotool", &["key", "Num_Lock"]);
    run("xdotool", &["key", "ctrl+alt+shift+k"]);
    run("xdotool", &["key", "Num_Lock"]);
    until("the press with Num Lock on", pressed(2)).await;

    // Another program (here, a second node) cannot take the same chord.
    let (other, _) = providers(dir.path());
    let taken = other.hotkey.bind(&c, "k".into(), chord.into()).await;
    assert!(
        matches!(&taken, Err(CallError::Failed(m)) if m.contains("another program")),
        "{taken:?}"
    );
    // And a chord bound by another plugin here is refused.
    let other_plugin = Caller::new("other");
    let mine = p.hotkey.bind(&other_plugin, "x".into(), chord.into()).await;
    assert!(mine.is_err());

    p.hotkey.unbind(&c, "k".into()).await.unwrap();
    run("xdotool", &["key", "ctrl+alt+shift+k"]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(events.lock().unwrap().len(), 2);
    // Released, the chord is free for the second node.
    other
        .hotkey
        .bind(&c, "k".into(), chord.into())
        .await
        .unwrap();
    other.hotkey.release("plugin");
    assert!(other.hotkey.bound("plugin").is_empty());
}

#[ignore = "needs an X server; run by `just linux-test`"]
#[tokio::test(flavor = "multi_thread")]
async fn x11_apps_running_frontmost_and_launch() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let c = Caller::new("test");
    let title = unique("app");
    let xterm = Xterm::open(&title, "40x10+60+60");
    let w = window(&p, &title).await;
    p.window.focus(&c, w.id).await.unwrap();
    let front = until("xterm in front", async || {
        let front = p.app.frontmost(&c).await.unwrap()?;
        (front.pid == xterm.pid() as i32).then_some(front)
    })
    .await;
    assert_eq!(front.name, "XTerm");
    let running = p.app.running(&c).await.unwrap();
    assert!(
        running.iter().any(|a| a.pid == xterm.pid() as i32),
        "{running:?}"
    );

    // A desktop file in a data folder of our own, found by its Name and
    // started with `gio launch`.
    let data = dir.path().join("share");
    let launched = unique("launched");
    std::fs::create_dir_all(data.join("applications")).unwrap();
    std::fs::write(
        data.join("applications/bricolo-test.desktop"),
        format!(
            "[Desktop Entry]\nType=Application\nName=Bricolo Test App\n\
             Exec=xterm -class RhTest -title \"{launched}\" -e sleep 600\n\
             StartupWMClass=RhTest\n"
        ),
    )
    .unwrap();
    // SAFETY: nextest runs each test in its own process, and nothing else
    // in this one reads the environment concurrently.
    unsafe { std::env::set_var("XDG_DATA_HOME", &data) };
    p.app.launch(&c, "Bricolo Test App".into()).await.unwrap();
    let new = window(&p, &launched).await;
    assert_eq!(new.app, "RhTest");
    // Launching it again raises the open window instead of a second one.
    p.window.focus(&c, w.id).await.unwrap();
    p.app.launch(&c, "bricolo-test".into()).await.unwrap();
    until("the launched window in front", async || {
        let focused = p.window.focused(&c).await.unwrap()?;
        (focused.id == new.id).then_some(())
    })
    .await;
    let count = p.window.list(&c).await.unwrap();
    assert_eq!(count.iter().filter(|w| w.title == launched).count(), 1);
    let pid = p
        .app
        .frontmost(&c)
        .await
        .unwrap()
        .expect("a frontmost app")
        .pid;
    unsafe { libc::kill(pid, libc::SIGTERM) };

    assert_eq!(
        p.app.open_url(&c, "file:///etc/passwd".into()).await,
        Err(CallError::Denied)
    );
    assert!(p.app.launch(&c, "/usr/bin/xterm".into()).await.is_err());
}

/// An xterm running `sleep` whose `WM_CLASS` class is `class`, so an app
/// test can name it without touching other xterms.
fn xterm_of_class(class: &str, title: &str) -> Xterm {
    let child = Command::new("xterm")
        .args(["+sb", "-class", class, "-title", title])
        .args(["-geometry", "30x5+80+80", "-e", "sleep", "600"])
        .spawn()
        .expect("cannot start xterm");
    Xterm(child)
}

/// The ICCCM state the window manager gives a window: "Normal" or "Iconic".
fn wm_state(id: u64) -> String {
    run("xprop", &["-id", &id.to_string(), "WM_STATE"])
}

#[ignore = "needs an X server; run by `just linux-test`"]
#[tokio::test(flavor = "multi_thread")]
async fn x11_app_set_hidden_and_restore() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let c = Caller::new("test");
    let class = format!("RhHide{}", std::process::id());
    let (one, two) = (unique("hide one"), unique("hide two"));
    let _first = xterm_of_class(&class, &one);
    let a = window(&p, &one).await;
    let _second = xterm_of_class(&class, &two);
    let b = window(&p, &two).await;

    p.app.set_hidden(&c, class.clone(), true).await.unwrap();
    until("both windows minimized", async || {
        [a.id, b.id]
            .iter()
            .all(|&id| wm_state(id).contains("Iconic"))
            .then_some(())
    })
    .await;

    // Any case finds the app; restoring leaves the newest window in front.
    p.app
        .set_hidden(&c, class.to_lowercase(), false)
        .await
        .unwrap();
    until("both windows restored", async || {
        [a.id, b.id]
            .iter()
            .all(|&id| wm_state(id).contains("Normal"))
            .then_some(())
    })
    .await;
    until("the newest window in front", async || {
        let focused = p.window.focused(&c).await.unwrap()?;
        (focused.id == b.id).then_some(())
    })
    .await;

    assert_eq!(
        p.app
            .set_hidden(&c, "bricolo-no-such-app".into(), true)
            .await,
        Err(CallError::Failed("no such app".into()))
    );
}

#[ignore = "needs an X server; run by `just linux-test`"]
#[tokio::test(flavor = "multi_thread")]
async fn x11_app_quit_closes_its_windows() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let c = Caller::new("test");
    let class = format!("RhQuit{}", std::process::id());
    let (one, two) = (unique("quit one"), unique("quit two"));
    let mut first = xterm_of_class(&class, &one);
    window(&p, &one).await;
    let mut second = xterm_of_class(&class, &two);
    window(&p, &two).await;
    // Another xterm, which quitting the app must leave open.
    let other = unique("quit other");
    let _other = Xterm::open(&other, "30x5+120+120");
    window(&p, &other).await;

    p.app.quit(&c, class.clone()).await.unwrap();
    // xterm exits when its window is asked to close.
    until("both xterms exited", async || {
        let a = first.0.try_wait().unwrap().is_some();
        let b = second.0.try_wait().unwrap().is_some();
        (a && b).then_some(())
    })
    .await;
    let list = until("the app's windows gone from the list", async || {
        let list = p.window.list(&c).await.unwrap();
        (!list.iter().any(|w| w.app == class)).then_some(list)
    })
    .await;
    assert!(list.iter().any(|w| w.title == other), "{list:?}");

    assert_eq!(
        p.app.quit(&c, class).await,
        Err(CallError::Failed("no such app".into()))
    );
}

#[ignore = "needs a D-Bus session with a notification server; run by `just linux-test`"]
#[tokio::test(flavor = "multi_thread")]
async fn x11_notify_reaches_the_notification_server() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let body = unique("notification");
    p.notify
        .show(&Caller::new("test"), "bricolo".into(), body.clone())
        .await
        .unwrap();
    // dunst keeps closed notifications in its history. (`dunstctl history`
    // needs systemd's busctl, so ask dunst directly.)
    run("dunstctl", &["close-all"]);
    let history = until("the notification in dunst's history", async || {
        let history = run(
            "dbus-send",
            &[
                "--session",
                "--print-reply",
                "--dest=org.freedesktop.Notifications",
                "/org/freedesktop/Notifications",
                "org.dunstproject.cmd0.NotificationListHistory",
            ],
        );
        history.contains(&body).then_some(history)
    })
    .await;
    assert!(history.contains("\"bricolo\""), "{history}");
}

/// `secrets` through a real Secret Service: gnome-keyring, with a
/// throwaway keyring in a temp folder inside the test D-Bus session
/// (scripts/linux-test/inside.sh), never the user's keyring. `secret-tool` (libsecret) reads
/// the item back, so other Secret Service clients find it by the same
/// attributes.
#[ignore = "needs a D-Bus session with an unlocked Secret Service; run by `just linux-test`"]
#[tokio::test(flavor = "multi_thread")]
async fn x11_secrets_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let c = Caller::new("bricolo-test");
    let name = unique("secret").replace(' ', "-");
    let account = format!("bricolo-test/{name}");
    let lookup = || {
        Command::new("secret-tool")
            .args(["lookup", "service", "bricolo", "account", &account])
            .output()
            .expect("cannot run secret-tool")
    };

    assert_eq!(p.secrets.get(&c, name.clone()).await, Ok(None));
    p.secrets.set(&c, name.clone(), "v1".into()).await.unwrap();
    assert_eq!(p.secrets.get(&c, name.clone()).await, Ok(Some("v1".into())));
    p.secrets.set(&c, name.clone(), "v2".into()).await.unwrap();
    assert_eq!(p.secrets.get(&c, name.clone()).await, Ok(Some("v2".into())));
    // Replaced, not added: one item with the new value.
    assert_eq!(String::from_utf8_lossy(&lookup().stdout), "v2");
    let search = Command::new("secret-tool")
        .args(["search", "--all", "service", "bricolo", "account", &account])
        .output()
        .expect("cannot run secret-tool");
    let listing = String::from_utf8_lossy(&search.stdout).into_owned()
        + &String::from_utf8_lossy(&search.stderr);
    assert_eq!(listing.matches("secret = ").count(), 1, "{listing}");
    assert!(
        listing.contains(&format!("label = bricolo: {account}")),
        "{listing}"
    );

    // Another plugin does not see it.
    assert_eq!(
        p.secrets.get(&Caller::new("other"), name.clone()).await,
        Ok(None)
    );

    p.secrets.delete(&c, name.clone()).await.unwrap();
    assert_eq!(p.secrets.get(&c, name.clone()).await, Ok(None));
    assert!(lookup().stdout.is_empty());
    // Deleting what is not there is fine.
    p.secrets.delete(&c, name.clone()).await.unwrap();
}

/// Without a display the desktop capabilities answer `unavailable` and
/// everything else still works. Skipped when a display is set.
#[tokio::test(flavor = "multi_thread")]
async fn without_a_display_desktop_capabilities_are_unavailable() {
    let has = |v: &str| std::env::var_os(v).is_some_and(|v| !v.is_empty());
    if has("DISPLAY") || has("WAYLAND_DISPLAY") {
        eprintln!("skipped: a display is set");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let c = Caller::new("test");
    assert_eq!(p.window.list(&c).await, Err(CallError::Unavailable));
    assert_eq!(p.window.screens(&c).await, Err(CallError::Unavailable));
    assert_eq!(
        p.hotkey.bind(&c, "k".into(), "ctrl+k".into()).await,
        Err(CallError::Unavailable)
    );
    assert_eq!(p.clipboard.get(&c).await, Err(CallError::Unavailable));
    assert_eq!(p.app.running(&c).await, Err(CallError::Unavailable));
    assert_eq!(p.app.frontmost(&c).await, Err(CallError::Unavailable));
    assert_eq!(
        p.screen.capture(&c, None).await,
        Err(CallError::Unavailable)
    );
    assert_eq!(
        p.keyboard.type_text(&c, "hi".into()).await,
        Err(CallError::Unavailable)
    );
    assert_eq!(
        p.app.open_url(&c, "file:///etc/passwd".into()).await,
        Err(CallError::Denied)
    );
    // The portable capabilities don't care.
    p.kv.set("test", "k", "v").unwrap();
    assert_eq!(p.kv.get("test", "k").as_deref(), Some("v"));
}

/// `keyboard.keystroke` sends the chord through XTEST, and the hotkey
/// grab on the same server sees it, modifiers included.
#[ignore = "needs an X server; run by `just linux-test`"]
#[tokio::test(flavor = "multi_thread")]
async fn x11_keyboard_keystroke_reaches_a_hotkey_grab() {
    let dir = tempfile::tempdir().unwrap();
    let (p, events) = providers(dir.path());
    let c = Caller::new("plugin");
    p.hotkey
        .bind(&c, "j".into(), "ctrl+alt+shift+j".into())
        .await
        .unwrap();
    let mods = ["ctrl", "alt", "shift"].map(String::from).to_vec();
    p.keyboard.keystroke(&c, mods, "j".into()).await.unwrap();
    until("the press", async || {
        (!events.lock().unwrap().is_empty()).then_some(())
    })
    .await;
    assert_eq!(
        events.lock().unwrap()[0],
        ("plugin".into(), ProviderEvent::Hotkey("j".into()))
    );
    // Without the modifiers, the grab does not fire.
    p.keyboard.keystroke(&c, vec![], "j".into()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(events.lock().unwrap().len(), 1);
    let bad = p
        .keyboard
        .keystroke(&c, vec!["hyper".into()], "j".into())
        .await;
    assert!(matches!(bad, Err(CallError::Failed(_))), "{bad:?}");
    p.hotkey.release("plugin");
}

/// `keyboard.type_text` types into a focused xterm whose shell reads one
/// line into a file: shifted characters, a character the US map lacks
/// (typed through a spare keycode), and Enter for the line break.
#[ignore = "needs an X server; run by `just linux-test`"]
#[tokio::test(flavor = "multi_thread")]
async fn x11_keyboard_types_into_an_xterm() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let out = dir.path().join("typed.txt");
    let title = "bricolo-typing-target";
    let script = format!("IFS= read -r line; printf %s \"$line\" > {}", out.display());
    let mut xterm = Command::new("xterm")
        .env("LC_ALL", "C.UTF-8")
        .args(["+sb", "-u8", "-title", title, "-geometry", "80x10+10+10"])
        .args(["-e", "sh", "-c", &script])
        .spawn()
        .expect("cannot start xterm");
    let id = run("xdotool", &["search", "--sync", "--name", title])
        .lines()
        .next()
        .unwrap()
        .to_string();
    run("xdotool", &["windowactivate", "--sync", &id]);
    // Give the shell inside a moment to reach `read`.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let c = Caller::new("plugin");
    let text = "Hello, World! 42 (café)";
    p.keyboard.type_text(&c, format!("{text}\n")).await.unwrap();
    let typed = until("the typed line", async || {
        std::fs::read_to_string(&out).ok().filter(|t| !t.is_empty())
    })
    .await;
    assert_eq!(typed, text);
    let _ = xterm.kill();
    let _ = xterm.wait();
}

/// `system.idle-seconds` comes from the X server's screen saver extension,
/// which counts XTEST input as the user's.
#[ignore = "needs an X server; run by `just linux-test`"]
#[tokio::test(flavor = "multi_thread")]
async fn x11_system_idle_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let c = Caller::new("test");
    run("xdotool", &["mousemove_relative", "--", "3", "3"]);
    let fresh = p.system.idle_seconds(&c).await.unwrap();
    assert!(fresh <= 1, "idle {fresh}s right after a pointer move");
    tokio::time::sleep(Duration::from_millis(2200)).await;
    let later = p.system.idle_seconds(&c).await.unwrap();
    assert!(later >= 2, "idle {later}s after two quiet seconds");
    run("xdotool", &["mousemove_relative", "--", "-3", "-3"]);
    let again = p.system.idle_seconds(&c).await.unwrap();
    assert!(again <= 1, "idle {again}s after moving again");
}

/// The RandR monitors as `xrandr --listmonitors` prints them, primary
/// flag included.
fn monitors() -> String {
    run("xrandr", &["--listmonitors"])
}

/// Puts back the RandR primary output found at the start, even when the
/// test fails, so the tests after it see the same screen.
struct RestorePrimary(Option<String>);

impl RestorePrimary {
    fn new() -> Self {
        let primary = run("xrandr", &["--query"])
            .lines()
            .find(|l| l.contains(" connected primary"))
            .and_then(|l| l.split_whitespace().next())
            .map(str::to_string);
        Self(primary)
    }
}

impl Drop for RestorePrimary {
    fn drop(&mut self) {
        let args: Vec<&str> = match &self.0 {
            Some(output) => vec!["--output", output, "--primary"],
            None => vec!["--noprimary"],
        };
        let _ = Command::new("xrandr").args(&args).status();
    }
}

/// `system.watch` works on X11 without logind (the container has no
/// system bus), and a RandR screen change reaches the watching plugin
/// only. Switching the primary output sends RandR's screen change
/// notification without resizing anything; the guard switches it back.
#[ignore = "needs an X server; run by `just linux-test`"]
#[tokio::test(flavor = "multi_thread")]
async fn x11_system_watch_sees_screen_changes() {
    use bricolo_providers::SystemEvent;
    let before = monitors();
    let output = run("xrandr", &["--query"])
        .lines()
        .find(|l| l.contains(" connected"))
        .and_then(|l| l.split_whitespace().next())
        .expect("no connected RandR output")
        .to_string();
    let dir = tempfile::tempdir().unwrap();
    let (p, events) = providers(dir.path());
    p.system.watch(&Caller::new("watcher"), true).await.unwrap();
    let changed = |events: &Events| {
        events.lock().unwrap().iter().any(|(plugin, e)| {
            plugin == "watcher" && *e == ProviderEvent::System(SystemEvent::ScreensChanged)
        })
    };
    {
        let _restore = RestorePrimary::new();
        run("xrandr", &["--output", &output, "--primary"]);
        until("a screens-changed event", async || {
            changed(&events).then_some(())
        })
        .await;
        assert!(
            events
                .lock()
                .unwrap()
                .iter()
                .all(|(plugin, _)| plugin == "watcher")
        );
        // After `release`, nothing more arrives.
        p.release("watcher");
        events.lock().unwrap().clear();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(events.lock().unwrap().is_empty());
    assert_eq!(monitors(), before, "the screen is not as the test found it");
}
