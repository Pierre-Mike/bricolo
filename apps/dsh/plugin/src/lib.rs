//! dsh: a hub tile for the DeepSeek Harness web server, which runs as the
//! `dsh` sidecar (`dsh --profile web --no-open --port 3080`). It opens the
//! web UI with its login token and restarts the server.
//!
//! A port of the dsh.lua script. The node starts the server, restarts it
//! when it dies and stops it with the node, so the tile has no on/off
//! switch. `domain.rs` holds the logic as plain functions, tested with
//! `cargo test`. This file is the adapter: it calls the capabilities and
//! passes their results to the domain.
//!
//! Build:   cargo build --target wasm32-wasip2 --release
//! Check:   bricolo check .
//! Install: bricolo install . -y

// The generated `event` bindings take more than seven arguments.
#![allow(clippy::too_many_arguments)]

mod domain;

use std::cell::RefCell;
use std::sync::OnceLock;

wit_bindgen::generate!({
    path: "wit",
    world: "bricolo:stdlib/plugin",
});

use bricolo::stdlib::log::{self, Level};
use bricolo::stdlib::types::UiEvent;
use bricolo::stdlib::{app, disk, http, shell, timer, ui};

static SETTINGS: OnceLock<domain::Settings> = OnceLock::new();

thread_local! {
    static STATE: RefCell<domain::State> = RefCell::default();
}

struct Plugin;

fn settings() -> domain::Settings {
    SETTINGS.get().cloned().unwrap_or_default()
}

/// The newest address with its token from the server's log, if any.
async fn read_address() -> Option<String> {
    let log = disk::read(settings().server_log).await.ok()?;
    domain::address_from(&domain::tail(&log))
}

async fn poll() {
    let request = http::Request {
        method: "GET".into(),
        url: format!("{}/manifest.webmanifest", settings().base()),
        headers: vec![],
        body: vec![],
        timeout_ms: Some(3000),
    };
    let up = matches!(http::send(request).await, Ok(r) if r.status == 200);
    if let Some(line) = STATE.with_borrow_mut(|s| s.polled(up)) {
        log::write(Level::Info, line);
    }
    draw();
}

async fn open_ui() {
    let url = read_address().await.unwrap_or_else(|| settings().base());
    if let Err(e) = app::open_url(url).await {
        log::write(Level::Warn, &format!("cannot open the UI: {e:?}"));
    }
}

async fn restart() {
    if !STATE.with_borrow_mut(|s| s.begin_restart()) {
        return;
    }
    draw();
    let args: Vec<String> = vec!["sidecar".into(), "restart".into(), "dsh".into()];
    let failed = match shell::run(settings().bricolo, args, vec![]).await {
        Ok(r) if r.code == Some(0) => None,
        Ok(r) => Some(r.stderr),
        Err(e) => Some(format!("{e:?}")),
    };
    if let Some(why) = failed {
        STATE.with_borrow_mut(|s| s.busy = None);
        toast(format!("Cannot restart dsh: {}", why.trim()));
    }
    for s in domain::RETRY_SECS {
        timer::after(s * 1000);
    }
}

fn toast(text: String) {
    let _ = ui::show_panel(&ui::Panel {
        kind: ui::PanelKind::Toast,
        title: String::new(),
        text,
        choices: Vec::new(),
        timeout_ms: None,
    });
}

fn draw() {
    let status = STATE.with_borrow(|s| s.status(&settings().base()));
    let button = |id: &str, label: &str| ui::Button {
        id: id.into(),
        label: label.into(),
    };
    let tile = ui::Tile {
        title: "DSH".into(),
        status_text: status,
        switches: vec![],
        buttons: vec![
            button("open", "Open the web UI"),
            button("restart", "Restart"),
            button("refresh", "Refresh now"),
        ],
    };
    if let Err(e) = ui::set_tile(&tile) {
        log::write(Level::Warn, &format!("cannot set the tile: {e:?}"));
    }
}

impl Guest for Plugin {
    /// `config` is this plugin's settings as JSON.
    async fn init(config: String) -> Result<(), String> {
        let settings = SETTINGS.get_or_init(|| domain::Settings::from_json(&config));
        poll().await;
        timer::every(settings.poll_ms);
        let running = STATE.with_borrow(|s| s.running);
        log::write(
            Level::Info,
            &format!(
                "dsh: tile ready, server {}",
                if running { "up" } else { "not answering" }
            ),
        );
        Ok(())
    }

    async fn on_event(event: Event) {
        match event {
            // The poll timer and the retries after a restart.
            Event::Timer(_) => poll().await,
            Event::Ui(UiEvent::Button(id)) => match id.as_str() {
                "open" => open_ui().await,
                "restart" => restart().await,
                _ => poll().await,
            },
            _ => {}
        }
    }

    async fn dispose() {}
}

export!(Plugin);
