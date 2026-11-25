//! `bricolo-ui`: the hub window, the menu bar item and the floating panels, in
//! a process of their own (Decision 8). It follows the node over the
//! control socket and draws what the node reports; a crash here never
//! stops an automation.
//!
//! `bricolo-ui --background` starts with only the menu bar item; the hub opens
//! from there, or drops down from the notch on cmd+` (`toggle.rs`).
//! `--view <name>` picks the hub's first view: home, apps, sidecars,
//! library, mesh or logs. The older names widgets, plugins and scripts
//! still work and open home or apps.

// A release build on Windows is a GUI program: no console window opens
// with the hub. Debug builds keep the console for their logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod panels;
mod toggle;
mod tray;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;

use bricolo_ui::client::{self, NodeEvent};
use bricolo_ui::launch;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, RunEvent, WindowEvent};

/// Log lines the hub keeps for a page that loads late.
const KEEP_LOGS: usize = 2000;

/// What the node last reported.
#[derive(Default)]
struct Model {
    connected: bool,
    /// Why no node answers, while `connected` is false.
    reason: String,
    status: Value,
    ui: Value,
    logs: VecDeque<Value>,
}

pub struct App {
    data: PathBuf,
    socket: PathBuf,
    /// The hub's first view, from `--view`.
    view: Option<String>,
    model: Mutex<Model>,
}

impl App {
    fn status(&self) -> Value {
        self.model.lock().unwrap().status.clone()
    }
}

/// Everything the hub needs to draw itself after a (re)load.
#[tauri::command]
fn snapshot(app: tauri::State<'_, App>) -> Value {
    let m = app.model.lock().unwrap();
    json!({
        "connected": m.connected,
        "reason": m.reason,
        "status": m.status,
        "ui": m.ui,
        "logs": m.logs,
        "can_start": launch::find_bricolo().is_some(),
        "socket": app.socket,
        "view": app.view,
    })
}

/// Forwards one hub request to the node.
#[tauri::command]
async fn request(app: tauri::State<'_, App>, request: Value) -> Result<Value, String> {
    send(&app, request).await
}

