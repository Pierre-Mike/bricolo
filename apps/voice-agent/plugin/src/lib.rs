//! voice-agent: the thin side of the pipecat voice agent. ctrl+alt+cmd+v
//! starts or ends a call; the hub tile shows the daemon's state, picks the
//! voice, restarts the daemon and runs the daily review. The speed widget
//! shows where the wait between you and the agent goes, read from the
//! daemon's latency log (`speed.rs`).
//!
//! A port of the voice_agent.lua script. The daemon (python -m
//! voice_agent.daemon on port 8792) runs detached, so reloading the plugin
//! or restarting the node never ends a call or a Claude Code job. A daemon
//! that already answers is adopted, not restarted.
//!
//! `domain.rs` holds the logic as plain functions, tested with
//! `cargo test`. This file is the adapter: it calls the capabilities and
//! passes their results to the domain.
//!
//! Build:   cargo build --target wasm32-wasip2 --release
//! Check:   bricolo check .
//! Install: bricolo install . -y
//! Try it:  bricolo fire voice-agent h1

// The generated `event` bindings take more than seven arguments.
#![allow(clippy::too_many_arguments)]

mod domain;
mod speed;

use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

wit_bindgen::generate!({
    path: "wit",
    world: "bricolo:stdlib/plugin",
});

use bricolo::stdlib::log::{self, Level};
use bricolo::stdlib::types::UiEvent;
use bricolo::stdlib::{disk, hotkey, http, kv, shell, timer, ui};

/// The binding id, the one the script's only `bricolo.hotkey.bind` got.
const TOGGLE: &str = "h1";

static SETTINGS: OnceLock<domain::Settings> = OnceLock::new();
static AGENT: LazyLock<Mutex<domain::Agent>> = LazyLock::new(Default::default);
/// The latency log the speed widget last showed, and its size, so a poll
/// that finds the same file redraws nothing.
static SPEED_SEEN: Mutex<Option<(String, usize)>> = Mutex::new(None);
static TIMERS: Mutex<Timers> = Mutex::new(Timers {
    poller: None,
    polls: Vec::new(),
    review: None,
    choice: None,
});

/// The timers and the open voice list, which the script kept in closures.
struct Timers {
    /// The next heartbeat.
    poller: Option<u64>,
    /// The checks after a launch, at 5, 10, 20 and 40 s.
    polls: Vec<u64>,
    /// The daily review check, every minute.
    review: Option<u64>,
    /// The voice list's panel id and the voice behind each entry.
    choice: Option<(String, Vec<String>)>,
}

struct Plugin;

fn settings() -> domain::Settings {
    SETTINGS.get().cloned().unwrap_or_default()
}

fn warn(message: &str) {
    log::write(Level::Warn, message);
}

fn info(message: &str) {
    log::write(Level::Info, message);
}

fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn toast(text: &str) {
    let _ = ui::show_panel(&ui::Panel {
        kind: ui::PanelKind::Toast,
        title: String::new(),
        text: text.into(),
        choices: Vec::new(),
        timeout_ms: None,
    });
}

async fn get(url: String, timeout_ms: Option<u32>) -> Option<http::Response> {
    let request = http::Request {
        method: "GET".into(),
        url,
        headers: Vec::new(),
        body: Vec::new(),
        timeout_ms,
    };
    http::send(request).await.ok()
}

async fn post(path: &str, body: Option<String>) -> Option<http::Response> {
    let request = http::Request {
        method: "POST".into(),
        url: format!("{}{path}", settings().base_url),
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: body.unwrap_or_else(|| "{}".into()).into_bytes(),
        timeout_ms: Some(10 * 1000),
    };
    match http::send(request).await {
        Ok(r) => Some(r),
        Err(e) => {
            warn(&format!("POST {path}: {e:?}"));
            None
        }
    }
}

async fn run(program: &str, args: &[&str], env: &[(&str, &str)]) -> Option<shell::Output> {
    let args = args.iter().map(|a| a.to_string()).collect();
    let env = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    match shell::run(program.into(), args, env).await {
        Ok(out) => Some(out),
        Err(e) => {
            warn(&format!("cannot run {program}: {e:?}"));
            None
        }
    }
}

/// The pids listening on the daemon's port.
async fn listeners(port: u16) -> Vec<String> {
    let tcp = format!("-tiTCP:{port}");
    run("/usr/sbin/lsof", &[&tcp, "-sTCP:LISTEN"], &[])
        .await
        .map(|out| domain::pids(&out.stdout))
        .unwrap_or_default()
}

/// Stops whatever listens on the port, with SIGKILL after 10 s.
async fn stop_listeners(port: u16) {
    let pids = listeners(port).await;
    if pids.is_empty() {
        return;
    }
    let args: Vec<&str> = pids.iter().map(String::as_str).collect();
    run("/bin/kill", &args, &[]).await;
    for _ in 0..50 {
        if listeners(port).await.is_empty() {
            return;
        }
        timer::sleep(200).await;
    }
    let pids = listeners(port).await;
    if !pids.is_empty() {
        let args: Vec<&str> = ["-9"]
            .into_iter()
            .chain(pids.iter().map(String::as_str))
            .collect();
        run("/bin/kill", &args, &[]).await;
    }
}

