//! tts: the speech queue's tile, and a chord that reads the selection
//! aloud. ctrl+alt+shift+s copies what is selected, speaks it in the
//! "reading" voice, and puts the clipboard back.
//!
//! A port of the tts.lua script. `domain.rs` holds the logic as plain
//! functions, tested with `cargo test`. This file is the adapter: it calls
//! the capabilities and passes their results to the domain.
//!
//! Build:   cargo build --target wasm32-wasip2 --release
//! Check:   bricolo check .
//! Install: bricolo install . -y
//! Try it:  bricolo fire tts h1

// The generated `event` bindings take more than seven arguments.
#![allow(clippy::too_many_arguments)]

mod domain;

use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

wit_bindgen::generate!({
    path: "wit",
    world: "bricolo:stdlib/plugin",
});

use bricolo::stdlib::log::{self, Level};
use bricolo::stdlib::types::{CallError, UiEvent};
use bricolo::stdlib::{clipboard, hotkey, keyboard, timer, tts, ui};

/// The binding id: the one the script's first `bricolo.hotkey.bind` got, so
/// `bricolo fire tts h1` still works.
const SELECTION: &str = "h1";

static SETTINGS: OnceLock<domain::Settings> = OnceLock::new();
/// The last status line drawn, so an idle queue doesn't redraw.
static LAST: Mutex<Option<String>> = Mutex::new(None);

struct Plugin;

fn settings() -> &'static domain::Settings {
    SETTINGS.get_or_init(|| domain::Settings::from_json("{}"))
}

/// The error text the script showed: "unavailable", "failed: why".
fn err(e: &CallError) -> String {
    match e {
        CallError::Unavailable => domain::error_name("unavailable", None),
        CallError::Timeout => domain::error_name("timeout", None),
        CallError::Denied => domain::error_name("denied", None),
        CallError::Failed(m) => domain::error_name("failed", Some(m)),
    }
}

fn toast(text: &str) {
    let panel = ui::Panel {
        kind: ui::PanelKind::Toast,
        title: String::new(),
        text: text.into(),
        choices: vec![],
        timeout_ms: None,
    };
    if let Err(e) = ui::show_panel(&panel) {
        log::write(Level::Warn, &format!("cannot show a toast: {}", err(&e)));
    }
}

async fn clipboard_text() -> Option<String> {
    clipboard::get().await.ok().flatten()
}

async fn speak(text: Option<String>, voice: Option<String>) {
    let Some(text) = text.filter(|t| domain::readable(Some(t))) else {
        toast("Nothing to read");
        return;
    };
    if let Err(e) = tts::speak(text, voice).await {
        toast(&format!("Cannot speak: {}", err(&e)));
    }
}

/// Copies the selection with cmd+c, waits for the clipboard to change, and
/// restores the old text after.
async fn speak_selection() {
    let before = clipboard_text().await;
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let marker = domain::marker(secs);
    let _ = clipboard::set(marker.clone()).await;
    if let Err(e) = keyboard::keystroke(vec!["cmd".into()], "c".into()).await {
        toast(&format!("Cannot copy the selection: {}", err(&e)));
        return;
    }
    let mut text = None;
    for _ in 0..15 {
        timer::sleep(30).await;
        text = clipboard_text().await;
        if domain::copied(text.as_deref(), &marker) {
            break;
        }
    }
    if let Some(before) = before {
        let _ = clipboard::set(before).await;
    }
    if !domain::copied(text.as_deref(), &marker) {
        toast("Nothing selected");
        return;
    }
    speak(text, Some(settings().selection_voice.clone())).await;
}

async fn draw(force: bool) {
    let status = tts::status().await.ok();
    let line = domain::describe(
        status
            .as_ref()
            .map(|s| domain::Status {
                speaking: s.speaking,
                queued: s.queued,
                voice: s.voice.as_deref(),
            })
            .as_ref(),
    );
    {
        let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
        if last.as_deref() == Some(line.as_str()) && !force {
            return;
        }
        *last = Some(line.clone());
    }
    let button = |id: &str, label: String| ui::Button {
        id: id.into(),
        label,
    };
    let tile = ui::Tile {
        title: "Speech queue".into(),
        status_text: line,
        switches: vec![],
        buttons: vec![
            button("stop", "Stop".into()),
            button("selection", domain::selection_label(&settings().chord)),
            button("clipboard", "Speak the clipboard".into()),
        ],
    };
    if let Err(e) = ui::set_tile(&tile) {
        log::write(Level::Warn, &format!("cannot set the tile: {}", err(&e)));
    }
}

impl Guest for Plugin {
    /// `config` is this plugin's settings as JSON.
    async fn init(config: String) -> Result<(), String> {
        let settings = SETTINGS.get_or_init(|| domain::Settings::from_json(&config));
        // A chord another program holds fails to bind. Log it and keep
        // running, so the tile and `bricolo fire` still work.
        if let Err(e) = hotkey::bind(SELECTION.into(), settings.chord.clone()).await {
            log::write(
                Level::Warn,
                &format!("tts: cannot bind {}: {}", settings.chord, err(&e)),
            );
        }
        draw(true).await;
        timer::every(settings.poll_ms);
        log::write(
            Level::Info,
            &format!("tts: press {} to read the selection aloud", settings.chord),
        );
        Ok(())
    }

    async fn on_event(event: Event) {
        match event {
            Event::Hotkey(id) if id == SELECTION => speak_selection().await,
            Event::Timer(_) => draw(false).await,
            Event::Ui(UiEvent::Button(id)) => {
                match id.as_str() {
                    "stop" => {
                        let _ = tts::stop().await;
                    }
                    "selection" => speak_selection().await,
                    _ => speak(clipboard_text().await, None).await,
                }
                draw(false).await;
            }
            _ => {}
        }
    }

    async fn dispose() {}
}

export!(Plugin);
