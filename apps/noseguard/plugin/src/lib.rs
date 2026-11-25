//! noseguard: NoseGuard in a hub tile. While Watching is on, the camera
//! looks for a fingertip resting on your nose and answers each touch with
//! a red flash over the whole screen, an alarm and a count.
//!
//! The camera daemon runs in the noseguard sidecar (port 8802), which also
//! draws the flash, plays the alarm and counts. This plugin owns the tile
//! and the settings, kept in kv, and polls the sidecar for the count.
//! Watching starts off, and stopping or reloading the plugin turns it off,
//! so the camera light never outlives the tile.
//!
//! A port of the noseguard.lua script. `domain.rs` holds the logic as
//! plain functions, tested with `cargo test`. This file is the adapter: it
//! calls the capabilities and passes their results to the domain.
//!
//! Build:   cargo build --target wasm32-wasip2 --release
//! Check:   bricolo check .
//! Install: bricolo install . -y

// The generated `event` bindings take more than seven arguments.
#![allow(clippy::too_many_arguments)]

mod domain;

use std::sync::{Mutex, OnceLock};

wit_bindgen::generate!({
    path: "wit",
    world: "bricolo:stdlib/plugin",
});

use bricolo::stdlib::log::{self, Level};
use bricolo::stdlib::types::{CallError, UiEvent};
use bricolo::stdlib::{http, kv, timer, ui};
use serde_json::Value;

static SETTINGS: OnceLock<domain::Settings> = OnceLock::new();

/// What the script kept in locals between events.
#[derive(Default)]
struct State {
    /// The sidecar's last status, or `None` while it doesn't answer.
    status: Option<domain::Status>,
    /// Why the sidecar didn't answer.
    down: Option<String>,
    /// The last daemon error already shown in a toast.
    seen_error: Option<String>,
    /// The tile last drawn, to redraw only on a change.
    drawn: Option<domain::Tile>,
    /// The open camera list: its panel id, and what each entry saves.
    picking: Option<(String, Vec<domain::Choice>)>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Runs `f` on the state. Never call a capability that waits inside it.
fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(State::default))
}

fn settings() -> domain::Settings {
    SETTINGS.get().cloned().unwrap_or_default()
}

fn prefs() -> domain::Prefs {
    domain::Prefs::read(
        kv::get("camera_id"),
        kv::get("camera_name"),
        kv::get("sensitivity"),
        kv::get("hold"),
        kv::get("debug"),
    )
}

fn save(key: &str, value: &str) {
    if let Err(e) = kv::set(key, value) {
        log::write(Level::Warn, &format!("cannot save {key}: {e:?}"));
    }
}

/// The error text a script saw for a call error.
fn error_name(e: &CallError) -> String {
    match e {
        CallError::Unavailable => "unavailable".into(),
        CallError::Timeout => "timeout".into(),
        CallError::Denied => "denied".into(),
        CallError::Failed(msg) => format!("failed: {msg}"),
    }
}

/// Calls the sidecar and reads its answer as JSON. A `body` goes as JSON.
async fn call(
    method: &str,
    path: &str,
    body: Option<String>,
    timeout_ms: Option<u32>,
) -> Result<Value, String> {
    let s = settings();
    let headers = match body {
        Some(_) => vec![("content-type".into(), "application/json".into())],
        None => vec![],
    };
    let request = http::Request {
        method: method.into(),
        url: format!("{}{path}", s.url),
        headers,
        body: body.unwrap_or_default().into_bytes(),
        timeout_ms: Some(timeout_ms.unwrap_or(s.timeout_ms)),
    };
    match http::send(request).await {
        Ok(r) => domain::json_of(r.status, &r.body),
        Err(e) => Err(domain::not_answering(&error_name(&e))),
    }
}

/// Logs "what: err" as a warning and shows it in a toast.
fn report(what: &str, err: &str) {
    let line = format!("{what}: {err}");
    log::write(Level::Warn, &line);
    let _ = ui::show_panel(&ui::Panel {
        kind: ui::PanelKind::Toast,
        title: String::new(),
        text: format!("NoseGuard: {line}"),
        choices: Vec::new(),
        timeout_ms: None,
    });
}

fn watching() -> bool {
    with(|s| s.status.as_ref().is_some_and(|st| st.watching))
}

async fn start() {
    match call("POST", "/start", Some(prefs().start_body()), None).await {
        Ok(v) => with(|s| {
            s.status = Some(domain::Status::from_value(&v));
            s.down = None;
            s.seen_error = None;
        }),
        Err(e) => report("cannot start watching", &e),
    }
}

async fn stop() {
    match call("POST", "/stop", None, None).await {
        Ok(v) => with(|s| {
            s.status = Some(domain::Status::from_value(&v));
            s.down = None;
        }),
        Err(e) => report("cannot stop watching", &e),
    }
}

/// A changed setting restarts the daemon when it runs, as in Hammerspoon.
async fn apply() {
    if watching() {
        start().await;
    }
    draw(true);
}

