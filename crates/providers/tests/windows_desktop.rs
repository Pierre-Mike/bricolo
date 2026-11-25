//! The Windows desktop providers on a real Windows session.
//!
//! They act on the signed-in user's desktop: the clipboard, a toast, the
//! Credential Manager (a throwaway `bricolo-test` item, removed again), the
//! speakers, and power and session state. They are ignored by default; run
//! them one at a time on a Windows machine you can spare for a minute:
//!
//! ```sh
//! cargo nextest run -p bricolo-providers --test windows_desktop --run-ignored only --test-threads 1
//! ```
#![cfg(windows)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

fn unique(what: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("bricolo {what} {nanos}")
}

#[tokio::test]
#[ignore = "needs a Windows desktop session"]
async fn win_clipboard_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let caller = Caller::new("bricolo-test");
    let saved = p.clipboard.get(&caller).await.unwrap();
    let text = unique("clipboard é ✓");
    p.clipboard.set(&caller, text.clone()).await.unwrap();
    assert_eq!(p.clipboard.get(&caller).await.unwrap(), Some(text));
    if let Some(saved) = saved {
        p.clipboard.set(&caller, saved).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "needs a Windows desktop session; shows a toast"]
async fn win_notify_shows_a_toast() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let caller = Caller::new("bricolo-test");
    p.notify
        .show(
            &caller,
            "bricolo test".into(),
            "A toast from win_notify_shows_a_toast <&>".into(),
        )
        .await
        .unwrap();
    // Twice: the second call uses the AppUserModelID the first settled on.
    p.notify
        .show(&caller, "bricolo test".into(), String::new())
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "writes a throwaway item to the user's Credential Manager"]
async fn win_secrets_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let caller = Caller::new("bricolo-test");
    let name = "windows-round-trip".to_string();
    p.secrets.delete(&caller, name.clone()).await.unwrap();
    assert_eq!(p.secrets.get(&caller, name.clone()).await.unwrap(), None);
    p.secrets
        .set(&caller, name.clone(), "v1 é".into())
        .await
        .unwrap();
    assert_eq!(
        p.secrets.get(&caller, name.clone()).await.unwrap(),
        Some("v1 é".into())
    );
    p.secrets
        .set(&caller, name.clone(), "v2".into())
        .await
        .unwrap();
    assert_eq!(
        p.secrets.get(&caller, name.clone()).await.unwrap(),
        Some("v2".into())
    );
    // Another plugin does not see it.
    assert_eq!(
        p.secrets
            .get(&Caller::new("bricolo-test-other"), name.clone())
            .await
            .unwrap(),
        None
    );
    p.secrets.delete(&caller, name.clone()).await.unwrap();
    assert_eq!(p.secrets.get(&caller, name.clone()).await.unwrap(), None);
    // Deleting a missing one is fine.
    p.secrets.delete(&caller, name.clone()).await.unwrap();
    let too_long = "x".repeat(2561);
    assert!(matches!(
        p.secrets.set(&caller, name, too_long).await,
        Err(CallError::Failed(_))
    ));
}

#[tokio::test]
#[ignore = "needs a Windows desktop session"]
async fn win_system_idle_power_keep_awake_and_watch() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let caller = Caller::new("bricolo-test");
    let idle = p.system.idle_seconds(&caller).await.unwrap();
    assert!(idle < 24 * 3600, "idle {idle} s");
    let power = p.system.power(&caller).await.unwrap();
    if let Some(battery) = power.battery {
        assert!(battery <= 100);
    }
    p.system.keep_awake(&caller, true).await.unwrap();
    p.system.keep_awake(&caller, true).await.unwrap();
    p.system.keep_awake(&caller, false).await.unwrap();
    p.system.keep_awake(&caller, true).await.unwrap();
    p.system.watch(&caller, true).await.unwrap();
    p.system.watch(&caller, false).await.unwrap();
    p.release("bricolo-test");
}

