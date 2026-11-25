//! The floating panels: one window per kind, made at startup and hidden.
//! When the node's UI state changes, each window shows the newest panel of
//! its kind, or hides.
//!
//! On macOS each window becomes a non-activating `NSPanel`
//! (`tauri-nspanel`): it floats above other windows and on every space,
//! and showing it never takes focus from the app in front. Captions and
//! toasts let clicks through; a choice list or a review panel becomes the
//! key window, without activating this app, so it gets the keyboard.
//! Elsewhere the windows are undecorated, always on top, and created
//! without focus.

use std::collections::HashMap;
use std::sync::Mutex;

#[cfg(not(target_os = "macos"))]
use bricolo_ui::placement;
use bricolo_ui::placement::size;
use serde_json::{Value, json};
#[cfg(not(target_os = "macos"))]
use tauri::PhysicalPosition;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

pub const KINDS: [&str; 4] = ["caption", "toast", "choice-list", "review"];

/// The panel each kind shows, as `(id#seq, panel)`.
#[derive(Default)]
pub struct Shown(Mutex<HashMap<&'static str, (String, Value)>>);

fn label(kind: &str) -> String {
    format!("panel-{kind}")
}

/// Whether the panel needs the keyboard.
fn keyed(kind: &str) -> bool {
    matches!(kind, "choice-list" | "review")
}

pub fn create(handle: &AppHandle) -> tauri::Result<()> {
    for kind in KINDS {
        let (width, height) = size(kind);
        let window =
            WebviewWindowBuilder::new(handle, label(kind), WebviewUrl::App("panel.html".into()))
                .title("bricolo panel")
                .inner_size(width, height)
                .visible(false)
                .focused(false)
                .decorations(false)
                .transparent(true)
                .always_on_top(true)
                .skip_taskbar(true)
                .resizable(false)
                .shadow(false)
                .visible_on_all_workspaces(true)
                .build()?;
        #[cfg(target_os = "macos")]
        mac::make_panel(&window, keyed(kind))?;
        #[cfg(not(target_os = "macos"))]
        let _ = window;
    }
    Ok(())
}

/// The newest panel of each kind in the node's `ui` state.
fn newest(ui: &Value) -> HashMap<&'static str, Value> {
    let mut out: HashMap<&'static str, Value> = HashMap::new();
    for panel in ui["panels"].as_array().into_iter().flatten() {
        let Some(kind) = KINDS.iter().find(|k| panel["kind"] == **k) else {
            continue;
        };
        let newer = out
            .get(kind)
            .is_none_or(|p| p["seq"].as_u64() < panel["seq"].as_u64());
        if newer {
            out.insert(kind, panel.clone());
        }
    }
    out
}

/// Shows, updates or hides each panel window to match `ui`.
pub fn update(handle: &AppHandle, ui: &Value) {
    let mut newest = newest(ui);
    let shown = handle.state::<Shown>();
    for kind in KINDS {
        let panel = newest.remove(kind);
        let key = panel
            .as_ref()
            .map(|p| format!("{}#{}", p["id"].as_str().unwrap_or(""), p["seq"]));
        {
            let mut shown = shown.0.lock().unwrap();
            let was = shown.get(kind).map(|(k, _)| k.clone());
            if was == key {
                continue;
            }
            match (&key, &panel) {
                (Some(k), Some(p)) => {
                    shown.insert(kind, (k.clone(), p.clone()));
                }
                _ => {
                    shown.remove(kind);
                }
            }
        }
        // Tauri's page-side `listen` hears events sent to any window, so
        // the event names its kind and each panel page keeps its own.
        let shows = panel.is_some();
        let _ = handle.emit("panel", json!({ "kind": kind, "panel": panel }));
        if shows {
            show(handle, kind);
        } else {
            hide(handle, kind);
        }
    }
}

/// What the `kind` window shows now, or null.
pub fn current(handle: &AppHandle, kind: &str) -> Value {
    let shown = handle.state::<Shown>();
    let shown = shown.0.lock().unwrap();
    KINDS
        .iter()
        .find(|k| **k == kind)
        .and_then(|k| shown.get(k))
        .map(|(_, p)| p.clone())
        .unwrap_or(Value::Null)
}

/// Puts the panel on the screen the user works on (see
/// [`placement`]). Away from macOS there is no portable way to find the
/// focused window's screen, so the screen with the pointer stands in for
/// it, else the primary screen.
#[cfg(not(target_os = "macos"))]
fn place(handle: &AppHandle, kind: &str) {
    let Some(window) = handle.get_webview_window(&label(kind)) else {
        return;
    };
    let pointer = window
        .cursor_position()
        .ok()
        .and_then(|p| window.monitor_from_point(p.x, p.y).ok().flatten());
    let Some(monitor) = pointer.or_else(|| window.primary_monitor().ok().flatten()) else {
        return;
    };
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let h = size(kind).1 * scale;
    let (left, top) = (f64::from(area.position.x), f64::from(area.position.y));
    let (aw, ah) = (f64::from(area.size.width), f64::from(area.size.height));
    // `frame` measures y upwards from the bottom; the window's position is
    // its top-left corner, measured downwards.
    let visible = bricolo_ui::dropdown::Rect {
        x: 0.0,
        y: 0.0,
        width: aw / scale,
        height: ah / scale,
    };
    let f = placement::frame(kind, visible);
    let x = left + f.x * scale;
    let y = top + ah - (f.y * scale) - h;
    let _ = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
}

