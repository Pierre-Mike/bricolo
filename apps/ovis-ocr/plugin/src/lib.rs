//! ovis-ocr: local OCR from a hub tile and a chord. Press ctrl+alt+shift+o,
//! drag over part of the screen, and its text lands on the clipboard as
//! Markdown. The tile also reads the image on the clipboard, opens the
//! server's drop page for files, and unloads the model.
//!
//! A port of the ovis_ocr.lua script. The server is the ovis-ocr sidecar
//! in node.toml (port 8797); it loads OvisOCR2 on the first page and drops
//! it after ten idle minutes. Screen capture needs the Screen Recording
//! permission for bricolo. `domain.rs` holds the logic as plain
//! functions, tested with `cargo test`. This file is the adapter: it calls
//! the capabilities and passes their results to the domain.
//!
//! Build:   cargo build --target wasm32-wasip2 --release
//! Check:   bricolo check .
//! Install: bricolo install . -y
//! Try it:  bricolo fire ovis-ocr h1

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
use bricolo::stdlib::types::{CallError, UiEvent};
use bricolo::stdlib::{app, clipboard, hotkey, http, shell, timer, ui};

/// The binding id the script's only `bricolo.hotkey.bind` got, kept so
/// `bricolo fire ovis-ocr h1` still reads a region.
const REGION: &str = "h1";

static SETTINGS: OnceLock<domain::Settings> = OnceLock::new();
static BUSY: AtomicBool = AtomicBool::new(false);

struct Plugin;

fn settings() -> domain::Settings {
    SETTINGS.get().cloned().unwrap_or_default()
}

/// The error as a script sees it: "unavailable", "failed: …".
fn why(e: &CallError) -> String {
    match e {
        CallError::Unavailable => "unavailable".into(),
        CallError::Timeout => "timeout".into(),
        CallError::Denied => "denied".into(),
        CallError::Failed(msg) => format!("failed: {msg}"),
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

async fn run(program: &str, args: Vec<String>) -> Result<shell::Output, CallError> {
    shell::run(program.into(), args, Vec::new()).await
}

/// Draws the tile with `status`, or with what /health says when None.
async fn draw(status: Option<String>) {
    let s = settings();
    let status = match status {
        Some(status) => status,
        None => {
            let request = http::Request {
                method: "GET".into(),
                url: format!("{}/health", s.base),
                headers: Vec::new(),
                body: Vec::new(),
                timeout_ms: Some(s.health_timeout_ms),
            };
            let body = match http::send(request).await {
                Ok(r) if r.status == 200 => Some(String::from_utf8_lossy(&r.body).into_owned()),
                _ => None,
            };
            domain::describe(body.as_deref())
        }
    };
    let buttons = domain::BUTTONS
        .iter()
        .map(|(id, label)| ui::Button {
            id: id.to_string(),
            label: label.to_string(),
        })
        .collect();
    let tile = ui::Tile {
        title: "OCR".into(),
        status_text: status,
        switches: vec![],
        buttons,
    };
    if let Err(e) = ui::set_tile(&tile) {
        log::write(Level::Warn, &format!("cannot set the tile: {}", why(&e)));
    }
}

/// OCRs the image or PDF at `path` and puts the Markdown on the clipboard.
async fn ocr_file(path: &str) {
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    let s = settings();
    draw(Some("Reading…".into())).await;
    let request = http::Request {
        method: "POST".into(),
        url: format!("{}/ocr", s.base),
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: domain::ocr_body(path).into_bytes(),
        timeout_ms: Some(s.ocr_timeout_ms),
    };
    let answer = http::send(request).await;
    BUSY.store(false, Ordering::SeqCst);
    match answer {
        Err(e) => {
            toast(format!("OCR: {}", why(&e)));
            log::write(Level::Warn, &format!("ocr: {}", why(&e)));
        }
        Ok(r) => match domain::outcome(r.status, &r.body) {
            domain::Outcome::Refused { status, body } => {
                toast(format!("OCR: {body}"));
                log::write(Level::Warn, &format!("ocr: {status} {body}"));
            }
            domain::Outcome::NoText => toast("OCR found no text".into()),
            domain::Outcome::Copied { text } => {
                // The script ignored a failed set too.
                let _ = clipboard::set(text.clone()).await;
                toast(domain::copied_toast(&text));
                log::write(Level::Info, &domain::copied_log(&text, path));
            }
        },
    }
    draw(None).await;
}

async fn ocr_region() {
    let shot = settings().shot;
    // -i: drag a region; Escape cancels and writes no file.
    let _ = run("/bin/rm", vec!["-f".into(), shot.clone()]).await;
    let capture = run(
        "/usr/sbin/screencapture",
        vec!["-i".into(), "-x".into(), shot.clone()],
    )
    .await;
    if let Err(e) = capture {
        log::write(Level::Warn, &format!("screencapture: {}", why(&e)));
        return;
    }
    let exists = run("/bin/test", vec!["-s".into(), shot.clone()]).await;
    if matches!(exists, Ok(r) if r.code == Some(0)) {
        ocr_file(&shot).await;
    }
}

async fn ocr_clipboard() {
    let shot = settings().shot;
    match run("/usr/bin/osascript", domain::clipboard_script(&shot)).await {
        Err(e) => toast(format!("Cannot read the clipboard: {}", why(&e))),
        Ok(r) if r.code != Some(0) => toast(format!("Cannot read the clipboard: {}", r.stderr)),
        Ok(r) if r.stdout.contains("none") => toast("The clipboard holds no image".into()),
        Ok(_) => ocr_file(&shot).await,
    }
}

async fn unload() {
    let request = http::Request {
        method: "POST".into(),
        url: format!("{}/unload", settings().base),
        headers: Vec::new(),
        body: Vec::new(),
        timeout_ms: None,
    };
    let _ = http::send(request).await;
    draw(None).await;
}

impl Guest for Plugin {
    /// `config` is this plugin's settings as JSON.
    async fn init(config: String) -> Result<(), String> {
        let s = SETTINGS.get_or_init(|| domain::Settings::from_json(&config));
        if let Err(e) = hotkey::bind(REGION.into(), s.chord.clone()).await {
            log::write(
                Level::Warn,
                &format!("ovis_ocr: cannot bind {}: {}", s.chord, why(&e)),
            );
        }
        draw(None).await;
        timer::every(s.poll_ms);
        log::write(
            Level::Info,
            &format!(
                "ovis_ocr: press {} and drag over text to copy it as Markdown",
                s.chord
            ),
        );
        Ok(())
    }

    async fn on_event(event: Event) {
        match event {
            Event::Hotkey(id) if id == REGION => ocr_region().await,
            Event::Timer(_) if !BUSY.load(Ordering::SeqCst) => draw(None).await,
            Event::Ui(UiEvent::Button(id)) => match id.as_str() {
                "region" => ocr_region().await,
                "clipboard" => ocr_clipboard().await,
                "page" => {
                    let url = format!("{}/", settings().base);
                    if let Err(e) = app::open_url(url.clone()).await {
                        log::write(Level::Warn, &format!("cannot open {url}: {}", why(&e)));
                    }
                }
                "unload" => unload().await,
                _ => {}
            },
            _ => {}
        }
    }

    async fn dispose() {}
}

export!(Plugin);
