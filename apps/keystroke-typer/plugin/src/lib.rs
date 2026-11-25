//! keystroke-typer: types the clipboard as real keystrokes, for the places
//! that refuse or mangle a paste: password fields, remote consoles, VNC and
//! RDP sessions, kiosk forms that only listen for key presses.
//!
//! A port of the keystroke_typer.lua script. `domain.rs` holds the logic as
//! plain functions, tested with `cargo test`. This file is the adapter: it
//! calls the capabilities and passes their results to the domain.
//!
//! Build:   cargo build --target wasm32-wasip2 --release
//! Check:   bricolo check .
//! Install: bricolo install . -y
//! Try it:  bricolo fire keystroke-typer h1

// The generated `event` bindings take more than seven arguments.
#![allow(clippy::too_many_arguments)]

mod domain;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

wit_bindgen::generate!({
    path: "wit",
    world: "bricolo:stdlib/plugin",
});

use bricolo::stdlib::log::{self, Level};
use bricolo::stdlib::types::CallError;
use bricolo::stdlib::{clipboard, hotkey, keyboard, ui};

/// The binding id, the one the script's first `bricolo.hotkey.bind` got.
const TYPE: &str = "h1";

static SETTINGS: OnceLock<domain::Settings> = OnceLock::new();
/// Set while a run types, so a press meanwhile does nothing.
static BUSY: AtomicBool = AtomicBool::new(false);

struct Plugin;

/// The error as a script sees it: "denied", "failed: ...".
fn error_name(e: &CallError) -> String {
    match e {
        CallError::Unavailable => "unavailable".into(),
        CallError::Timeout => "timeout".into(),
        CallError::Denied => "denied".into(),
        CallError::Failed(msg) => format!("failed: {msg}"),
    }
}

fn toast(text: String) {
    let panel = ui::Panel {
        kind: ui::PanelKind::Toast,
        title: String::new(),
        text,
        choices: vec![],
        timeout_ms: None,
    };
    if let Err(e) = ui::show_panel(&panel) {
        log::write(Level::Warn, &format!("cannot show a toast: {e:?}"));
    }
}

async fn type_steps(steps: Vec<domain::Step>) -> Result<(), CallError> {
    for step in steps {
        match step {
            domain::Step::Type(text) => keyboard::type_text(text).await?,
            domain::Step::ShiftEnter => {
                keyboard::keystroke(vec!["shift".into()], "enter".into()).await?
            }
        }
    }
    Ok(())
}

async fn run() {
    if BUSY.load(Ordering::SeqCst) {
        return;
    }
    let settings = SETTINGS.get().cloned().unwrap_or_default();
    let text = match clipboard::get().await {
        Ok(text) => text,
        Err(e) => {
            log::write(
                Level::Warn,
                &format!("cannot read the clipboard: {}", error_name(&e)),
            );
            return;
        }
    };
    let count = match domain::check(text.as_deref(), settings.max_chars) {
        domain::Check::Refuse(why) => return toast(why),
        domain::Check::Type(count) => count,
    };
    let text = text.unwrap_or_default();
    BUSY.store(true, Ordering::SeqCst);
    let result = type_steps(domain::steps(&text, settings.newline)).await;
    BUSY.store(false, Ordering::SeqCst);
    match result {
        Ok(()) => log::write(Level::Info, &format!("typed {count} characters")),
        Err(e) => {
            let e = error_name(&e);
            log::write(Level::Warn, &format!("cannot type: {e}"));
            toast(format!("Cannot type: {e}"));
        }
    }
}

impl Guest for Plugin {
    /// `config` is this plugin's settings as JSON.
    async fn init(config: String) -> Result<(), String> {
        let settings = SETTINGS.get_or_init(|| domain::Settings::from_json(&config));
        // A chord another program holds fails to bind. Log it and keep
        // running, so `bricolo fire` still reaches the plugin.
        if let Err(e) = hotkey::bind(TYPE.into(), settings.chord.clone()).await {
            log::write(
                Level::Warn,
                &format!("cannot bind {}: {}", settings.chord, error_name(&e)),
            );
        }
        log::write(
            Level::Info,
            &format!("press {} to type the clipboard", settings.chord),
        );
        Ok(())
    }

    async fn on_event(event: Event) {
        match event {
            Event::Hotkey(id) if id == TYPE => run().await,
            _ => {}
        }
    }

    async fn dispose() {}
}

export!(Plugin);
