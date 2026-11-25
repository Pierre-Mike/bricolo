//! password-typer: types a password saved in the Keychain into the focused
//! field, after Touch ID.
//!
//! The chord (cmd+alt+shift+p by default) opens a list of the names in
//! `names`; with one name it skips the list. After you pick one, the
//! touch-id sidecar (`../sidecar`) opens the Touch ID prompt. Once you
//! confirm, the plugin reads the password through `secrets` and types it
//! with `keyboard`, so it never goes through the clipboard.
//!
//! To save a password, press "Set password…" in the plugin's card in the
//! hub and pick a name. After Touch ID, a macOS dialog with a hidden field
//! (`osascript`, through `shell`) asks for it twice. The plugin holds it
//! in memory only until it saves it; it never touches the clipboard. The
//! Keychain holds it as service `bricolo`, account
//! `password-typer/<name>`.
//!
//! `domain.rs` holds the logic as plain functions, tested with
//! `cargo test`. This file is the adapter.
//!
//! Build:   cargo build --target wasm32-wasip2 --release
//! Check:   bricolo check .
//! Install: bricolo install . -y
//! Try it:  bricolo fire password-typer type-password
//!          bricolo fire password-typer set-password

// The generated `event` bindings take more than seven arguments.
#![allow(clippy::too_many_arguments)]

mod domain;

use std::sync::{Mutex, OnceLock};

use domain::Action;

wit_bindgen::generate!({
    path: "wit",
    world: "bricolo:stdlib/plugin",
});

use bricolo::stdlib::log::{self, Level};
use bricolo::stdlib::types::UiEvent;
use bricolo::stdlib::{hotkey, http, keyboard, secrets, shell, timer, ui};

/// The binding id of the chord. `bricolo fire password-typer type-password`
/// sends it too.
const TYPE: &str = "type-password";
/// The tile button, and an id `bricolo fire` can send.
const SAVE: &str = "set-password";
/// Time for the app in front to get the keyboard back after the Touch ID
/// prompt closes.
const FOCUS_BACK_MS: u32 = 250;

static SETTINGS: OnceLock<domain::Settings> = OnceLock::new();
/// The open picker's panel id and what it was opened for.
static PICKER: Mutex<Option<(String, Action)>> = Mutex::new(None);

struct Plugin;

fn settings() -> domain::Settings {
    SETTINGS.get().cloned().unwrap_or_default()
}

fn toast(text: String) {
    let panel = ui::Panel {
        kind: ui::PanelKind::Toast,
        title: "password-typer".into(),
        text,
        choices: vec![],
        timeout_ms: None,
    };
    if let Err(e) = ui::show_panel(&panel) {
        log::write(Level::Warn, &format!("cannot show a toast: {e:?}"));
    }
}

/// Logs `why` and shows it.
fn fail(why: String) {
    log::write(Level::Warn, &why);
    toast(why);
}

fn draw_tile(settings: &domain::Settings) {
    let tile = ui::Tile {
        title: "Passwords".into(),
        status_text: format!("{} types: {}", settings.chord, settings.names.join(", ")),
        switches: vec![],
        buttons: vec![ui::Button {
            id: SAVE.into(),
            label: "Set password…".into(),
        }],
    };
    if let Err(e) = ui::set_tile(&tile) {
        log::write(Level::Warn, &format!("cannot set the tile: {e:?}"));
    }
}

/// Types at once with one name, else asks which name first. Saving always
/// asks, so it is never a single accidental click.
async fn start(action: Action) {
    let settings = settings();
    if action == Action::Type && settings.names.len() == 1 {
        return type_password(&settings.names[0]).await;
    }
    let panel = ui::Panel {
        kind: ui::PanelKind::ChoiceList,
        title: action.picker_title().into(),
        text: String::new(),
        choices: settings.names.clone(),
        timeout_ms: Some(30_000),
    };
    match ui::show_panel(&panel) {
        Ok(id) => *PICKER.lock().unwrap() = Some((id, action)),
        Err(e) => fail(format!("cannot show the list: {e:?}")),
    }
}

