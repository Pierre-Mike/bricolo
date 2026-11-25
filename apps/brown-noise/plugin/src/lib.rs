//! brown-noise: a noise machine in a hub tile. A Play switch, volume
//! buttons and one button per noise color. Brown is the default.
//!
//! A port of the brown_noise.lua script. `audio` loops noise_<color>.wav
//! from the `sounds` folder with no gap and changes the volume while it
//! plays. Color, volume and whether it plays survive reloads in kv: a
//! reload stops the sound, and init starts it again. `domain.rs` holds the
//! logic as plain functions, tested with `cargo test`. This file is the
//! adapter: it calls the capabilities and passes their results to the
//! domain.
//!
//! Build:   cargo build --target wasm32-wasip2 --release
//! Check:   bricolo check .
//! Install: bricolo install . -y

// The generated `event` bindings take more than seven arguments.
#![allow(clippy::too_many_arguments)]

mod domain;

wit_bindgen::generate!({
    path: "wit",
    world: "bricolo:stdlib/plugin",
});

use bricolo::stdlib::log::{self, Level};
use bricolo::stdlib::types::{CallError, UiEvent};
use bricolo::stdlib::{audio, kv, ui};
use std::sync::{Mutex, OnceLock};

struct Plugin;

static SETTINGS: OnceLock<domain::Settings> = OnceLock::new();
/// The id `audio` gave the sound that plays, or none.
static SOUND: Mutex<Option<u64>> = Mutex::new(None);

fn settings() -> &'static domain::Settings {
    SETTINGS.get_or_init(|| domain::Settings::from_json("{}"))
}

fn sound() -> Option<u64> {
    *SOUND.lock().unwrap()
}

fn color() -> String {
    domain::color(kv::get("color"))
}

fn volume() -> u8 {
    domain::volume(kv::get("volume").as_deref())
}

/// The error as the script printed it.
fn describe(e: &CallError) -> String {
    match e {
        CallError::Unavailable => "unavailable".into(),
        CallError::Timeout => "timeout".into(),
        CallError::Denied => "denied".into(),
        CallError::Failed(msg) => format!("failed: {msg}"),
    }
}

fn save(key: &str, value: &str) {
    if let Err(e) = kv::set(key, value) {
        log::write(Level::Warn, &format!("cannot save {key}: {e:?}"));
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
        log::write(Level::Warn, &format!("cannot show a toast: {e:?}"));
    }
}

/// Starts the current color. The new sound starts before the old one
/// stops, so switching colors leaves no silence.
async fn play() -> bool {
    let path = domain::wav(&settings().sounds, &color());
    let options = audio::PlayOptions {
        volume: volume(),
        looping: true,
    };
    match audio::play(path, options).await {
        Ok(id) => {
            let old = SOUND.lock().unwrap().replace(id);
            if let Some(old) = old {
                let _ = audio::stop(old).await;
            }
            true
        }
        Err(e) => {
            let err = describe(&e);
            log::write(Level::Warn, &format!("noise: {err}"));
            toast(&format!("Noise: {err}"));
            false
        }
    }
}

async fn stop() {
    let old = SOUND.lock().unwrap().take();
    if let Some(old) = old {
        let _ = audio::stop(old).await;
    }
}

async fn set_playing(on: bool) {
    let on = if on {
        play().await
    } else {
        stop().await;
        false
    };
    save("playing", domain::playing_flag(on));
    draw();
}

async fn press(id: &str) {
    if id == "down" || id == "up" {
        let v = domain::stepped(volume(), id == "up", settings().step);
        save("volume", &v.to_string());
        if let Some(s) = sound() {
            let _ = audio::set_volume(s, volume()).await;
        }
    } else {
        save("color", id);
        if sound().is_some() {
            play().await;
        }
    }
    draw();
}

fn draw() {
    let color = color();
    let playing = sound().is_some();
    let tile = ui::Tile {
        title: "Noise".into(),
        status_text: domain::status(playing, &color, volume()),
        switches: vec![ui::Switch {
            id: "play".into(),
            label: "Play".into(),
            on: playing,
        }],
        buttons: domain::buttons(&color)
            .into_iter()
            .map(|(id, label)| ui::Button { id, label })
            .collect(),
    };
    if let Err(e) = ui::set_tile(&tile) {
        log::write(Level::Warn, &format!("cannot set the tile: {e:?}"));
    }
}

impl Guest for Plugin {
    /// `config` is this plugin's settings as JSON.
    async fn init(config: String) -> Result<(), String> {
        let _ = SETTINGS.set(domain::Settings::from_json(&config));
        if kv::get("playing").as_deref() == Some("1") {
            play().await;
        }
        draw();
        log::write(Level::Info, "brown_noise: tile ready");
        Ok(())
    }

    async fn on_event(event: Event) {
        match event {
            Event::Ui(UiEvent::Switch(s)) => set_playing(s.on).await,
            Event::Ui(UiEvent::Button(id)) => press(&id).await,
            _ => {}
        }
    }

    async fn dispose() {}
}

export!(Plugin);