/// Hides a keyed panel the moment the user answers it, before the answer
/// reaches the node: its key window must be gone by the time the plugin
/// acts, or a paste the plugin sends lands in the panel instead of the app
/// below. The node then clears the panel, which hides it again.
pub fn dismiss(handle: &AppHandle, kind: &str) {
    if let Some(kind) = KINDS.iter().find(|k| **k == kind && keyed(k)) {
        hide(handle, kind);
    }
}

fn show(handle: &AppHandle, kind: &'static str) {
    #[cfg(target_os = "macos")]
    mac::show(handle, kind, &label(kind), keyed(kind));
    #[cfg(not(target_os = "macos"))]
    place(handle, kind);
    #[cfg(not(target_os = "macos"))]
    if let Some(window) = handle.get_webview_window(&label(kind)) {
        let _ = window.show();
        if keyed(kind) {
            let _ = window.set_focus();
        }
    }
}

fn hide(handle: &AppHandle, kind: &'static str) {
    #[cfg(target_os = "macos")]
    mac::hide(handle, &label(kind));
    #[cfg(not(target_os = "macos"))]
    if let Some(window) = handle.get_webview_window(&label(kind)) {
        let _ = window.hide();
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use bricolo_ui::dropdown::Rect;
    use bricolo_ui::placement;
    use objc2_app_kit as ak;
    use objc2_foundation as fd;
    use tauri::{AppHandle, Manager, WebviewWindow};
    use tauri_nspanel::{
        CollectionBehavior, ManagerExt, PanelLevel, StyleMask, WebviewWindowExt, tauri_panel,
    };

    tauri_panel! {
        panel!(RhPanel {
            config: {
                can_become_key_window: true,
                can_become_main_window: false,
                is_floating_panel: true
            }
        })
    }

    /// Turns `window` into a non-activating panel above other windows.
    /// Runs in `setup`, on the main thread.
    pub fn make_panel(window: &WebviewWindow, keyed: bool) -> tauri::Result<()> {
        let panel = window.to_panel::<RhPanel>()?;
        panel.set_level(PanelLevel::Status.value());
        if let Err(e) = panel.add_style_mask(StyleMask::empty().nonactivating_panel().value()) {
            eprintln!("bricolo-ui: {e}");
        }
        panel.set_collection_behavior(
            CollectionBehavior::new()
                .can_join_all_spaces()
                .full_screen_auxiliary()
                .value(),
        );
        panel.set_hides_on_deactivate(false);
        panel.set_becomes_key_only_if_needed(true);
        panel.set_has_shadow(false);
        panel.set_transparent(true);
        // Captions and toasts let clicks through to the app below.
        panel.set_ignores_mouse_events(!keyed);
        Ok(())
    }

    /// Panel calls must run on the main thread.
    fn run(
        handle: &AppHandle,
        label: &str,
        f: impl FnOnce(&dyn tauri_nspanel::Panel) + Send + 'static,
    ) {
        let h = handle.clone();
        let label = label.to_string();
        let _ = handle.run_on_main_thread(move || {
            if let Ok(panel) = h.get_webview_panel(&label) {
                f(&*panel);
            }
        });
    }

    /// Moves the panel to the screen with the focused window, then shows
    /// it; a keyed one becomes the key window.
    pub fn show(handle: &AppHandle, kind: &'static str, label: &str, keyed: bool) {
        let window = handle.get_webview_window(label);
        run(handle, label, move |panel| {
            if let Some(window) = window {
                place(&window, kind);
            }
            if keyed {
                panel.show_and_make_key();
            } else {
                panel.show();
            }
        });
    }

    pub fn hide(handle: &AppHandle, label: &str) {
        run(handle, label, |panel| panel.hide());
    }

    /// Sets the panel's frame on the screen the user works on. AppKit's
    /// main screen is the one holding the window with keyboard focus,
    /// whichever app owns it; the screen with the pointer stands in when
    /// there is none. Runs on the main thread.
    fn place(window: &WebviewWindow, kind: &str) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Ok(ptr) = window.ns_window() else {
            return;
        };
        // SAFETY: Tauri hands out the window's live NSWindow, and this
        // runs on the main thread.
        let ns_window: &ak::NSWindow = unsafe { &*ptr.cast() };
        let screens = ak::NSScreen::screens(mtm);
        let frames: Vec<Rect> = screens.iter().map(|s| rect(s.frame())).collect();
        let focused = ak::NSScreen::mainScreen(mtm).map(|s| rect(s.frame()));
        let mouse = ak::NSEvent::mouseLocation();
        let Some(i) = placement::screen(&frames, focused, (mouse.x, mouse.y)) else {
            return;
        };
        let visible = rect(screens.objectAtIndex(i).visibleFrame());
        let f = placement::frame(kind, visible);
        ns_window.setFrame_display(
            fd::NSRect::new(
                fd::NSPoint::new(f.x, f.y),
                fd::NSSize::new(f.width, f.height),
            ),
            false,
        );
    }

    fn rect(r: fd::NSRect) -> Rect {
        Rect {
            x: r.origin.x,
            y: r.origin.y,
            width: r.size.width,
            height: r.size.height,
        }
    }
}
