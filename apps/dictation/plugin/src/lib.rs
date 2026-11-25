//! dictation: hold Fn to talk and let go to stop, or press ctrl+alt+shift+d
//! to start recording and again to stop. The take is transcribed by the
//! speech server, cleaned, shown in the review bar at the bottom of the
//! screen, and pasted at the cursor on ↩ (at once when the tile's review
//! switch is off). The
//! tile rewrites the last take with Claude (Clean up, Improve, Formal,
//! Bullets and the rest), with an optional
//! ~/.config/bricolo/text_actions.json for presets.
//!
//! A port of the dictation.lua script. `domain.rs` holds the state machine
//! and the ports of lib/transcript_filter.lua and lib/text_actions.lua,
//! tested with `cargo test`. This file is the adapter: it turns events into
//! domain inputs, carries out the actions the domain returns, and feeds
//! their results back.
//!
//! Fn needs the Input Monitoring permission (`bricolo permissions --request`),
//! and System Settings, Keyboard, "Press 🌐 key to" set to "Do Nothing".
//!
//! Build:   cargo build --target wasm32-wasip2 --release
//! Check:   bricolo check .
//! Install: bricolo install . -y
//! Try it:  bricolo fire dictation h1   (twice: start, then stop)

// The generated `event` bindings take more than seven arguments.
#![allow(clippy::too_many_arguments)]

mod domain;

use std::collections::VecDeque;
use std::future::poll_fn;
use std::pin::pin;
use std::sync::{Mutex, OnceLock};
use std::task::Poll;
use std::time::Instant;

wit_bindgen::generate!({
    path: "wit",
    world: "bricolo:stdlib/plugin",
});

use domain::{Action, Config, Dictation, Input, Settings};
use bricolo::stdlib::log::{self, Level};
use bricolo::stdlib::types::{CallError, UiEvent};
use bricolo::stdlib::{audio, clipboard, disk, hotkey, keyboard, kv, mic, shell, speech, timer, ui};

/// The binding ids the script got from `bricolo.hotkey.bind`, in its order.
const CHORD_ID: &str = "h1";
const HOLD_ID: &str = "h2";
/// How long `Settle` waits; events queued before it are dropped.
const SETTLE_MS: u32 = 10;
/// On: a take shows in the review bar at the bottom of the screen, and ↩
/// pastes it. The tile's switch turns review off, to paste at once.
const REVIEW_DEFAULT: bool = true;

static SETTINGS: OnceLock<Settings> = OnceLock::new();
static MACHINE: Mutex<Option<Dictation>> = Mutex::new(None);
/// The hold timers still to fire; the script never cancels them.
static HOLD_TIMERS: Mutex<Vec<u64>> = Mutex::new(Vec::new());
/// The safety stop's timer, while one runs.
static SAFETY: Mutex<Option<u64>> = Mutex::new(None);
static SETTLE: Mutex<Option<u64>> = Mutex::new(None);
/// The timer that primes the mic after `init`, until it fires.
static PRIME: Mutex<Option<u64>> = Mutex::new(None);
/// When the last stop began, to log how long the release took to paste.
static STOPPED_AT: Mutex<Option<Instant>> = Mutex::new(None);
/// The start sound's volume, 0 to 100.
const START_SOUND_VOLUME: u8 = 40;
/// The caption's panel id while recording, transcribing or rewriting.
static CAPTION: Mutex<Option<String>> = Mutex::new(None);
/// The rewrite choice list's take and presets, and the pick.
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

struct Pending {
    take: String,
    cfg: Config,
    picked: usize,
}

struct Plugin;

fn settings() -> Settings {
    SETTINGS.get().cloned().unwrap_or_default()
}

fn setting(key: &str, default: bool) -> bool {
    domain::switch_on(kv::get(key).as_deref(), default)
}

fn describe(e: CallError) -> String {
    match e {
        CallError::Failed(msg) => msg,
        other => format!("{other:?}"),
    }
}

fn show(kind: ui::PanelKind, title: &str, text: String, choices: Vec<String>) -> Option<String> {
    let panel = ui::Panel {
        kind,
        title: title.into(),
        text,
        choices,
        timeout_ms: None,
    };
    ui::show_panel(&panel).ok()
}

fn toast(text: String) {
    show(ui::PanelKind::Toast, "", text, vec![]);
}

fn hide_caption() {
    if let Some(id) = CAPTION.lock().unwrap().take() {
        let _ = ui::hide_panel(&id);
    }
}