/// Change the display resolution or scale, or lock and unlock (Win+L),
/// within 60 seconds of starting this test.
#[tokio::test]
#[ignore = "needs a person to lock the screen or change the display"]
async fn win_system_watch_sees_a_lock_or_a_display_change() {
    let dir = tempfile::tempdir().unwrap();
    let (p, events) = providers(dir.path());
    let caller = Caller::new("bricolo-test");
    p.system.watch(&caller, true).await.unwrap();
    let start = Instant::now();
    while events.lock().unwrap().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(60), "no system event");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let seen = events.lock().unwrap().clone();
    assert!(seen.iter().all(
        |(plugin, event)| plugin == "bricolo-test" && matches!(event, ProviderEvent::System(_))
    ));
}

#[tokio::test]
#[ignore = "needs a Windows session with sound; speaks aloud"]
async fn win_tts_speaks_with_sapi_and_stops() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let caller = Caller::new("bricolo-test");
    p.tts
        .speak(&caller, "Testing the Windows speech engine.".into(), None)
        .await
        .unwrap();
    let start = Instant::now();
    loop {
        let status = p.tts.status(&caller).await.unwrap();
        if !status.speaking && status.queued == 0 && start.elapsed() > Duration::from_millis(500) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "still speaking");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // A long text, cut off after a second.
    let long = "This sentence should be cut off. ".repeat(20);
    p.tts.speak(&caller, long, None).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    p.tts.stop(&caller).await.unwrap();
    let status = p.tts.status(&caller).await.unwrap();
    assert_eq!(status.queued, 0);
}

/// Waits up to 10 seconds for `check` to find something.
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
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn near(a: bricolo_providers::window::Rect, b: bricolo_providers::window::Rect) -> bool {
    (a.x - b.x).abs() <= 2
        && (a.y - b.y).abs() <= 2
        && a.width.abs_diff(b.width) <= 2
        && a.height.abs_diff(b.height) <= 2
}

