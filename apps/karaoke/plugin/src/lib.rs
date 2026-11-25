//! karaoke: the lyrics of whatever is playing, one line at a time, in a
//! caption at the bottom of the screen. cmd+alt+shift+k or the tile's
//! switch turns it on and off; it starts off, and the choice survives
//! reloads in kv. The tile's buttons move the lyrics earlier or later.
//!
//! A port of the karaoke.lua script and its lib/karaoke.lua. What is
//! playing comes from `media-control get`, asked once a second; lyrics come
//! from LRCLIB. `domain.rs` holds the logic as plain functions, tested with
//! `cargo test`. This file is the adapter: it calls the capabilities and
//! passes their results to the domain.
//!
//! Build:   cargo build --target wasm32-wasip2 --release
//! Check:   bricolo check .
//! Install: bricolo install . -y
//! Try it:  bricolo fire karaoke h1

// The generated `event` bindings take more than seven arguments.
#![allow(clippy::too_many_arguments)]

mod domain;

use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

wit_bindgen::generate!({
    path: "wit",
    world: "bricolo:stdlib/plugin",
});

use domain::{Karaoke, Lookup, Next, Settings};
use bricolo::stdlib::log::{self, Level};
use bricolo::stdlib::types::UiEvent;
use bricolo::stdlib::{hotkey, http, kv, shell, timer, ui};

/// The binding id, the one the script's `bricolo.hotkey.bind` got.
const HOTKEY: &str = "h1";
const MEDIA_CONTROL: &str = "/opt/homebrew/bin/media-control";
const USER_AGENT: &str = "bricolo-karaoke (https://github.com/Pierre-Mike/bricolo)";

static SETTINGS: OnceLock<Settings> = OnceLock::new();
static RUN: LazyLock<Mutex<Run>> = LazyLock::new(Mutex::default);

/// Everything that lives between events.
#[derive(Default)]
struct Run {
    karaoke: Karaoke,
    ticker: Option<u64>,
    ticks: u64,
    lookup: Option<Lookup>,
    /// The timer that starts or retries the lookup's next request.
    lookup_timer: Option<u64>,
    caption: Option<String>,
    shown_text: Option<String>,
}

fn with<R>(f: impl FnOnce(&mut Run) -> R) -> R {
    f(&mut RUN.lock().unwrap())
}

fn settings() -> &'static Settings {
    SETTINGS.get_or_init(|| Settings::from_json("{}"))
}

fn enabled() -> bool {
    kv::get("enabled").as_deref() == Some("on")
}

/// Seconds since the epoch, with a fraction.
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn hide() {
    with(|r| {
        if let Some(id) = r.caption.take() {
            let _ = ui::hide_panel(&id);
            r.shown_text = None;
        }
    });
}

fn show(text: String) {
    with(|r| {
        if r.shown_text.as_ref() == Some(&text) {
            return;
        }
        r.shown_text = Some(text.clone());
        let panel = ui::Panel {
            kind: ui::PanelKind::Caption,
            title: String::new(),
            text,
            choices: vec![],
            timeout_ms: None,
        };
        if let Ok(id) = ui::show_panel(&panel) {
            r.caption = Some(id);
        }
    });
}

fn draw_tile() {
    let status = with(|r| r.karaoke.tile_status(enabled()));
    let tile = ui::Tile {
        title: "Karaoke".into(),
        status_text: status,
        switches: vec![ui::Switch {
            id: "on".into(),
            label: "Show lyrics".into(),
            on: enabled(),
        }],
        buttons: vec![
            ui::Button {
                id: "earlier".into(),
                label: "Lyrics earlier".into(),
            },
            ui::Button {
                id: "later".into(),
                label: "Lyrics later".into(),
            },
        ],
    };
    if let Err(e) = ui::set_tile(&tile) {
        log::write(Level::Warn, &format!("cannot set the tile: {e:?}"));
    }
}

/// One GET from LRCLIB: the status, 0 when it never answered, and the body.
async fn get(url: String) -> (u16, String) {
    let request = http::Request {
        method: "GET".into(),
        url,
        headers: vec![("User-Agent".into(), USER_AGENT.into())],
        body: vec![],
        timeout_ms: Some(10_000),
    };
    match http::send(request).await {
        Ok(r) => (r.status, String::from_utf8_lossy(&r.body).into_owned()),
        Err(e) => {
            log::write(Level::Warn, &format!("cannot reach LRCLIB: {e:?}"));
            (0, String::new())
        }
    }
}