fn draw() {
    let s = settings();
    let switch = |id: &str, label: &str, default: bool| ui::Switch {
        id: id.into(),
        label: label.into(),
        on: setting(id, default),
    };
    let button = |id: &str, label: &str| ui::Button {
        id: id.into(),
        label: label.into(),
    };
    let tile = ui::Tile {
        title: "Dictation".into(),
        status_text: domain::tile_status(&s),
        switches: vec![
            switch("review", "Review before pasting", REVIEW_DEFAULT),
            switch("pause_media", "Pause music while recording", true),
        ],
        buttons: vec![
            button("rewrite", "Rewrite the last take…"),
            button("copy", "Copy the last take"),
        ],
    };
    if let Err(e) = ui::set_tile(&tile) {
        log::write(
            Level::Warn,
            &format!("cannot set the tile: {}", describe(e)),
        );
    }
}

impl Guest for Plugin {
    /// `config` is this plugin's settings as JSON.
    async fn init(config: String) -> Result<(), String> {
        let s = SETTINGS.get_or_init(|| Settings::from_json(&config));
        *MACHINE.lock().unwrap() = Some(Dictation::new(s.clone()));
        if let Err(e) = hotkey::bind(CHORD_ID.into(), s.chord.clone()).await {
            log::write(
                Level::Warn,
                &format!("dictation: cannot bind {}: {}", s.chord, describe(e)),
            );
        }
        if let Some(hold) = &s.hold
            && let Err(e) = hotkey::bind(HOLD_ID.into(), hold.clone()).await
        {
            log::write(
                Level::Warn,
                &format!(
                    "dictation: cannot bind {hold}: {}; run bricolo permissions --request",
                    describe(e)
                ),
            );
        }
        draw();
        // Not in init itself: the first open can take longer than init's
        // budget.
        if s.prime_mic {
            *PRIME.lock().unwrap() = Some(timer::after(0));
        }
        log::write(
            Level::Info,
            &format!(
                "dictation: hold {} or press {} to record",
                s.hold.as_deref().unwrap_or("nil"),
                s.chord
            ),
        );
        Ok(())
    }

    async fn on_event(event: Event) {
        let input = match event {
            Event::Hotkey(id) if id == CHORD_ID => Input::Toggle,
            Event::Hotkey(id) if id == HOLD_ID => Input::Press,
            Event::HotkeyReleased(id) if id == HOLD_ID => Input::Release,
            Event::Timer(id) if take_if(&PRIME, id) => {
                prime_mic().await;
                return;
            }
            Event::Timer(id) => {
                let mut holds = HOLD_TIMERS.lock().unwrap();
                if let Some(i) = holds.iter().position(|t| *t == id) {
                    holds.remove(i);
                    Input::HoldTimeUp
                } else if take_if(&SAFETY, id) {
                    Input::TimeUp
                } else if take_if(&SETTLE, id) {
                    Input::Settled
                } else {
                    return;
                }
            }
            Event::Ui(UiEvent::Switch(s)) => {
                let _ = kv::set(&s.id, if s.on { "on" } else { "off" });
                draw();
                return;
            }
            Event::Ui(UiEvent::Button(id)) if id == "rewrite" => Input::RewriteAsked {
                has_take: kv::get("last_take").is_some(),
            },
            Event::Ui(UiEvent::Button(_)) => {
                if let Some(take) = kv::get("last_take") {
                    let _ = clipboard::set(take).await;
                    toast("Copied the last take".into());
                }
                return;
            }
            Event::Ui(UiEvent::Choice(c)) => {
                let mut pending = PENDING.lock().unwrap();
                let (Some(p), Some(i)) = (pending.as_mut(), c.index) else {
                    return;
                };
                let Some(preset) = p.cfg.presets.get(i as usize) else {
                    return;
                };
                p.picked = i as usize;
                Input::RewritePicked(preset.label.clone())
            }
            Event::Ui(UiEvent::Review(r)) => {
                if let Some(text) = domain::accepted(r.accepted, &r.text) {
                    let _ = kv::set("last_take", text);
                    paste(text).await;
                }
                return;
            }
            _ => return,
        };
        run(input).await;
    }

    async fn dispose() {
        let actions = match MACHINE.lock().unwrap().as_mut() {
            Some(machine) => machine.dispose(),
            None => return,
        };
        for action in actions {
            execute(action).await;
        }
    }
}

/// Takes the timer id in `slot` if it is `id`.
fn take_if(slot: &Mutex<Option<u64>>, id: u64) -> bool {
    let mut slot = slot.lock().unwrap();
    if *slot == Some(id) {
        slot.take();
        true
    } else {
        false
    }
}

