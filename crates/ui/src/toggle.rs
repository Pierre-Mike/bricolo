//! The chord that toggles the hub, cmd+` unless `BRICOLO_UI_CHORD` names
//! another, the way Hammerspoon toggles its console. The hub drops down
//! from the notch on the screen that holds the pointer
//! ([`bricolo_ui::dropdown`]); the chord again, or Esc in the hub, puts it
//! away and gives the front back to the app that had it. A click in
//! another app puts it away too, and that app keeps the front.
//!
//! Open hub in the menu bar drops it down the same way. On macOS the hub
//! has no title bar, no close button, and stays where it drops: the
//! mouse cannot move or resize it. The chord is macOS only; elsewhere
//! the hub is a normal window and Esc hides it.

use tauri::AppHandle;

/// Pins the hub and registers the chord. Runs in `setup`, on the main
/// thread. A chord that another app holds only logs a warning.
pub fn register(handle: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        mac::on_main(handle, mac::pin);
        mac::register(handle);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = handle;
}

/// Shows the hub: from the menu bar, or at start.
pub fn open(handle: &AppHandle) {
    #[cfg(target_os = "macos")]
    mac::on_main(handle, mac::drop_down);
    #[cfg(not(target_os = "macos"))]
    crate::show_hub(handle);
}