/// Opens Notepad on a new file and drives it through every desktop
/// capability: the window list, focus, set-frame, screens, the mouse,
/// typing, a screenshot, minimizing, app events and quitting. On Windows
/// 11 the file may open as a tab of a Notepad window already open; close
/// Notepad first.
#[tokio::test]
#[ignore = "needs a Windows desktop session; opens Notepad and types into it"]
async fn win_notepad_window_keyboard_mouse_screen_and_quit() {
    use bricolo_providers::window::Rect;
    let dir = tempfile::tempdir().unwrap();
    let (p, events) = providers(dir.path());
    let caller = Caller::new("bricolo-test");
    p.app.watch(&caller, true).await.unwrap();

    let stem = unique("notepad").replace(' ', "-");
    let file = dir.path().join(format!("{stem}.txt"));
    std::fs::write(&file, "").unwrap();
    let mut child = std::process::Command::new("notepad.exe")
        .arg(&file)
        .spawn()
        .expect("cannot start notepad");
    // Reaped when it exits; on Windows 11 this is a stub that hands over.
    std::thread::spawn(move || child.wait());

    let window = until("the Notepad window", async || {
        let list = p.window.list(&caller).await.unwrap();
        list.into_iter().find(|w| w.title.contains(&stem))
    })
    .await;
    assert!(window.app.eq_ignore_ascii_case("notepad"), "{window:?}");
    let running = p.app.running(&caller).await.unwrap();
    assert!(
        running
            .iter()
            .any(|a| a.name.eq_ignore_ascii_case("notepad"))
    );

    p.window.focus(&caller, window.id).await.unwrap();
    until("Notepad in front", async || {
        let focused = p.window.focused(&caller).await.unwrap()?;
        (focused.id == window.id).then_some(())
    })
    .await;
    let front = p.app.frontmost(&caller).await.unwrap().unwrap();
    assert!(front.name.eq_ignore_ascii_case("notepad"));

    let screens = p.window.screens(&caller).await.unwrap();
    let main = screens.iter().find(|s| s.main).expect("a main screen");
    assert_eq!((main.frame.x, main.frame.y), (0, 0));
    assert!(main.visible_frame.height <= main.frame.height);

    let target = Rect {
        x: main.visible_frame.x + 100,
        y: main.visible_frame.y + 100,
        width: 640,
        height: 480,
    };
    p.window
        .set_frame(&caller, window.id, target)
        .await
        .unwrap();
    until("the new frame", async || {
        let list = p.window.list(&caller).await.unwrap();
        let w = list.into_iter().find(|w| w.id == window.id)?;
        near(w.frame, target).then_some(())
    })
    .await;

    let center = bricolo_providers::mouse::Point {
        x: target.x + 320,
        y: target.y + 240,
    };
    p.mouse.move_to(&caller, center).await.unwrap();
    assert_eq!(p.mouse.position(&caller).await.unwrap(), center);
    p.mouse
        .click(&caller, bricolo_providers::mouse::MouseButton::Left, 1)
        .await
        .unwrap();
    p.mouse.scroll(&caller, 0, 1).await.unwrap();

    p.keyboard
        .type_text(&caller, "hello é ✓\tdone\n".into())
        .await
        .unwrap();
    p.keyboard
        .keystroke(&caller, vec!["ctrl".into()], "s".into())
        .await
        .unwrap();
    until("the saved file", async || {
        let text = std::fs::read_to_string(&file).ok()?;
        text.contains("hello é ✓\tdone").then_some(())
    })
    .await;

    let shot = p.screen.capture(&caller, Some(target)).await.unwrap();
    let bytes = std::fs::read(&shot).unwrap();
    assert_eq!(&bytes[..4], b"\x89PNG");
    assert!(p.sensors.active().is_empty());
    let whole = p.screen.capture(&caller, None).await.unwrap();
    assert!(std::fs::metadata(&whole).unwrap().len() > 0);

    p.window
        .set_minimized(&caller, window.id, true)
        .await
        .unwrap();
    p.window
        .set_minimized(&caller, window.id, false)
        .await
        .unwrap();
    p.app
        .set_hidden(&caller, "notepad".into(), true)
        .await
        .unwrap();
    p.app
        .set_hidden(&caller, "notepad".into(), false)
        .await
        .unwrap();

    p.window.close(&caller, window.id).await.unwrap();
    until("Notepad to close", async || {
        let list = p.window.list(&caller).await.unwrap();
        (!list.iter().any(|w| w.id == window.id)).then_some(())
    })
    .await;
    // The watcher saw Notepad come to the front.
    let seen = events.lock().unwrap().clone();
    assert!(seen.iter().any(|(plugin, e)| plugin == "bricolo-test"
        && matches!(e, ProviderEvent::App(a) if a.app.name.eq_ignore_ascii_case("notepad"))));
    p.release("bricolo-test");

    assert!(matches!(
        p.app.quit(&caller, "bricolo-no-such-app".into()).await,
        Err(CallError::Failed(m)) if m == "no such app"
    ));
    assert!(matches!(
        p.window.focus(&caller, 1).await,
        Err(CallError::Failed(_))
    ));
    assert_eq!(
        p.app
            .open_url(&caller, "file:///C:/Windows/win.ini".into())
            .await,
        Err(CallError::Denied)
    );
}

/// Starts Notepad by name and closes only the windows it opened.
#[tokio::test]
#[ignore = "needs a Windows desktop session; opens Notepad"]
async fn win_app_launch_starts_or_raises() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let caller = Caller::new("bricolo-test");
    let before: Vec<u64> = p
        .window
        .list(&caller)
        .await
        .unwrap()
        .iter()
        .map(|w| w.id)
        .collect();
    p.app.launch(&caller, "notepad".into()).await.unwrap();
    let opened = until("a Notepad window", async || {
        let list = p.window.list(&caller).await.unwrap();
        list.into_iter()
            .find(|w| w.app.eq_ignore_ascii_case("notepad"))
    })
    .await;
    // A second launch raises it instead of starting another.
    p.app.launch(&caller, "Notepad.exe".into()).await.unwrap();
    if !before.contains(&opened.id) {
        p.window.close(&caller, opened.id).await.unwrap();
    }
    assert!(matches!(
        p.app.launch(&caller, "bricolo-no-such-app".into()).await,
        Err(CallError::Failed(_))
    ));
    assert!(
        p.app
            .launch(&caller, r"C:\Windows\notepad.exe".into())
            .await
            .is_err()
    );
}

