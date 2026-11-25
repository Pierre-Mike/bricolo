//! Test fixture driven by `hotkey(id)` events. Each command logs one line
//! starting with its name:
//! - `clip-roundtrip` sets the clipboard, reads it back, restores it;
//! - `clip-get`, `clip-set:<text>`;
//! - `apps`, `front`, `windows`, `focused`, `screens`;
//! - `notify:<text>`;
//! - `speak:<text>`, `tts-status`;
//! - `bind:<chord>` binds the chord as `pressed`; a press logs `pressed`;
//! - `shell:<program>` runs it with the argument `hi`;
//! - `http:<url>` sends a GET;
//! - `tile:<title>` sets a tile with the switch `armed` and the button
//!   `ping`, `clear-tile`; `widget:<value>` sets a widget with one row,
//!   `clear-widget`;
//! - `close:<id>` and `minimize:<id>` close or minimize a window;
//!   `quit:<app>` and `hide-app:<app>` quit or hide an app;
//! - `caption:<text>`, `toast:<text>`, `review:<text>`, `choices:<a,b>`
//!   show a panel and log its id, `hide:<id>` hides one;
//! - `mic-start` starts a recording; `mic-stop` stops it and transcribes
//!   the file it returns;
//! - `snapshot` takes a photo and logs its path;
//! - `play:<path>` loops a sound at volume 30, `sounds` lists them;
//! - `secret-get:<name>` reads a secret;
//! - `app-watch` starts `app` events, `idle` reads the idle time,
//!   `files-watch:<path>` watches a folder.
//! - `mouse-pos` reads the pointer, `click:<n>` left-clicks `n` times.
//!
//! A `ui` event logs `ui: <event>`; an `app`, `system` or `file` event logs
//! `app-event:`, `system-event:` or `file-event:` and its fields.

// The generated `event` bindings take more than seven arguments.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../wit",
    world: "bricolo:stdlib/plugin",
});

use bricolo::stdlib::log::{self, Level};
use std::sync::atomic::{AtomicU64, Ordering};

use bricolo::stdlib::{
    app, audio, camera, clipboard, files, hotkey, http, mic, mouse, notify, screen, secrets,
    selection, shell, speech, system, tts, ui, window,
};

/// The recording `mic-start` started.
static RECORDING: AtomicU64 = AtomicU64::new(0);

struct Caps;

fn say(line: String) {
    log::write(Level::Info, &line);
}

async fn clip_roundtrip() -> String {
    let before = match clipboard::get().await {
        Ok(text) => text,
        Err(e) => return format!("{e:?}"),
    };
    let probe = "bricolo caps fixture";
    if let Err(e) = clipboard::set(probe.to_string()).await {
        return format!("{e:?}");
    }
    let back = clipboard::get().await;
    if let Some(text) = before {
        let _ = clipboard::set(text).await;
    }
    match back {
        Ok(Some(text)) if text == probe => "ok".into(),
        other => format!("read back {other:?}"),
    }
}

fn tile(title: &str) -> ui::Tile {
    ui::Tile {
        title: title.into(),
        status_text: "ready".into(),
        switches: vec![ui::Switch {
            id: "armed".into(),
            label: "Armed".into(),
            on: false,
        }],
        buttons: vec![ui::Button {
            id: "ping".into(),
            label: "Ping".into(),
        }],
    }
}

/// `caption:`, `toast:`, `review:` and `choices:` commands.
fn ui_panel(command: &str) -> Option<String> {
    let (name, rest) = command.split_once(':')?;
    let (kind, text, choices) = match name {
        "caption" => (ui::PanelKind::Caption, rest, vec![]),
        "toast" => (ui::PanelKind::Toast, rest, vec![]),
        "review" => (ui::PanelKind::Review, rest, vec![]),
        "choices" => (
            ui::PanelKind::ChoiceList,
            "",
            rest.split(',').map(String::from).collect(),
        ),
        _ => return None,
    };
    let shown = ui::show_panel(&ui::Panel {
        kind,
        title: "caps".into(),
        text: text.into(),
        choices,
        timeout_ms: None,
    });
    Some(format!("{name}: {shown:?}"))
}

impl Guest for Caps {
    async fn init(_config: String) -> Result<(), String> {
        say("init".into());
        Ok(())
    }

