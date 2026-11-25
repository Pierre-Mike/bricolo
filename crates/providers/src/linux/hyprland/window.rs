//! `window` and the app list on Hyprland, from `j/clients`, `j/monitors`
//! and `j/activewindow`. Coordinates are Hyprland's layout coordinates:
//! pixels divided by the monitor's scale, from the top-left corner of the
//! layout. `set-frame` floats a tiled window first, since the tiling layout
//! owns a tiled window's frame. `set-minimized` parks a window on a
//! special workspace, since Hyprland has no minimized state.

use super::{HyprError, Hyprland, MINIMIZED, Workspace, cmd};
use crate::app::AppInfo;
use crate::error::CallError;
use crate::window::{Rect, Screen, WindowInfo};

/// Runs blocking IPC off the async threads.
pub async fn blocking<T: Send + 'static>(
    hypr: Hyprland,
    f: impl FnOnce(&Hyprland) -> Result<T, HyprError> + Send + 'static,
) -> Result<T, CallError> {
    tokio::task::spawn_blocking(move || f(&hypr))
        .await
        .map_err(CallError::failed)?
        .map_err(CallError::from)
}

pub fn list(h: &Hyprland) -> Result<Vec<WindowInfo>, HyprError> {
    Ok(h.clients()?
        .iter()
        .filter(|c| c.is_app_window())
        .filter_map(|c| c.info())
        .collect())
}

pub fn focused(h: &Hyprland) -> Result<Option<WindowInfo>, HyprError> {
    Ok(h.active()?.and_then(|c| c.info()))
}

pub fn focus(h: &Hyprland, id: u64) -> Result<(), HyprError> {
    h.client(id)?;
    h.run(&cmd::focus(h.dialect(), id))
}

pub fn set_frame(h: &Hyprland, id: u64, frame: Rect) -> Result<(), HyprError> {
    let client = h.client(id)?;
    if client.is_fullscreen() {
        return Err(HyprError::Refused {
            command: "set-frame".into(),
            reply: "the window is fullscreen".into(),
        });
    }
    h.run(&cmd::set_frame(h.dialect(), id, !client.floating, frame))
}

pub fn close(h: &Hyprland, id: u64) -> Result<(), HyprError> {
    h.client(id)?;
    h.run(&cmd::close(h.dialect(), id))
}

/// Parks the window on the [`MINIMIZED`] special workspace, or brings it
/// back to the active workspace and focuses it. Restoring a window that
/// is not parked focuses it, as the X11 backend does.
pub fn set_minimized(h: &Hyprland, id: u64, minimized: bool) -> Result<(), HyprError> {
    let client = h.client(id)?;
    let d = h.dialect();
    match (minimized, client.is_minimized()) {
        (true, true) => Ok(()),
        (true, false) => h.run(&cmd::move_to(d, id, MINIMIZED, false)),
        (false, true) => {
            let active: Workspace = h.query("activeworkspace")?;
            h.run(&cmd::move_to(d, id, &active.id.to_string(), true))
        }
        (false, false) => h.run(&cmd::focus(d, id)),
    }
}

pub fn screens(h: &Hyprland) -> Result<Vec<Screen>, HyprError> {
    Ok(super::screens(&h.monitors()?))
}

/// One entry per app: its class and process id.
pub fn running(h: &Hyprland) -> Result<Vec<AppInfo>, HyprError> {
    let mut apps: Vec<AppInfo> = Vec::new();
    for c in h.clients()?.iter().filter(|c| c.is_app_window()) {
        if !apps.iter().any(|a| a.pid == c.pid && a.name == c.class) {
            apps.push(app_info(c));
        }
    }
    Ok(apps)
}

pub fn frontmost(h: &Hyprland) -> Result<Option<AppInfo>, HyprError> {
    Ok(h.active()?.as_ref().map(app_info))
}

/// Focuses a window whose class matches one of `names`, any case. False
/// when none is open.
pub fn raise(h: &Hyprland, names: &[String]) -> Result<bool, HyprError> {
    let hit = h
        .clients()?
        .into_iter()
        .filter(|c| c.is_app_window())
        .find(|c| names.iter().any(|n| n.eq_ignore_ascii_case(&c.class)));
    match hit.and_then(|c| c.id()) {
        Some(id) => h.run(&cmd::focus(h.dialect(), id)).map(|()| true),
        None => Ok(false),
    }
}

fn app_info(c: &super::Client) -> AppInfo {
    AppInfo {
        name: c.class.clone(),
        bundle_id: None,
        pid: c.pid.max(0),
    }
}