/// Hides the hub when another app takes the front, as a click outside it
/// does. A panel of ours taking the keyboard leaves bricolo-ui in front, and
/// the hub stays.
pub fn blurred(handle: &AppHandle) {
    // The check waits a turn of the event loop: the hub may lose the
    // keyboard before AppKit marks bricolo-ui inactive.
    #[cfg(target_os = "macos")]
    {
        let h = handle.clone();
        let _ = handle.run_on_main_thread(move || mac::on_main(&h, mac::put_away));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = handle;
}

/// Hides the hub: Esc, or the chord while the hub has the keyboard.
pub fn hide(handle: &AppHandle) {
    #[cfg(target_os = "macos")]
    mac::on_main(handle, mac::hide);
    #[cfg(not(target_os = "macos"))]
    if let Some(hub) = tauri::Manager::get_webview_window(handle, "hub") {
        let _ = hub.hide();
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::RefCell;
    use std::str::FromStr;

    use bricolo_ui::dropdown::{self, Rect};
    use global_hotkey::hotkey::HotKey;
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationOptions, NSEvent, NSRunningApplication, NSScreen,
        NSWindow, NSWorkspace,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use tauri::{AppHandle, Manager};

    /// Keeps the chord registered while the app runs.
    struct Chord(#[allow(dead_code)] GlobalHotKeyManager);

    thread_local! {
        /// The app in front before the hub showed; it gets the front back
        /// when the hub goes away.
        static BEFORE: RefCell<Option<Retained<NSRunningApplication>>> = const { RefCell::new(None) };
    }

    /// No title bar, so no close, minimise or zoom buttons, and a frame
    /// that only [`drop_down`] sets: the mouse can neither move nor
    /// resize the hub.
    pub fn pin(handle: &AppHandle, window: &NSWindow, _: MainThreadMarker) {
        if let Some(hub) = handle.get_webview_window("hub") {
            let _ = hub.set_decorations(false);
            let _ = hub.set_resizable(false);
            let _ = hub.set_maximizable(false);
            let _ = hub.set_minimizable(false);
            let _ = hub.set_closable(false);
        }
        window.setMovable(false);
        window.setMovableByWindowBackground(false);
    }

    pub fn register(handle: &AppHandle) {
        let var = std::env::var("BRICOLO_UI_CHORD").ok();
        let Some(chord) = dropdown::chord(var.as_deref()) else {
            return;
        };
        let hotkey = match HotKey::from_str(chord) {
            Ok(hotkey) => hotkey,
            Err(e) => {
                eprintln!("bricolo-ui: warning: BRICOLO_UI_CHORD {chord:?}: {e}");
                return;
            }
        };
        let registered = GlobalHotKeyManager::new()
            .and_then(|manager| manager.register(hotkey).map(|_| manager));
        let manager = match registered {
            Ok(manager) => manager,
            Err(e) => {
                eprintln!("bricolo-ui: warning: cannot register {chord} to toggle the hub: {e}");
                return;
            }
        };
        let id = hotkey.id();
        let h = handle.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.id == id && event.state == HotKeyState::Pressed {
                on_main(&h, toggle);
            }
        }));
        handle.manage(Chord(manager));
    }

    /// Runs `f` with the hub's window on the main thread: at once when
    /// called there, as from the menu, so the frame is set before the hub
    /// shows.
    pub fn on_main(handle: &AppHandle, f: fn(&AppHandle, &NSWindow, MainThreadMarker)) {
        let h = handle.clone();
        let run = move || {
            let (Some(mtm), Some(hub)) = (MainThreadMarker::new(), h.get_webview_window("hub"))
            else {
                return;
            };
            let Ok(ptr) = hub.ns_window() else {
                return;
            };
            // SAFETY: Tauri hands out the window's live NSWindow, and this
            // runs on the main thread.
            let window: &NSWindow = unsafe { &*ptr.cast() };
            f(&h, window, mtm);
        };
        if MainThreadMarker::new().is_some() {
            run();
        } else {
            let _ = handle.run_on_main_thread(run);
        }
    }

    fn toggle(handle: &AppHandle, window: &NSWindow, mtm: MainThreadMarker) {
        let active = NSApplication::sharedApplication(mtm).isActive();
        if active && window.isVisible() && window.isKeyWindow() {
            hide(handle, window, mtm);
        } else {
            drop_down(handle, window, mtm);
        }
    }

    pub fn drop_down(handle: &AppHandle, window: &NSWindow, mtm: MainThreadMarker) {
        let screens = NSScreen::screens(mtm);
        let frames: Vec<Rect> = screens.iter().map(|s| rect(s.frame())).collect();
        let mouse = NSEvent::mouseLocation();
        let Some(i) = dropdown::screen_at(&frames, mouse.x, mouse.y) else {
            return;
        };
        let visible = rect(screens.objectAtIndex(i).visibleFrame());
        let f = dropdown::frame(visible, dropdown::SIZE);
        remember_front(mtm);
        window.setFrame_display(
            NSRect::new(NSPoint::new(f.x, f.y), NSSize::new(f.width, f.height)),
            true,
        );
        crate::show_hub(handle);
    }

    pub fn hide(_: &AppHandle, window: &NSWindow, _: MainThreadMarker) {
        window.orderOut(None);
        if let Some(app) = BEFORE.take()
            && !app.isTerminated()
        {
            app.activateWithOptions(NSApplicationActivationOptions::empty());
        }
    }

    /// Hides the hub once bricolo-ui is no longer in front. The front stays
    /// with the app the user clicked.
    pub fn put_away(_: &AppHandle, window: &NSWindow, mtm: MainThreadMarker) {
        if window.isVisible() && !NSApplication::sharedApplication(mtm).isActive() {
            window.orderOut(None);
            BEFORE.take();
        }
    }

    /// Notes the app in front, unless it is bricolo-ui.
    fn remember_front(mtm: MainThreadMarker) {
        if NSApplication::sharedApplication(mtm).isActive() {
            return;
        }
        let ours = NSRunningApplication::currentApplication().processIdentifier();
        let front = NSWorkspace::sharedWorkspace().frontmostApplication();
        if let Some(app) = front.filter(|a| a.processIdentifier() != ours) {
            BEFORE.set(Some(app));
        }
    }

    fn rect(r: NSRect) -> Rect {
        Rect {
            x: r.origin.x,
            y: r.origin.y,
            width: r.size.width,
            height: r.size.height,
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::str::FromStr;

    use global_hotkey::hotkey::{Code, HotKey, Modifiers};

    #[test]
    fn default_chord_is_command_backquote() {
        let hotkey = HotKey::from_str(bricolo_ui::dropdown::CHORD).unwrap();
        assert_eq!(hotkey, HotKey::new(Some(Modifiers::META), Code::Backquote));
    }
}