    async fn on_event(event: Event) {
        let id = match event {
            Event::Hotkey(id) => id,
            Event::Ui(e) => return say(format!("ui: {e:?}")),
            Event::App(e) => return say(format!("app-event: {:?} {}", e.change, e.name)),
            Event::System(e) => return say(format!("system-event: {e:?}")),
            Event::File(e) => {
                return say(format!("file-event: {} {} {:?}", e.watch, e.path, e.change));
            }
            _ => return,
        };
        let line = match id.as_str() {
            "clip-roundtrip" => format!("clip-roundtrip: {}", clip_roundtrip().await),
            "clip-get" => format!("clip-get: {:?}", clipboard::get().await),
            "apps" => match app::running().await {
                Ok(apps) => format!("apps: {}", apps.len()),
                Err(e) => format!("apps: {e:?}"),
            },
            "front" => match app::frontmost().await {
                Ok(front) => format!("front: {:?}", front.map(|a| a.name)),
                Err(e) => format!("front: {e:?}"),
            },
            "windows" => match window::list().await {
                Ok(windows) => format!("windows: {}", windows.len()),
                Err(e) => format!("windows: {e:?}"),
            },
            "focused" => match window::focused().await {
                Ok(w) => format!("focused: {:?}", w.map(|w| (w.app, w.frame))),
                Err(e) => format!("focused: {e:?}"),
            },
            "pressed" => "pressed".into(),
            "app-watch" => format!("app-watch: {:?}", app::watch(true).await),
            "idle" => format!("idle: {:?}", system::idle_seconds().await),
            "clear-tile" => format!("clear-tile: {:?}", ui::clear_tile()),
            "clear-widget" => format!("clear-widget: {:?}", ui::clear_widget()),
            "mic-start" => {
                let options = mic::RecordOptions {
                    max_secs: 30,
                    format: mic::AudioFormat::Wav16kMono,
                };
                match mic::record_start(options).await {
                    Ok(id) => {
                        RECORDING.store(id, Ordering::Relaxed);
                        format!("mic-start: {id}")
                    }
                    Err(e) => format!("mic-start: {e:?}"),
                }
            }
            "mic-stop" => match mic::record_stop(RECORDING.load(Ordering::Relaxed)).await {
                Ok(path) => format!("mic-stop: {:?}", speech::transcribe_file(path).await),
                Err(e) => format!("mic-stop: {e:?}"),
            },
            "mouse-pos" => format!("mouse-pos: {:?}", mouse::position().await),
            "snapshot" => format!("snapshot: {:?}", camera::snapshot().await),
            "tts-status" => format!("tts-status: {:?}", tts::status().await),
            "sounds" => format!("sounds: {:?}", audio::sounds().await),
            "shot" => format!("shot: {:?}", screen::capture(None).await),
            "shot-empty" => {
                let area = window::Rect {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 5,
                };
                format!("shot-empty: {:?}", screen::capture(Some(area)).await)
            }
            "selection" => format!("selection: {:?}", selection::get_text().await),
            "screens" => match window::screens().await {
                Ok(screens) => format!("screens: {}", screens.len()),
                Err(e) => format!("screens: {e:?}"),
            },
            other => {
                if let Some(path) = other.strip_prefix("files-watch:") {
                    format!("files-watch: {:?}", files::watch(path.into()).await)
                } else if let Some(text) = other.strip_prefix("clip-set:") {
                    format!("clip-set: {:?}", clipboard::set(text.to_string()).await)
                } else if let Some(text) = other.strip_prefix("speak:") {
                    format!("speak: {:?}", tts::speak(text.into(), None).await)
                } else if let Some(n) = other.strip_prefix("click:") {
                    let n = n.parse().unwrap_or(1);
                    format!("click: {:?}", mouse::click(mouse::Button::Left, n).await)
                } else if let Some(name) = other.strip_prefix("secret-get:") {
                    format!("secret-get: {:?}", secrets::get(name.into()).await)
                } else if let Some(path) = other.strip_prefix("play:") {
                    let options = audio::PlayOptions {
                        volume: 30,
                        looping: true,
                    };
                    format!("play: {:?}", audio::play(path.into(), options).await)
                } else if let Some(chord) = other.strip_prefix("bind:") {
                    let bound = hotkey::bind("pressed".into(), chord.into()).await;
                    format!("bind {chord}: {bound:?}")
                } else if let Some(text) = other.strip_prefix("notify:") {
                    let shown = notify::show("bricolo".into(), text.into()).await;
                    format!("notify: {shown:?}")
                } else if let Some(program) = other.strip_prefix("shell:") {
                    match shell::run(program.into(), vec!["hi".into()], vec![]).await {
                        Ok(out) => format!(
                            "shell {program}: code {:?} stdout {:?}",
                            out.code, out.stdout
                        ),
                        Err(e) => format!("shell {program}: {e:?}"),
                    }
                } else if let Some(id) = other.strip_prefix("close:") {
                    format!("close: {:?}", window::close(id.parse().unwrap_or(0)).await)
                } else if let Some(id) = other.strip_prefix("minimize:") {
                    format!(
                        "minimize: {:?}",
                        window::set_minimized(id.parse().unwrap_or(0), true).await
                    )
                } else if let Some(name) = other.strip_prefix("quit:") {
                    format!("quit: {:?}", app::quit(name.to_string()).await)
                } else if let Some(name) = other.strip_prefix("hide-app:") {
                    format!(
                        "hide-app: {:?}",
                        app::set_hidden(name.to_string(), true).await
                    )
                } else if let Some(title) = other.strip_prefix("tile:") {
                    format!("tile: {:?}", ui::set_tile(&tile(title)))
                } else if let Some(value) = other.strip_prefix("widget:") {
                    let widget = ui::Widget {
                        title: "Caps".into(),
                        rows: vec![ui::WidgetRow {
                            label: "value".into(),
                            value: value.into(),
                        }],
                    };
                    format!("widget: {:?}", ui::set_widget(&widget))
                } else if let Some(command) = ui_panel(other) {
                    command
                } else if let Some(id) = other.strip_prefix("hide:") {
                    format!("hide: {:?}", ui::hide_panel(id))
                } else if let Some(url) = other.strip_prefix("http:") {
                    let request = http::Request {
                        method: "GET".into(),
                        url: url.into(),
                        headers: vec![],
                        body: vec![],
                        timeout_ms: Some(5000),
                    };
                    match http::send(request).await {
                        Ok(r) => format!(
                            "http {url}: {} {}",
                            r.status,
                            String::from_utf8_lossy(&r.body)
                        ),
                        Err(e) => format!("http {url}: {e:?}"),
                    }
                } else {
                    format!("unknown command {other}")
                }
            }
        };
        say(line);
    }

    async fn dispose() {}
}

export!(Caps);