/// Binds a chord no program uses, presses it with `keyboard`, and checks
/// the press and the release arrive.
#[tokio::test]
#[ignore = "needs a Windows desktop session; presses keys"]
async fn win_hotkey_fires_and_releases() {
    let dir = tempfile::tempdir().unwrap();
    let (p, events) = providers(dir.path());
    let caller = Caller::new("bricolo-test");
    let chord = "ctrl+alt+shift+f12";
    p.hotkey
        .bind(&caller, "go".into(), chord.into())
        .await
        .unwrap();
    // The same chord again under the same id is fine; another plugin is
    // refused.
    p.hotkey
        .bind(&caller, "go".into(), chord.into())
        .await
        .unwrap();
    assert!(matches!(
        p.hotkey
            .bind(
                &Caller::new("bricolo-test-other"),
                "go".into(),
                chord.into()
            )
            .await,
        Err(CallError::Failed(_))
    ));
    assert_eq!(
        p.hotkey.bind(&caller, "fn".into(), "fn".into()).await,
        Err(CallError::Unavailable)
    );
    p.keyboard
        .keystroke(
            &caller,
            vec!["ctrl".into(), "alt".into(), "shift".into()],
            "f12".into(),
        )
        .await
        .unwrap();
    until("the press and the release", async || {
        let seen = events.lock().unwrap().clone();
        let pressed = seen
            .iter()
            .any(|(pl, e)| pl == "bricolo-test" && *e == ProviderEvent::Hotkey("go".into()));
        let released = seen.iter().any(|(pl, e)| {
            pl == "bricolo-test" && *e == ProviderEvent::HotkeyReleased("go".into())
        });
        (pressed && released).then_some(())
    })
    .await;
    p.release("bricolo-test");
    assert!(p.hotkey.bound("bricolo-test").is_empty());
}

/// Opens Notepad, reads it through UI Automation, writes its text and reads
/// it back. Close other Notepad windows first; the test closes its own.
#[tokio::test]
#[ignore = "needs a Windows desktop session"]
async fn win_accessibility_notepad_round_trip() {
    use bricolo_providers::accessibility::Verification;
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let caller = Caller::new("bricolo-test");
    let mut child = std::process::Command::new("notepad.exe").spawn().unwrap();
    let app = until("Notepad in the app list", async || {
        let apps = p.accessibility.list_apps(&caller).await.ok()?;
        apps.into_iter()
            .find(|a| a.name.eq_ignore_ascii_case("notepad"))
            .map(|a| a.name)
    })
    .await;
    // The text control is a document (Windows 11) or an edit (Windows 10).
    let field = until("Notepad's text control", async || {
        for role in ["document", "edit"] {
            let found = p
                .accessibility
                .find(&caller, app.clone(), role.into())
                .await;
            if let Some(e) = found.ok().and_then(|v| v.into_iter().next()) {
                return Some(e);
            }
        }
        None
    })
    .await;
    let text = unique("accessibility é");
    let result = p
        .accessibility
        .set_value(&caller, app.clone(), field.index, text.clone())
        .await
        .unwrap();
    assert_ne!(result.verification, Verification::Mismatch, "{result:?}");
    let state = p
        .accessibility
        .snapshot(&caller, app.clone(), None, Some(text.clone()), None, None)
        .await
        .unwrap();
    assert!(state.tree.contains(&text), "{}", state.tree);
    let _ = child.kill();
    let _ = child.wait();
    p.release("bricolo-test");
}