/// Hands `input` to the domain and carries out its actions, until none
/// produces a new input.
async fn run(input: Input) {
    let mut queue = VecDeque::from([input]);
    while let Some(input) = queue.pop_front() {
        let actions = match MACHINE.lock().unwrap().as_mut() {
            Some(machine) => machine.handle(input),
            None => return,
        };
        for action in actions {
            queue.extend(execute(action).await);
        }
    }
}

/// Runs `a` and `b` at the same time and waits for both. The host runs
/// their capability calls concurrently.
async fn both<A: Future, B: Future>(a: A, b: B) -> (A::Output, B::Output) {
    let mut a = pin!(a);
    let mut b = pin!(b);
    let (mut ra, mut rb) = (None, None);
    poll_fn(|cx| {
        if ra.is_none()
            && let Poll::Ready(v) = a.as_mut().poll(cx)
        {
            ra = Some(v);
        }
        if rb.is_none()
            && let Poll::Ready(v) = b.as_mut().poll(cx)
        {
            rb = Some(v);
        }
        match (ra.take(), rb.take()) {
            (Some(x), Some(y)) => Poll::Ready((x, y)),
            (x, y) => {
                ra = x;
                rb = y;
                Poll::Pending
            }
        }
    })
    .await
}

/// Carries out one action. Capability calls answer with the next inputs.
async fn execute(action: Action) -> Vec<Input> {
    let s = settings();
    match action {
        Action::StartHoldTimer { ms } => HOLD_TIMERS.lock().unwrap().push(timer::after(ms)),
        Action::ResumeMedia => {
            let _ = shell::run(s.media_control, vec!["play".into()], vec![]).await;
        }
        Action::StartRecording { max_secs } => {
            let options = mic::RecordOptions {
                max_secs,
                format: mic::AudioFormat::Wav16kMono,
            };
            let begun = Instant::now();
            let open = async {
                let r = mic::record_start(options).await;
                (r, begun.elapsed().as_millis())
            };
            let (((started, open_ms), paused), ()) =
                both(both(open, pause_media(&s)), start_sound(&s)).await;
            log::write(
                Level::Info,
                &format!(
                    "dictation: mic open after {open_ms} ms, media paused too after {} ms",
                    begun.elapsed().as_millis()
                ),
            );
            // `MediaPaused` first, so a failed start knows to resume.
            return vec![
                Input::MediaPaused(paused),
                Input::Started(started.map_err(describe)),
            ];
        }
        Action::StartSafetyTimer { ms } => *SAFETY.lock().unwrap() = Some(timer::after(ms)),
        Action::CancelSafetyTimer => {
            if let Some(id) = SAFETY.lock().unwrap().take() {
                timer::cancel(id);
            }
        }
        Action::StopRecording(id) => {
            let begun = Instant::now();
            *STOPPED_AT.lock().unwrap() = Some(begun);
            let r = mic::record_stop(id).await.map_err(describe);
            log::write(
                Level::Info,
                &format!(
                    "dictation: mic stopped in {} ms",
                    begun.elapsed().as_millis()
                ),
            );
            return vec![Input::Stopped(r)];
        }
        Action::CancelRecording(id) => {
            let _ = mic::record_cancel(id).await;
        }
        Action::Transcribe(path) => {
            let begun = Instant::now();
            let r = speech::transcribe_file(path).await.map_err(describe);
            log::write(
                Level::Info,
                &format!(
                    "dictation: transcribed in {} ms",
                    begun.elapsed().as_millis()
                ),
            );
            return vec![Input::Transcribed(r)];
        }
        Action::Caption(text) => {
            if let Some(id) = show(ui::PanelKind::Caption, "", text, vec![]) {
                *CAPTION.lock().unwrap() = Some(id);
            }
        }
        Action::HideCaption => hide_caption(),
        Action::Toast(text) => toast(text),
        Action::Info(text) => log::write(Level::Info, &text),
        Action::Warn(text) => log::write(Level::Warn, &text),
        Action::Deliver(text) => deliver(text).await,
        Action::ShowRewriteChoices => show_rewrite_choices(&s).await,
        Action::Rewrite => return vec![Input::Rewritten(rewrite().await)],
        Action::Settle => *SETTLE.lock().unwrap() = Some(timer::after(SETTLE_MS)),
    }
    vec![]
}

