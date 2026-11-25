//! The menu bar item: open the hub, a quick on/off per plugin and script,
//! the node's state with a Start button when it is down, and the
//! recording dot while a plugin uses the microphone, camera or screen.

use std::sync::Mutex;

use serde_json::{Value, json};
use tauri::image::Image;
use tauri::menu::{
    CheckMenuItemBuilder, Menu as TauriMenu, MenuBuilder, MenuItemBuilder, SubmenuBuilder,
};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

use crate::App;

const TRAY: &str = "main";

/// The last menu built, so a status that changes nothing the menu shows
/// does not rebuild it.
#[derive(Default)]
pub struct Menu(Mutex<String>);

/// `(name, state)` for each plugin or script in `list`.
fn entries(list: &Value) -> Vec<(String, String)> {
    list.as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            (
                p["name"].as_str().unwrap_or("?").to_string(),
                p["state"].as_str().unwrap_or("?").to_string(),
            )
        })
        .collect()
}

/// "caps (mic), dict.lua (screen)" for the active sensors.
pub fn recording(status: &Value) -> Option<String> {
    let active: Vec<String> = status["active_sensors"]
        .as_array()?
        .iter()
        .map(|s| {
            format!(
                "{} ({})",
                s["plugin"].as_str().unwrap_or("?"),
                s["sensor"].as_str().unwrap_or("?")
            )
        })
        .collect();
    (!active.is_empty()).then(|| active.join(", "))
}

fn build(handle: &AppHandle, connected: bool, status: &Value) -> tauri::Result<TauriMenu<Wry>> {
    let mut menu =
        MenuBuilder::new(handle).item(&MenuItemBuilder::with_id("open", "Open hub").build(handle)?);
    if let Some(who) = recording(status) {
        menu = menu.separator().item(
            &MenuItemBuilder::with_id("recording", format!("● Recording: {who}"))
                .enabled(false)
                .build(handle)?,
        );
    }
    menu = menu.separator();
    if !connected {
        menu = menu.item(
            &MenuItemBuilder::with_id("state", "Node not running")
                .enabled(false)
                .build(handle)?,
        );
        if bricolo_ui::launch::find_bricolo().is_some() {
            menu = menu.item(&MenuItemBuilder::with_id("start", "Start node").build(handle)?);
        }
    } else {
        let name = status["node"].as_str().unwrap_or("node");
        menu = menu.item(
            &MenuItemBuilder::with_id("state", format!("Node {name} running"))
                .enabled(false)
                .build(handle)?,
        );
        let plugins = entries(&status["plugins"]);
        if !plugins.is_empty() {
            menu = menu.separator();
            for (name, state) in plugins {
                menu = menu.item(
                    &CheckMenuItemBuilder::with_id(format!("plugin:{name}"), &name)
                        .checked(state != "disabled")
                        .build(handle)?,
                );
            }
        }
        let scripts = entries(&status["scripts"]);
        if !scripts.is_empty() {
            let mut sub = SubmenuBuilder::new(handle, "Scripts");
            for (name, state) in scripts {
                sub = sub.item(
                    &CheckMenuItemBuilder::with_id(format!("script:{name}"), &name)
                        .checked(state != "disabled")
                        .build(handle)?,
                );
            }
            menu = menu.item(&sub.build()?);
        }
    }
    menu.separator()
        .item(&MenuItemBuilder::with_id("quit", "Quit bricolo-ui").build(handle)?)
        .build()
}

pub fn create(handle: &AppHandle) -> tauri::Result<()> {
    let icon = Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    let menu = build(handle, false, &Value::Null)?;
    TrayIconBuilder::with_id(TRAY)
        .icon(icon)
        .icon_as_template(true)
        .tooltip("bricolo")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|handle, event| on_menu(handle, event.id().as_ref()))
        .build(handle)?;
    Ok(())
}

/// Rebuilds the menu and the title when what they show changed.
pub fn update(handle: &AppHandle, connected: bool, status: &Value) {
    let rec = recording(status);
    let key = json!([
        connected,
        status["node"],
        entries(&status["plugins"]),
        entries(&status["scripts"]),
        rec,
    ])
    .to_string();
    let menu_state = handle.state::<Menu>();
    {
        let mut last = menu_state.0.lock().unwrap();
        if *last == key {
            return;
        }
        *last = key;
    }
    let Some(tray) = handle.tray_by_id(TRAY) else {
        return;
    };
    match build(handle, connected, status) {
        Ok(menu) => {
            let _ = tray.set_menu(Some(menu));
        }
        Err(e) => eprintln!("bricolo-ui: cannot build the tray menu: {e}"),
    }
    // The dot next to the icon while something records.
    let title = rec.as_ref().map(|_| "●");
    let _ = tray.set_title(title);
    let tooltip = match rec {
        Some(who) => format!("bricolo: recording, {who}"),
        None => "bricolo".to_string(),
    };
    let _ = tray.set_tooltip(Some(tooltip));
}

fn on_menu(handle: &AppHandle, id: &str) {
    match id {
        "open" => crate::toggle::open(handle),
        "quit" => handle.exit(0),
        "start" => {
            if let Err(e) = crate::start(&handle.state::<App>()) {
                eprintln!("bricolo-ui: {e}");
            }
        }
        _ => {
            let Some((kind, name)) = id.split_once(':') else {
                return;
            };
            let list = match kind {
                "plugin" => "plugins",
                "script" => "scripts",
                _ => return,
            };
            let status = handle.state::<App>().status();
            let disabled = entries(&status[list])
                .iter()
                .any(|(n, s)| n == name && s == "disabled");
            let cmd = if disabled { "enable" } else { "disable" };
            let request = json!({ "cmd": cmd, kind: name });
            let handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = crate::send(&handle.state::<App>(), request).await {
                    eprintln!("bricolo-ui: {e}");
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_lists_each_plugin_and_sensor() {
        assert_eq!(recording(&Value::Null), None);
        assert_eq!(recording(&json!({ "active_sensors": [] })), None);
        let status = json!({ "active_sensors": [
            { "plugin": "caps", "sensor": "mic" },
            { "plugin": "dict.lua", "sensor": "screen" },
        ] });
        assert_eq!(
            recording(&status).as_deref(),
            Some("caps (mic), dict.lua (screen)")
        );
    }
}