/// Asks the touch-id sidecar to confirm, unless Touch ID is off.
async fn confirm(settings: &domain::Settings, action: Action, name: &str) -> Result<(), String> {
    if !settings.touch_id {
        return Ok(());
    }
    let request = http::Request {
        method: "POST".into(),
        url: settings.url.clone(),
        headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
        body: action.reason(name).into_bytes(),
        timeout_ms: Some(settings.timeout_ms),
    };
    match http::send(request).await {
        Ok(response) => domain::read_auth(response.status, &response.body),
        // `unavailable` usually means the sidecar is not running: check
        // `bricolo sidecar list`.
        Err(e) => Err(format!("cannot reach the touch-id sidecar: {e:?}")),
    }
}

async fn type_password(name: &str) {
    let settings = settings();
    if let Err(why) = confirm(&settings, Action::Type, name).await {
        return fail(why);
    }
    let value = match secrets::get(name.into()).await {
        Ok(value) => value,
        Err(e) => return fail(format!("cannot read \"{name}\" from the Keychain: {e:?}")),
    };
    if let Err(why) = domain::check_password(name, value.as_deref()) {
        return fail(why);
    }
    let value = value.unwrap_or_default();
    if settings.touch_id {
        timer::sleep(FOCUS_BACK_MS).await;
    }
    match keyboard::type_text(value).await {
        // Never log the password, only its name.
        Ok(()) => log::write(Level::Info, &format!("typed \"{name}\"")),
        Err(e) => fail(format!("cannot type \"{name}\": {e:?}")),
    }
}

/// Shows the hidden-field dialog and returns what was typed.
async fn ask(prompt: &str) -> Result<String, String> {
    match shell::run(domain::OSASCRIPT.into(), domain::dialog_args(prompt), vec![]).await {
        Ok(out) => domain::read_dialog(out.code, &out.stdout, &out.stderr),
        Err(e) => Err(format!("cannot show the password dialog: {e:?}")),
    }
}

async fn save_password(name: &str) {
    let settings = settings();
    if let Err(why) = confirm(&settings, Action::Save, name).await {
        return fail(why);
    }
    let [first, again] = domain::dialog_prompts(name);
    let value = match ask(&first).await {
        Ok(value) => value,
        Err(why) => return fail(why),
    };
    let value = match ask(&again).await.and_then(|second| domain::matching(value, &second)) {
        Ok(value) => value,
        Err(why) => return fail(why),
    };
    if let Err(e) = secrets::set(name.into(), value).await {
        return fail(format!("cannot save \"{name}\" in the Keychain: {e:?}"));
    }
    // Never log the password, only its name.
    log::write(Level::Info, &format!("saved \"{name}\""));
    toast(format!("Saved \"{name}\""));
}

async fn run(name: &str, action: Action) {
    match action {
        Action::Type => type_password(name).await,
        Action::Save => save_password(name).await,
    }
}

impl Guest for Plugin {
    /// `config` is this plugin's settings as JSON.
    async fn init(config: String) -> Result<(), String> {
        let settings = SETTINGS.get_or_init(|| domain::Settings::from_json(&config));
        for name in &settings.rejected {
            log::write(
                Level::Warn,
                &format!("ignoring the name {name:?}: use 1 to 128 letters, digits, '.', '_' or '-'"),
            );
        }
        // A chord another program holds fails to bind. Log it and keep
        // running, so `bricolo fire` still reaches the plugin.
        if let Err(e) = hotkey::bind(TYPE.into(), settings.chord.clone()).await {
            log::write(Level::Warn, &format!("cannot bind {}: {e:?}", settings.chord));
        }
        draw_tile(settings);
        log::write(
            Level::Info,
            &format!(
                "press {} to type a password ({}), Touch ID {}",
                settings.chord,
                settings.names.join(", "),
                if settings.touch_id { "on" } else { "off" }
            ),
        );
        Ok(())
    }

    async fn on_event(event: Event) {
        match event {
            Event::Hotkey(id) if id == TYPE => start(Action::Type).await,
            Event::Hotkey(id) if id == SAVE => start(Action::Save).await,
            Event::Ui(UiEvent::Button(id)) if id == SAVE => start(Action::Save).await,
            Event::Ui(UiEvent::Choice(choice)) => {
                let open = PICKER.lock().unwrap().take();
                let Some((panel, action)) = open else { return };
                if panel != choice.panel {
                    // An older list; keep waiting for the open one.
                    *PICKER.lock().unwrap() = Some((panel, action));
                    return;
                }
                let names = settings().names;
                if let Some(name) = domain::picked(&names, choice.index) {
                    run(name, action).await;
                }
            }
            _ => {}
        }
    }

    async fn dispose() {}
}

export!(Plugin);