async fn launch(force: bool) {
    let s = settings();
    let python = s.python();
    let exists = run("/bin/test", &["-x", &python], &[]).await;
    if !exists.is_some_and(|out| out.code == Some(0)) {
        toast(&format!("Voice agent: run `uv sync` in {}", s.project));
        return;
    }
    if force && let Some(port) = s.port() {
        stop_listeners(port).await;
    }
    let env = [("USER", s.user.as_str()), ("PYTHONUNBUFFERED", "1")];
    run("/bin/sh", &["-c", &domain::daemon_command(&s)], &env).await;
    AGENT.lock().unwrap().launched_at = Some(now_s());
    info(&format!(
        "launching the daemon (detached, output in {})",
        s.daemon_log
    ));
    let ids: Vec<u64> = [5, 10, 20, 40]
        .iter()
        .map(|secs| timer::after(secs * 1000))
        .collect();
    TIMERS.lock().unwrap().polls.extend(ids);
}

async fn poll() {
    let s = settings();
    let response = get(format!("{}/status", s.base_url), Some(5000)).await;
    let status = response
        .filter(|r| r.status == 200)
        .and_then(|r| domain::parse_status(&r.body));
    let seen = AGENT.lock().unwrap().observe(status, now_s(), &s);
    if let Some((old, new)) = seen.changed {
        info(&format!("state {} -> {}", old.name(), new.name()));
    }
    if let Some(n) = seen.relaunch {
        warn(&format!(
            "daemon not answering; relaunch {n} of {}",
            s.relaunches
        ));
        launch(true).await;
    }
    draw();
    draw_speed().await;
}

/// The speed widget, from the newest latency log. Turns are written as
/// they end, so this follows a call while it happens.
async fn draw_speed() {
    let s = settings();
    let dir = s.latency_dir();
    let names = disk::list(dir.clone()).await.unwrap_or_default();
    let Some(name) = speed::latest_log(&names).map(String::from) else {
        set_speed(vec![
            ("Turns".into(), "no latency log".into()),
            ("Turn it on".into(), "LATENCY_LOG=1".into()),
        ]);
        return;
    };
    let body = match disk::read(format!("{dir}/{name}")).await {
        Ok(body) => body,
        Err(e) => {
            warn(&format!("cannot read {name}: {e:?}"));
            return;
        }
    };
    let seen = Some((name.clone(), body.len()));
    {
        let mut last = SPEED_SEEN.lock().unwrap();
        if *last == seen {
            return;
        }
        *last = seen;
    }
    let day = speed::log_day(&name).unwrap_or_default();
    set_speed(speed::rows(&speed::parse_turns(&body), s.speed_turns, day));
}

fn set_speed(rows: Vec<(String, String)>) {
    let widget = ui::Widget {
        title: "Voice agent speed".into(),
        rows: rows
            .into_iter()
            .map(|(label, value)| ui::WidgetRow { label, value })
            .collect(),
    };
    if let Err(e) = ui::set_widget(&widget) {
        warn(&format!("cannot set the speed widget: {e:?}"));
    }
}

/// Polls every 2 s during a call, every 10 s otherwise.
async fn heartbeat() {
    poll().await;
    let live = AGENT.lock().unwrap().state == domain::State::Live;
    let id = timer::after(if live { 2000 } else { 10000 });
    TIMERS.lock().unwrap().poller = Some(id);
}

async fn set_voice(voice: &str) {
    if let Err(e) = kv::set("voice", voice) {
        warn(&format!("cannot save the voice: {e:?}"));
    }
    if let Some(r) = post("/voice", Some(domain::voice_body(voice))).await
        && r.status != 200
    {
        warn(&format!(
            "the daemon refused the voice: {} {}",
            r.status,
            String::from_utf8_lossy(&r.body)
        ));
    }
    poll().await;
}

async fn pick_voice() {
    let response = get(format!("{}/voices", settings().pocket_tts_url), None).await;
    let voices = response
        .filter(|r| r.status == 200)
        .and_then(|r| domain::parse_voices(&r.body));
    let Some(voices) = voices else {
        toast("pocket-tts gave no voices");
        return;
    };
    let status = AGENT.lock().unwrap().status.clone();
    let current = domain::current_voice(status.as_ref(), kv::get("voice"));
    let (values, labels) = domain::voice_choices(&voices, &current);
    let panel = ui::Panel {
        kind: ui::PanelKind::ChoiceList,
        title: "Voice of the agent".into(),
        text: String::new(),
        choices: labels,
        timeout_ms: None,
    };
    match ui::show_panel(&panel) {
        Ok(id) => TIMERS.lock().unwrap().choice = Some((id, values)),
        Err(e) => warn(&format!("cannot show the voices: {e:?}")),
    }
}

