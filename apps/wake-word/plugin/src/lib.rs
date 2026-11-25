//! wake-word: say "Codec" with no call open and the voice agent starts
//! one. The tile's switch opens and closes the microphone; it starts off.
//!
//! The listening runs in the wake-word sidecar (port 8803), which records
//! from [mic] device in node.toml. This plugin owns the switch, remembers
//! it in kv, and shows what the sidecar is doing.
//!
//! A port of the wake_word.lua script. `domain.rs` holds the logic as
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

/// The last status line drawn.
static LAST: Mutex<Option<String>> = Mutex::new(None);

struct Plugin;

fn settings() -> domain::Settings {
    SETTINGS.get().cloned().unwrap_or_default()
}

fn enabled() -> bool {
    domain::enabled(kv::get("enabled").as_deref())
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

/// Calls the sidecar and reads its answer as JSON.
async fn call(method: &str, path: &str, timeout_ms: u32) -> Result<Value, String> {
    let request = http::Request {
        method: method.into(),
        url: format!("{}{path}", settings().url),
        headers: vec![],
        body: vec![],
        timeout_ms: Some(timeout_ms),
    };
    match http::send(request).await {
        Ok(r) => domain::json_of(r.status, &r.body),
        Err(e) => Err(domain::not_answering(&error_name(&e))),
    }
}

/// Brings the sidecar in line with the switch: after a sidecar restart it
/// comes back disabled.
async fn sync(s: Option<Value>) -> Option<Value> {
    let path = domain::sync_path(s.as_ref(), enabled())?;
    match call("POST", path, settings().timeout_ms).await {
        Ok(doc) => Some(doc),
        Err(_) => s,
    }
}

async fn poll(force: bool) {
    let status = call("GET", "/status", settings().status_timeout_ms).await;
    let s = sync(status.ok()).await;
    let line = domain::describe(s.as_ref());
    let changed = {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let changed = last.as_deref() != Some(line.as_str());
        if changed || force {
            *last = Some(line.clone());
        }
        changed
    };
    if changed || force {
        draw(line);
    }
}

fn draw(line: String) {
    let tile = ui::Tile {
        title: "Wake word".into(),
        status_text: line,
        switches: vec![ui::Switch {
            id: "on".into(),
            label: "Listen for \"Codec\"".into(),
            on: enabled(),
        }],
        buttons: vec![],
    };
    if let Err(e) = ui::set_tile(&tile) {
        log::write(Level::Warn, &format!("cannot set the tile: {e:?}"));
    }
}

impl Guest for Plugin {
    /// `config` is this plugin's settings as JSON.
    async fn init(config: String) -> Result<(), String> {
        let settings = SETTINGS.get_or_init(|| domain::Settings::from_json(&config));
        poll(true).await;
        timer::every(settings.poll_ms);
        let state = if enabled() { "listening" } else { "off" };
        log::write(Level::Info, &format!("wake_word: {state}"));
        Ok(())
    }

    async fn on_event(event: Event) {
        match event {
            Event::Timer(_) => poll(false).await,
            Event::Ui(UiEvent::Switch(s)) => {
                if let Err(e) = kv::set("enabled", domain::kv_value(s.on)) {
                    log::write(Level::Warn, &format!("cannot save enabled: {e:?}"));
                }
                log::write(Level::Info, &format!("switched {}", domain::kv_value(s.on)));
                poll(true).await;
            }
            _ => {}
        }
    }

    async fn dispose() {}
}

export!(Plugin);