/// Opens the mic and drops the recording at once, so the device is awake
/// for the first take. Skipped when a take already started.
async fn prime_mic() {
    let idle = MACHINE
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|m| m.state() == domain::State::Idle);
    if !idle {
        return;
    }
    let begun = Instant::now();
    let options = mic::RecordOptions {
        max_secs: 1,
        format: mic::AudioFormat::Wav16kMono,
    };
    match mic::record_start(options).await {
        Ok(id) => {
            let _ = mic::record_cancel(id).await;
            log::write(
                Level::Info,
                &format!(
                    "dictation: mic primed in {} ms",
                    begun.elapsed().as_millis()
                ),
            );
        }
        Err(e) => log::write(
            Level::Warn,
            &format!("dictation: cannot prime the mic: {}", describe(e)),
        ),
    }
}

/// Plays the start sound, if one is set. A failure is logged, never shown.
async fn start_sound(s: &Settings) {
    let Some(path) = s.start_sound.clone() else {
        return;
    };
    let options = audio::PlayOptions {
        volume: START_SOUND_VOLUME,
        looping: false,
    };
    if let Err(e) = audio::play(path.clone(), options).await {
        log::write(
            Level::Warn,
            &format!("dictation: cannot play {path}: {}", describe(e)),
        );
    }
}

/// Pauses what is playing, if anything, when the switch is on. Returns
/// whether it paused something, to resume later.
async fn pause_media(s: &Settings) -> bool {
    if !setting("pause_media", true) {
        return false;
    }
    let args = vec!["get".into(), "--no-artwork".into()];
    let title = match shell::run(s.media_control.clone(), args, vec![]).await {
        Ok(r) if r.code == Some(0) => domain::playing(&r.stdout),
        _ => None,
    };
    let Some(title) = title else {
        return false;
    };
    let _ = shell::run(s.media_control.clone(), vec!["pause".into()], vec![]).await;
    log::write(Level::Info, &format!("paused {title}"));
    true
}

async fn paste(text: &str) {
    if let Err(e) = clipboard::set(text.to_string()).await {
        log::write(Level::Warn, &format!("cannot copy: {}", describe(e)));
        return;
    }
    let (mods, key) = SETTINGS.get().cloned().unwrap_or_default().paste_keys();
    if let Err(e) = keyboard::keystroke(mods, key).await {
        hide_caption();
        toast(format!("copied; paste it yourself ({})", describe(e)));
    }
}

/// Shows `text` for review when that is on, else pastes it.
async fn deliver(text: String) {
    let _ = kv::set("last_take", &text);
    // Set by a stop, so a rewrite logs nothing.
    let stopped_at = STOPPED_AT.lock().unwrap().take();
    if !setting("review", REVIEW_DEFAULT) {
        paste(&text).await;
        if let Some(at) = stopped_at {
            log::write(
                Level::Info,
                &format!(
                    "dictation: pasted {} ms after the stop",
                    at.elapsed().as_millis()
                ),
            );
        }
        return;
    }
    show(ui::PanelKind::Review, "Dictation", text, vec![]);
}

/// The presets file merged over the built-ins. A missing or unreadable
/// file, or one that is not JSON, leaves the built-ins.
async fn presets(s: &Settings) -> Config {
    let decoded = disk::read(s.presets_file.clone())
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let cfg = domain::config(decoded.as_ref(), &s.claude);
    for w in &cfg.warnings {
        log::write(Level::Warn, &format!("text actions: {w}"));
    }
    cfg
}

async fn show_rewrite_choices(s: &Settings) {
    let Some(take) = kv::get("last_take") else {
        return;
    };
    let cfg = presets(s).await;
    let labels = cfg.presets.iter().map(|p| p.label.clone()).collect();
    show(
        ui::PanelKind::ChoiceList,
        "Rewrite the last take",
        String::new(),
        labels,
    );
    *PENDING.lock().unwrap() = Some(Pending {
        take,
        cfg,
        picked: 0,
    });
}

/// Rewrites the take with the picked preset through `claude -p`.
async fn rewrite() -> Result<String, String> {
    let (take, cfg, preset) = {
        let pending = PENDING.lock().unwrap();
        let p = pending.as_ref().ok_or("no take")?;
        let preset = p.cfg.presets.get(p.picked).ok_or("no preset")?.clone();
        (p.take.clone(), p.cfg.clone(), preset)
    };
    let args = domain::argv(&preset, &cfg, &take);
    let r = shell::run(cfg.claude.clone(), args, vec![])
        .await
        .map_err(describe)?;
    let decoded = serde_json::from_str::<serde_json::Value>(&r.stdout).ok();
    domain::parse(decoded.as_ref(), &take, &preset)
}

export!(Plugin);