/// The local "YYYY-MM-DD HH:MM", from date.
async fn local_now() -> Option<String> {
    let out = run("/bin/date", &["+%Y-%m-%d %H:%M"], &[]).await?;
    (out.code == Some(0)).then_some(out.stdout)
}

async fn run_review() {
    let s = settings();
    let env = [("PYTHONUNBUFFERED", "1")];
    run("/bin/sh", &["-c", &domain::review_command(&s)], &env).await;
    if let Some(now) = local_now().await
        && let Some(day) = domain::day_of(&now)
        && let Err(e) = kv::set("last_review", day)
    {
        warn(&format!("cannot save the review day: {e:?}"));
    }
    info(&format!("daily review started (output in {})", s.retro_log));
}

async fn open_review() {
    let dir = settings().retro_dir();
    let names = disk::list(dir.clone()).await.unwrap_or_default();
    let Some(latest) = domain::latest_review(&names) else {
        toast("No review yet");
        return;
    };
    run("/usr/bin/open", &[&format!("{dir}/{latest}")], &[]).await;
}

/// Once a day at retro_at, or at the first check after it.
async fn review_due() {
    let Some(now) = local_now().await else {
        return;
    };
    let last = kv::get("last_review");
    if domain::review_due(&now, &settings().retro_at, last.as_deref()) {
        run_review().await;
    }
}

fn draw() {
    let (state, status) = {
        let agent = AGENT.lock().unwrap();
        (agent.state, agent.status.clone())
    };
    let voice = domain::current_voice(status.as_ref(), kv::get("voice"));
    let button = |id: &str, label: String| ui::Button {
        id: id.into(),
        label,
    };
    let tile = ui::Tile {
        title: "Voice agent".into(),
        status_text: domain::describe(state, status.as_ref()),
        switches: vec![ui::Switch {
            id: "call".into(),
            label: "Call".into(),
            on: state == domain::State::Live,
        }],
        buttons: vec![
            button("voice", format!("Voice: {}", domain::voice_label(&voice))),
            button("review", "Run the daily review".into()),
            button("open", "Open the latest review".into()),
            button("restart", "Restart the daemon".into()),
        ],
    };
    if let Err(e) = ui::set_tile(&tile) {
        warn(&format!("cannot set the tile: {e:?}"));
    }
}

impl Guest for Plugin {
    /// `config` is this plugin's settings as JSON.
    async fn init(config: String) -> Result<(), String> {
        let s = SETTINGS.get_or_init(|| domain::Settings::from_json(&config));
        if let Err(e) = hotkey::bind(TOGGLE.into(), s.chord.clone()).await {
            warn(&format!("voice_agent: cannot bind {}: {e:?}", s.chord));
        }
        match get(format!("{}/health", s.base_url), Some(3000)).await {
            Some(h) if h.status == 200 => {
                info("daemon already healthy; adopted (session and jobs kept)")
            }
            _ => launch(false).await,
        }
        heartbeat().await;
        TIMERS.lock().unwrap().review = Some(timer::every(60 * 1000));
        info(&format!(
            "voice_agent: press {} to start or end a call",
            s.chord
        ));
        Ok(())
    }

    async fn on_event(event: Event) {
        match event {
            Event::Hotkey(id) if id == TOGGLE => {
                let state = AGENT.lock().unwrap().state;
                info(&format!("toggle requested (state {})", state.name()));
                post("/toggle", None).await;
                poll().await;
            }
            Event::Timer(id) => {
                let (beat, launch_poll, review) = {
                    let mut t = TIMERS.lock().unwrap();
                    let launch_poll = t.polls.contains(&id);
                    t.polls.retain(|p| *p != id);
                    (t.poller == Some(id), launch_poll, t.review == Some(id))
                };
                if beat {
                    heartbeat().await;
                } else if launch_poll {
                    poll().await;
                } else if review {
                    review_due().await;
                }
            }
            Event::Ui(UiEvent::Switch(s)) if s.id == "call" => {
                post(if s.on { "/start" } else { "/stop" }, None).await;
                poll().await;
            }
            Event::Ui(UiEvent::Button(id)) => match id.as_str() {
                "voice" => pick_voice().await,
                "review" => run_review().await,
                "open" => open_review().await,
                "restart" => {
                    AGENT.lock().unwrap().relaunches = 0;
                    launch(true).await;
                }
                _ => {}
            },
            Event::Ui(UiEvent::Choice(c)) => {
                let picked = {
                    let mut t = TIMERS.lock().unwrap();
                    match t.choice.take() {
                        Some((panel, values)) if panel == c.panel => {
                            c.index.and_then(|i| values.get(i as usize).cloned())
                        }
                        other => {
                            t.choice = other;
                            None
                        }
                    }
                };
                if let Some(voice) = picked {
                    set_voice(&voice).await;
                }
            }
            _ => {}
        }
    }

    async fn dispose() {
        if let Some(id) = TIMERS.lock().unwrap().poller {
            timer::cancel(id);
        }
    }
}

export!(Plugin);