/// Makes a hidden desktop, starts Notepad on it and ends both.
#[tokio::test]
#[ignore = "needs a Windows desktop session"]
async fn win_virtual_screen_hidden_desktop_runs_notepad() {
    use bricolo_providers::shell::ShellGrant;
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let mut caller = Caller::new("bricolo-test");
    caller.shell = ShellGrant::any();
    let screen = p.virtual_screen.create(&caller, 800, 600).await.unwrap();
    assert!(
        screen.name.starts_with("bricolo-virtual-"),
        "{}",
        screen.name
    );
    let pid = p
        .virtual_screen
        .launch(
            &caller,
            screen.id,
            r"C:\Windows\System32\notepad.exe".into(),
            vec![],
        )
        .await
        .unwrap();
    let listed = p.virtual_screen.list(&caller).await.unwrap();
    assert!(listed[0].pids.contains(&pid), "{listed:?}");
    // Notepad needs a moment to open its window on the hidden desktop.
    std::thread::sleep(std::time::Duration::from_secs(3));
    let before = p.virtual_screen.capture(&caller, screen.id).await.unwrap();
    let png = std::fs::read(&before).unwrap();
    assert_eq!(&png[1..4], b"PNG");
    // Text goes to Notepad's edit control without a click first.
    p.virtual_screen
        .type_text(&caller, screen.id, "hello from bricolo ".repeat(20))
        .await
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let after = p.virtual_screen.capture(&caller, screen.id).await.unwrap();
    assert_ne!(
        std::fs::read(&after).unwrap(),
        png,
        "typing changed nothing in the picture"
    );
    p.virtual_screen
        .click(&caller, screen.id, 20, 20)
        .await
        .unwrap();
    p.virtual_screen
        .press_key(&caller, screen.id, "end".into())
        .await
        .unwrap();
    assert!(
        p.virtual_screen
            .press_key(&caller, screen.id, "ctrl+s".into())
            .await
            .is_err()
    );
    assert!(
        p.virtual_screen
            .click(&caller, screen.id, 5000, 5)
            .await
            .is_err()
    );
    p.virtual_screen.destroy(&caller, screen.id).await.unwrap();
    assert!(p.virtual_screen.list(&caller).await.unwrap().is_empty());
}

/// An RDP-session virtual screen. Needs Windows Server with Remote Desktop
/// on and the account stored as the secrets `virtual-screen-rdp-user` and
/// `virtual-screen-rdp-password` (CI makes a throwaway one); skips
/// itself without them.
#[ignore = "needs Windows Server, Remote Desktop and a stored account"]
#[tokio::test]
async fn win_rdp_virtual_screen_runs_notepad() {
    use bricolo_providers::shell::ShellGrant;
    let dir = tempfile::tempdir().unwrap();
    let (p, _) = providers(dir.path());
    let mut caller = Caller::new("bricolo-test");
    caller.shell = ShellGrant::any();
    let screen = p.virtual_screen.create(&caller, 1024, 768).await.unwrap();
    if !screen.name.starts_with("rdp-session-") {
        eprintln!(
            "no RDP account stored or not Windows Server: got {}",
            screen.name
        );
        p.virtual_screen.destroy(&caller, screen.id).await.unwrap();
        return;
    }
    let pid = p
        .virtual_screen
        .launch(
            &caller,
            screen.id,
            r"C:\Windows\System32\notepad.exe".into(),
            vec![],
        )
        .await
        .unwrap();
    assert!(pid > 0);
    std::thread::sleep(std::time::Duration::from_secs(3));
    p.virtual_screen
        .type_text(&caller, screen.id, "hello from bricolo".into())
        .await
        .unwrap();
    let png = p.virtual_screen.capture(&caller, screen.id).await.unwrap();
    assert_eq!(&std::fs::read(&png).unwrap()[1..4], b"PNG");
    p.virtual_screen.destroy(&caller, screen.id).await.unwrap();
}