async fn send(app: &App, request: Value) -> Result<Value, String> {
    client::allowed(&request).map_err(|e| e["error"].as_str().unwrap_or_default().to_string())?;
    client::request(&app.socket, &request)
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn start_node(app: tauri::State<'_, App>) -> Result<u32, String> {
    start(&app)
}

fn start(app: &App) -> Result<u32, String> {
    let bricolo = launch::find_bricolo()
        .ok_or("no bricolo binary next to bricolo-ui, in the app bundle or on PATH")?;
    let log = app.data.join("logs").join("bricolo-run.log");
    launch::start_node(&bricolo, Some(&log))
        .map_err(|e| format!("cannot start {}: {e}", bricolo.display()))
}

/// The panel a panel window should show, for a page that loads late.
#[tauri::command]
fn panel_current(handle: AppHandle, kind: String) -> Value {
    panels::current(&handle, &kind)
}

/// Hides a choice list or review the user just answered. Not `async`, so
/// it runs on the main thread and the panel is hidden when it returns.
#[tauri::command]
fn dismiss_panel(handle: AppHandle, kind: String) {
    panels::dismiss(&handle, &kind);
}

/// Hides the hub on Esc.
#[tauri::command]
fn hide_hub(handle: AppHandle) {
    toggle::hide(&handle);
}

/// Lets the hub open on the Space in use, full-screen apps included, the
/// way Control Center does. Runs in `setup`, on the main thread.
#[cfg(target_os = "macos")]
fn follow_spaces(hub: &tauri::WebviewWindow) {
    use objc2_app_kit::{NSWindow, NSWindowCollectionBehavior};
    let Ok(ptr) = hub.ns_window() else {
        return;
    };
    // SAFETY: Tauri hands out the window's live NSWindow, and setup runs
    // on the main thread.
    let window: &NSWindow = unsafe { &*ptr.cast() };
    window.setCollectionBehavior(
        window.collectionBehavior()
            | NSWindowCollectionBehavior::MoveToActiveSpace
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
}

fn show_hub(handle: &AppHandle) {
    if let Some(hub) = handle.get_webview_window("hub") {
        let _ = hub.show();
        let _ = hub.unminimize();
        let _ = hub.set_focus();
    }
    // An accessory app is not activated by focusing its window. `activate`
    // needs macOS 14; this older call works on 13 too.
    #[cfg(target_os = "macos")]
    let _ = handle.run_on_main_thread(|| {
        if let Some(mtm) = objc2::MainThreadMarker::new() {
            #[allow(deprecated)]
            objc2_app_kit::NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        }
    });
}

/// Applies one event from the node and passes it to the pages, the tray
/// and the panels.
fn on_node(handle: &AppHandle, event: NodeEvent) {
    let app = handle.state::<App>();
    match event {
        NodeEvent::Up => {
            let mut m = app.model.lock().unwrap();
            m.connected = true;
            m.reason.clear();
            drop(m);
            let _ = handle.emit("node", json!({ "connected": true }));
        }
        NodeEvent::Down(reason) => {
            {
                let mut m = app.model.lock().unwrap();
                m.connected = false;
                m.reason = reason.clone();
                m.status = Value::Null;
                m.ui = Value::Null;
            }
            let _ = handle.emit("node", json!({ "connected": false, "reason": reason }));
            tray::update(handle, false, &Value::Null);
            panels::update(handle, &Value::Null);
        }
        NodeEvent::Event(event) => match event["event"].as_str() {
            Some("status") => {
                let status = event["status"].clone();
                app.model.lock().unwrap().status = status.clone();
                let _ = handle.emit("status", &status);
                tray::update(handle, true, &status);
            }
            Some("ui") => {
                let ui = event["ui"].clone();
                app.model.lock().unwrap().ui = ui.clone();
                let _ = handle.emit("ui", &ui);
                panels::update(handle, &ui);
            }
            Some("logs") => {
                let lines: VecDeque<Value> = event["lines"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into();
                app.model.lock().unwrap().logs = lines.clone();
                let _ = handle.emit("logs", &lines);
            }
            Some("log") => {
                let line = event["line"].clone();
                {
                    let mut m = app.model.lock().unwrap();
                    if m.logs.len() == KEEP_LOGS {
                        m.logs.pop_front();
                    }
                    m.logs.push_back(line.clone());
                }
                let _ = handle.emit("log", &line);
            }
            Some("lagged") => {
                let line = json!({
                    "ts_ms": 0,
                    "plugin": "bricolo-ui",
                    "level": "warn",
                    "message": format!("skipped {} log lines", event["skipped"]),
                });
                let _ = handle.emit("log", &line);
            }
            _ => {}
        },
    }
}

/// Holds `bricolo-ui.lock` in the data folder for as long as the process runs;
/// fails when another hub holds it.
fn one_instance(data: &std::path::Path) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(data).map_err(|e| format!("cannot create {}: {e}", data.display()))?;
    let path = data.join("bricolo-ui.lock");
    let file =
        std::fs::File::create(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(format!(
            "another bricolo-ui is running for {}",
            data.display()
        )),
        Err(std::fs::TryLockError::Error(e)) => Err(format!("cannot lock {}: {e}", path.display())),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let background = args.iter().any(|a| a == "--background");
    let view = args
        .iter()
        .position(|a| a == "--view")
        .and_then(|i| args.get(i + 1).cloned());
    let Some(data) = client::data_dir() else {
        eprintln!("error: cannot find the data folder; set BRICOLO_DATA_DIR");
        std::process::exit(1);
    };
    // One hub per node: a second would draw every panel again, and its
    // review panel would take the keys a plugin sends to the app below.
    let _lock = match one_instance(&data) {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!("bricolo-ui: {e}");
            std::process::exit(0);
        }
    };
    let socket = client::socket(&data);
    let builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    let builder = builder.plugin(tauri_nspanel::init());
    let app = builder
        .manage(App {
            data,
            socket,
            view,
            model: Mutex::default(),
        })
        .manage(panels::Shown::default())
        .manage(tray::Menu::default())
        .invoke_handler(tauri::generate_handler![
            snapshot,
            request,
            start_node,
            panel_current,
            dismiss_panel,
            hide_hub
        ])
        .setup(move |app| {
            // A menu bar app: no Dock icon, and it does not take the front
            // when it starts in the background.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            tray::create(app.handle())?;
            panels::create(app.handle())?;
            #[cfg(target_os = "macos")]
            if let Some(hub) = app.get_webview_window("hub") {
                follow_spaces(&hub);
            }
            toggle::register(app.handle());
            if !background {
                toggle::open(app.handle());
            }
            let handle = app.handle().clone();
            let socket = app.state::<App>().socket.clone();
            tauri::async_runtime::spawn(async move {
                client::follow(&socket, |event| on_node(&handle, event)).await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "hub" {
                return;
            }
            match event {
                // Closing the hub hides it; the menu bar item stays.
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                WindowEvent::Focused(false) => toggle::blurred(window.app_handle()),
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("cannot build the Tauri app");
    app.run(|_, event| {
        // Only "Quit" in the menu ends the app.
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            api.prevent_exit();
        }
    });
}