/// Runs the lookup from `next` until it waits, ends or stops.
async fn drive(mut next: Next) {
    loop {
        match next {
            Next::Request(url) => {
                let (status, body) = get(url).await;
                next = with(|r| {
                    let playing = r.karaoke.key().map(String::from);
                    match r.lookup.as_mut() {
                        Some(l) => l.answer(status, &body, playing.as_deref()),
                        None => Next::Stop,
                    }
                });
            }
            Next::Wait(ms) => {
                with(|r| r.lookup_timer = Some(timer::after(ms)));
                return;
            }
            Next::Found(lyrics) => {
                with(|r| {
                    let Some(song) = r.lookup.take().map(|l| l.song) else {
                        return;
                    };
                    log::write(
                        Level::Info,
                        &format!(
                            "lyrics for {}: {}",
                            domain::one_line(&song.key),
                            domain::status(Some(&song), Some(&lyrics))
                        ),
                    );
                    r.karaoke.found(&song.key, lyrics);
                });
                draw_tile();
                return;
            }
            Next::GaveUp(lyrics) => {
                let t = now();
                with(|r| {
                    if let Some(l) = r.lookup.take() {
                        r.karaoke.gave_up(&l.song.key, lyrics, t);
                    }
                });
                draw_tile();
                return;
            }
            Next::Stop => {
                with(|r| r.lookup = None);
                return;
            }
        }
    }
}

/// The lookup's timer fired: its next request.
async fn resume_lookup() {
    let next = with(|r| {
        r.lookup_timer = None;
        let playing = r.karaoke.key().map(String::from);
        r.lookup.as_mut().map(|l| l.resume(playing.as_deref()))
    });
    if let Some(next) = next {
        drive(next).await;
    }
}

/// What is playing, from `media-control`.
async fn read_media() {
    let args = vec!["get".into(), "--no-artwork".into(), "--micros".into()];
    let np = match shell::run(MEDIA_CONTROL.into(), args, vec![]).await {
        Ok(r) if r.code == Some(0) => serde_json::from_str(&r.stdout)
            .ok()
            .and_then(|raw| domain::normalize(&raw)),
        _ => None,
    };
    let t = now();
    let Some(new) = with(|r| r.karaoke.on_playing(np, t)) else {
        return;
    };
    let song = with(|r| r.karaoke.np.clone()).expect("a new song is playing");
    log::write(
        Level::Info,
        &format!("playing {}", domain::one_line(&song.key)),
    );
    draw_tile();
    if new.needs_lookup {
        with(|r| {
            if let Some(id) = r.lookup_timer.take() {
                timer::cancel(id);
            }
            r.lookup = Some(Lookup::new(song, settings().get_tries));
            r.lookup_timer = Some(timer::after(10));
        });
    }
}

async fn tick() {
    let s = settings();
    let read = with(|r| {
        r.ticks += 1;
        (r.ticks - 1) % s.poll_every == 0
    });
    if read {
        read_media().await;
    }
    let t = now();
    match with(|r| r.karaoke.caption(t, enabled(), s)) {
        Some(text) => show(text),
        None => hide(),
    }
}

fn start() {
    with(|r| {
        if r.ticker.is_none() {
            r.ticks = 0;
            r.ticker = Some(timer::every(settings().tick_ms));
        }
    });
}

fn stop() {
    if let Some(id) = with(|r| r.ticker.take()) {
        timer::cancel(id);
    }
    hide();
}

fn set_enabled(on: bool) {
    if let Err(e) = kv::set("enabled", if on { "on" } else { "off" }) {
        log::write(Level::Warn, &format!("cannot save the switch: {e:?}"));
    }
    if on {
        start();
    } else {
        stop();
    }
    draw_tile();
}

struct Plugin;

impl Guest for Plugin {
    /// `config` is this plugin's settings as JSON.
    async fn init(config: String) -> Result<(), String> {
        let s = SETTINGS.get_or_init(|| Settings::from_json(&config));
        if let Err(e) = hotkey::bind(HOTKEY.into(), s.chord.clone()).await {
            log::write(
                Level::Warn,
                &format!("karaoke: cannot bind {}: {e:?}", s.chord),
            );
        }
        draw_tile();
        if enabled() {
            start();
        }
        log::write(
            Level::Info,
            &format!(
                "karaoke: press {} to show lyrics of what is playing",
                s.chord
            ),
        );
        Ok(())
    }

    async fn on_event(event: Event) {
        match event {
            Event::Timer(id) if with(|r| r.ticker == Some(id)) => tick().await,
            Event::Timer(id) if with(|r| r.lookup_timer == Some(id)) => resume_lookup().await,
            Event::Hotkey(id) if id == HOTKEY => set_enabled(!enabled()),
            Event::Ui(UiEvent::Switch(s)) => set_enabled(s.on),
            Event::Ui(UiEvent::Button(id)) => {
                // Lyrics shown too late need the position moved forward.
                let nudge = settings().nudge;
                let delta = if id == "earlier" { nudge } else { -nudge };
                if with(|r| r.karaoke.nudge(delta)) {
                    draw_tile();
                }
            }
            _ => {}
        }
    }

    async fn dispose() {
        // The node stops this plugin's timers and panels itself.
    }
}

export!(Plugin);
