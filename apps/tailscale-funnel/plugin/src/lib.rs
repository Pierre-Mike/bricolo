//! tailscale-funnel: the hub tile for the tailscale-funnel sidecar, which
//! leases this Mac's Tailscale Funnel address (https://<machine>.<tailnet>.ts.net)
//! to the voice agent and anything else on 127.0.0.1:8795. The tile shows
//! the status in words, the public URL and who holds a lease, and has the
//! buttons that matter: turn the funnel on or off (the only way it is ever
//! armed), check again, copy the URL, and the next setup step.
//!
//! A port of the tailscale_funnel.lua script. `domain.rs` holds the logic as
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

use domain::{Act, Failure, Notice, View};
use bricolo::stdlib::log::{self, Level};
use bricolo::stdlib::types::{CallError, UiEvent};
use bricolo::stdlib::{app, clipboard, http, timer, ui};

static SETTINGS: OnceLock<domain::Settings> = OnceLock::new();

/// The sidecar's last GET /view answer, why it didn't answer, and one line
/// while the funnel is being switched.
#[derive(Default)]
struct State {
    view: Option<View>,
    unreachable: Option<String>,
    busy: Option<String>,
}

static STATE: Mutex<State> = Mutex::new(State {
    view: None,
    unreachable: None,
    busy: None,
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn settings() -> &'static domain::Settings {
    SETTINGS.get_or_init(domain::Settings::default)
}

struct Plugin;

/// The `err` a script sees: the `call-error` case name.
fn error_name(e: &CallError) -> String {
    match e {
        CallError::Unavailable => "unavailable".into(),
        CallError::Timeout => "timeout".into(),
        CallError::Denied => "denied".into(),
        CallError::Failed(msg) => format!("failed: {msg}"),
    }
}

/// The sidecar's JSON answer, or why not.
async fn call(method: &str, path: &str) -> Result<serde_json::Value, Failure> {
    let s = settings();
    let request = http::Request {
        method: method.into(),
        url: format!("{}{path}", s.url),
        headers: vec![],
        body: vec![],
        timeout_ms: Some(s.timeout_ms),
    };
    match http::send(request).await {
        Ok(r) => domain::json_of(Ok((r.status, &r.body))),
        Err(e) => domain::json_of(Err(error_name(&e))),
    }
}

async fn refresh() {
    let answer = call("GET", "/view").await;
    {
        let mut st = state();
        match answer {
            Ok(v) => {
                st.view = Some(View::from_value(&v));
                st.unreachable = None;
            }
            Err(f) => {
                st.view = None;
                st.unreachable = Some(domain::unreachable(&f.why));
            }
        }
    }
    draw();
}

fn draw() {
    let tile = {
        let st = state();
        ui::Tile {
            title: "Tailscale Funnel".into(),
            status_text: domain::status_text(
                st.view.as_ref(),
                st.unreachable.as_deref(),
                st.busy.as_deref(),
                &settings().url,
            ),
            switches: vec![],
            buttons: domain::buttons(st.view.as_ref())
                .into_iter()
                .map(|(id, label)| ui::Button { id, label })
                .collect(),
        }
    };
    if let Err(e) = ui::set_tile(&tile) {
        log::write(
            Level::Warn,
            &format!("cannot set the tile: {}", error_name(&e)),
        );
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

/// Logs "what: err" as a warning and shows it in a toast.
fn report(what: &str, err: &str) {
    let line = domain::report_line(what, err);
    log::write(Level::Warn, &line);
    toast(line);
}

fn notify(notice: Notice) {
    match notice {
        Notice::Report(what, err) => report(&what, &err),
        Notice::Toast(text) => toast(text),
        Notice::Info(line) => log::write(Level::Info, &line),
        Notice::Nothing => {}
    }
}

async fn open(url: &str) {
    if let Err(e) = app::open_url(url.to_string()).await {
        report(&format!("Could not open {url}"), &error_name(&e));
    }
}

async fn copy(text: Option<String>, what: &str) {
    let Some(text) = text else {
        toast("Nothing to copy yet".into());
        return;
    };
    if let Err(e) = clipboard::set(text).await {
        report(&format!("Could not copy {what}"), &error_name(&e));
        return;
    }
    log::write(Level::Info, &format!("copied {what}"));
    toast(format!("Copied {what}"));
}

/// Opens the app, then looks again a little later; the timer event
/// refreshes the tile.
async fn open_app() {
    if let Err(e) = app::launch(domain::TAILSCALE_APP.into()).await {
        report("Could not open the Tailscale app", &error_name(&e));
        return;
    }
    log::write(Level::Info, "opened the Tailscale app");
    timer::after(domain::AFTER_APP_MS);
}

/// Arms or disarms the funnel through the sidecar. A human pressing this is
/// the only way the funnel is ever turned on.
async fn toggle() {
    let on = {
        let mut st = state();
        let Some(view) = st.view.as_ref().filter(|_| st.busy.is_none()) else {
            return;
        };
        let on = !view.funnel;
        st.busy = Some(domain::busy_text(on).into());
        on
    };
    draw();
    let answer = call("POST", if on { "/arm" } else { "/disarm" }).await;
    state().busy = None;
    notify(domain::toggled(on, &answer));
    refresh().await;
}

async fn check() {
    let answer = call("POST", "/check").await;
    notify(domain::checked(&answer));
    refresh().await;
}

async fn step() {
    let acts = domain::step_acts(state().view.as_ref());
    for act in acts {
        match act {
            Act::Open(url) => open(&url).await,
            Act::OpenApp => open_app().await,
            Act::Copy(text, what) => copy(text, what).await,
        }
    }
}

async fn on_button(id: &str) {
    match id {
        domain::TOGGLE => toggle().await,
        domain::CHECK => check().await,
        domain::COPY => {
            let url = state().view.as_ref().and_then(|v| v.copyable.clone());
            copy(url, "the funnel URL").await
        }
        domain::STEP => step().await,
        domain::APP => open_app().await,
        domain::ADMIN => open(domain::URL_MACHINES).await,
        _ => {}
    }
}

impl Guest for Plugin {
    /// `config` is this plugin's settings as JSON.
    async fn init(config: String) -> Result<(), String> {
        let settings = SETTINGS.get_or_init(|| domain::Settings::from_json(&config));
        refresh().await;
        timer::every(settings.poll_ms);
        let line = {
            let st = state();
            match st.view.as_ref().and_then(|v| v.status_line.clone()) {
                Some(status) => status,
                None => format!(
                    "sidecar not answering: {}",
                    st.unreachable.as_deref().unwrap_or("nil")
                ),
            }
        };
        log::write(Level::Info, &format!("tile ready, {line}"));
        Ok(())
    }

    async fn on_event(event: Event) {
        match event {
            // The poll and the look after opening the app.
            Event::Timer(_) => {
                if state().busy.is_none() {
                    refresh().await;
                }
            }
            Event::Ui(UiEvent::Button(id)) => on_button(&id).await,
            _ => {}
        }
    }

    async fn dispose() {}
}

export!(Plugin);