async fn poll(quiet: bool) {
    let timeout = settings().status_timeout_ms;
    match call("GET", "/status", None, Some(timeout)).await {
        Err(err) => {
            let warn = with(|s| s.status.is_some() || s.down.is_none());
            if warn {
                log::write(Level::Warn, &format!("noseguard sidecar: {err}"));
            }
            with(|s| {
                s.status = None;
                s.down = Some(err.trim_start_matches("sidecar not answering: ").into());
            });
        }
        Ok(v) => {
            let st = domain::Status::from_value(&v);
            let (was_down, fresh) = with(|s| {
                let was_down = s.down.is_some();
                s.status = Some(st.clone());
                s.down = None;
                let fresh = st
                    .last_error
                    .clone()
                    .filter(|e| s.seen_error.as_ref() != Some(e));
                if fresh.is_some() {
                    s.seen_error = fresh.clone();
                }
                (was_down, fresh)
            });
            if was_down {
                log::write(Level::Info, "noseguard sidecar answers again");
            }
            if let (Some(e), false) = (fresh, quiet) {
                report("stopped", &e);
            }
        }
    }
    draw(false);
}

async fn pick_camera() {
    let timeout = settings().cameras_timeout_ms;
    let cams = match call("GET", "/cameras", None, Some(timeout)).await {
        Ok(v) => v,
        Err(e) => return report("cannot list cameras", &e),
    };
    let p = prefs();
    let choices = domain::camera_choices(&cams, &p.camera_id, &p.camera_name);
    let panel = ui::Panel {
        kind: ui::PanelKind::ChoiceList,
        title: "NoseGuard camera".into(),
        text: String::new(),
        choices: choices.iter().map(|c| c.label.clone()).collect(),
        timeout_ms: None,
    };
    match ui::show_panel(&panel) {
        Ok(id) => with(|s| s.picking = Some((id, choices))),
        Err(e) => log::write(Level::Warn, &format!("cannot show the cameras: {e:?}")),
    }
}

async fn on_choice(panel: String, index: Option<u32>) {
    let Some(choices) = with(|s| match &s.picking {
        Some((id, _)) if *id == panel => s.picking.take().map(|(_, c)| c),
        _ => None,
    }) else {
        return;
    };
    let Some(c) = index.and_then(|i| choices.get(i as usize)) else {
        return;
    };
    if c.id == prefs().camera_id {
        return;
    }
    save("camera_id", &c.id);
    save("camera_name", &c.name);
    log::write(
        Level::Info,
        &format!("camera: {}", domain::camera_label(&c.id, &c.name)),
    );
    apply().await;
}

async fn on_button(id: &str) {
    let p = prefs();
    match id {
        "camera" => return pick_camera().await,
        "zone" => save("sensitivity", &p.next_sensitivity()),
        "hold" => save("hold", &p.next_hold()),
        "debug" => save("debug", p.next_debug()),
        "flash" => {
            if let Err(e) = call("POST", "/flash", None, None).await {
                report("cannot flash", &e);
            }
            return;
        }
        "reset" => {
            match call("POST", "/reset", None, None).await {
                Ok(v) => with(|s| s.status = Some(domain::Status::from_value(&v))),
                Err(e) => report("cannot reset the count", &e),
            }
            draw(true);
            return;
        }
        _ => {}
    }
    apply().await;
}

fn draw(force: bool) {
    let status = with(|s| s.status.clone());
    let tile = domain::tile(status.as_ref(), &prefs(), &settings().url);
    let same = with(|s| {
        let same = s.drawn.as_ref() == Some(&tile);
        s.drawn = Some(tile.clone());
        same
    });
    if same && !force {
        return;
    }
    let wit_tile = ui::Tile {
        title: tile.title,
        status_text: tile.status,
        switches: vec![ui::Switch {
            id: "watching".into(),
            label: "Watching".into(),
            on: tile.watching,
        }],
        buttons: tile
            .buttons
            .into_iter()
            .map(|(id, label)| ui::Button {
                id: id.into(),
                label,
            })
            .collect(),
    };
    if let Err(e) = ui::set_tile(&wit_tile) {
        log::write(Level::Warn, &format!("cannot set the tile: {e:?}"));
    }
}

struct Plugin;

impl Guest for Plugin {
    /// `config` is this plugin's settings as JSON.
    async fn init(config: String) -> Result<(), String> {
        let settings = SETTINGS.get_or_init(|| domain::Settings::from_json(&config));
        poll(true).await;
        // Watching starts off: a daemon left from before a reload is stopped.
        if watching() {
            stop().await;
        }
        draw(true);
        timer::every(settings.poll_ms);
        let p = prefs();
        let answers = with(|s| s.status.is_some());
        log::write(
            Level::Info,
            &format!(
                "noseguard: tile ready, camera {}{}",
                domain::camera_label(&p.camera_id, &p.camera_name),
                if answers {
                    ""
                } else {
                    ", sidecar not answering"
                }
            ),
        );
        Ok(())
    }

    async fn on_event(event: Event) {
        match event {
            Event::Timer(_) => poll(false).await,
            Event::Ui(UiEvent::Switch(s)) if s.id == "watching" => {
                if s.on {
                    start().await;
                } else {
                    stop().await;
                }
                draw(true);
            }
            Event::Ui(UiEvent::Button(id)) => on_button(&id).await,
            Event::Ui(UiEvent::Choice(c)) => on_choice(c.panel, c.index).await,
            _ => {}
        }
    }

    async fn dispose() {
        if watching() {
            stop().await;
        }
    }
}

export!(Plugin);
