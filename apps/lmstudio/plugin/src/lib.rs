//! lmstudio: runs the LM Studio server and switches its loaded model from
//! a hub tile, through the `lms` command-line tool. Switching model unloads
//! the loaded models of the same kind first, so two 15 GB chat models never
//! sit side by side on a 32 GB Mac; an embedding model keeps serving. `lms
//! load` runs detached, because a big model takes longer than the 60
//! seconds `shell.run` allows, and the tile polls `lms ps` until it shows up.
//!
//! A port of the lmstudio.lua script. `domain.rs` holds the logic as plain
//! functions, tested with `cargo test`. This file is the adapter: it calls
//! the capabilities and passes their results to the domain.
//!
//! Build:   cargo build --target wasm32-wasip2 --release
//! Check:   bricolo check .
//! Install: bricolo install . -y

// The generated `event` bindings take more than seven arguments.
#![allow(clippy::too_many_arguments)]

mod domain;

use std::sync::{Mutex, MutexGuard, OnceLock};

wit_bindgen::generate!({
    path: "wit",
    world: "bricolo:stdlib/plugin",
});

use bricolo::stdlib::log::{self, Level};
use bricolo::stdlib::types::UiEvent;
use bricolo::stdlib::{kv, shell, timer, ui};

static SETTINGS: OnceLock<domain::Settings> = OnceLock::new();
static STATE: Mutex<Option<domain::State>> = Mutex::new(None);
static TIMERS: Mutex<Timers> = Mutex::new(Timers {
    poll: None,
    catalog: None,
    load: None,
});

/// The timer ids, and how long the load poller has run.
struct Timers {
    poll: Option<u64>,
    catalog: Option<u64>,
    load: Option<(u64, u32)>,
}

struct Plugin;

fn settings() -> domain::Settings {
    SETTINGS.get().cloned().unwrap_or_default()
}

/// The state, locked. Never hold it across an await.
fn state() -> MutexGuard<'static, Option<domain::State>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn with<T>(f: impl FnOnce(&mut domain::State) -> T) -> T {
    f(state().get_or_insert_with(domain::State::default))
}

fn timers() -> MutexGuard<'static, Timers> {
    TIMERS.lock().unwrap_or_else(|e| e.into_inner())
}

fn ttl() -> usize {
    domain::ttl_index(kv::get("ttl").as_deref())
}

/// Runs `lms` with `args`: its stdout, or the first line of why it failed.
async fn lms(args: &[&str]) -> Result<String, String> {
    let args = args.iter().map(|a| a.to_string()).collect();
    match shell::run(settings().lms, args, vec![]).await {
        Ok(r) if r.code == Some(0) => Ok(r.stdout),
        Ok(r) => Err(domain::lms_error(&r.stdout, &r.stderr)),
        Err(e) => Err(format!("{e:?}")),
    }
}

fn set_busy(line: Option<String>) {
    with(|s| s.busy = line);
}

async fn refresh(with_catalog: bool) {
    if (with_catalog || with(|s| s.catalog.is_empty()))
        && let Some(catalog) = lms(&["ls", "--json"])
            .await
            .ok()
            .and_then(|out| domain::parse_catalog(&out))
    {
        with(|s| s.catalog = catalog);
    }
    if let Ok(out) = lms(&["server", "status", "--json"]).await {
        with(|s| s.server = s.server.read(&out));
    }
    let loaded = lms(&["ps", "--json"])
        .await
        .ok()
        .and_then(|out| domain::parse_loaded(&out))
        .unwrap_or_default();
    with(|s| s.loaded = loaded);
    draw();
}

/// Logs "what: err" as a warning and shows it in a toast.
fn report(what: &str, err: &str) {
    let line = format!("{what}: {err}");
    log::write(Level::Warn, &line);
    toast(line);
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

async fn toggle_server(on: bool) {
    if with(|s| s.busy.is_some()) {
        return;
    }
    set_busy(Some(
        if on {
            "Starting the server…"
        } else {
            "Stopping the server…"
        }
        .into(),
    ));
    draw();
    let result = lms(&["server", if on { "start" } else { "stop" }]).await;
    set_busy(None);
    if let Err(e) = result {
        let what = if on {
            "Could not start the server"
        } else {
            "Could not stop the server"
        };
        report(what, &e);
    }
    refresh(false).await;
}

async fn load(key: String) {
    let (busy, name, targets) = with(|s| {
        (
            s.busy.is_some() || s.is_loaded(&key),
            s.name_of(&key),
            s.replaced(&key),
        )
    });
    if busy {
        return;
    }
    if !targets.is_empty() {
        set_busy(Some(format!("Making room for {name}…")));
        draw();
        for id in &targets {
            if let Err(e) = lms(&["unload", id]).await {
                log::write(Level::Warn, &format!("unload {id}: {e}"));
            }
        }
    }
    set_busy(Some(format!("Loading {name}…")));
    draw();
    let settings = settings();
    let secs = domain::TTLS[ttl()].1;
    let cmd = domain::load_command(&settings.lms, &key, secs, &settings.load_log);
    if let Err(e) = shell::run("/bin/sh".into(), vec!["-c".into(), cmd], vec![]).await {
        set_busy(None);
        report(&format!("Could not load {name}"), &format!("{e:?}"));
        draw();
        return;
    }
    log::write(
        Level::Info,
        &format!("loading {key} (output in {})", settings.load_log),
    );
    with(|s| s.waiting = Some(key));
    timers().load = Some((timer::every(domain::LOAD_POLL_MS), 0));
}

/// One tick of the load poller: done when the model shows up in `lms ps`
/// or the time runs out.
async fn load_tick() {
    let Some((id, started)) = timers().load else {
        return;
    };
    let started = started + domain::LOAD_POLL_MS;
    timers().load = Some((id, started));
    if let Some(loaded) = lms(&["ps", "--json"])
        .await
        .ok()
        .and_then(|out| domain::parse_loaded(&out))
    {
        with(|s| s.loaded = loaded);
    }
    let Some(key) = with(|s| s.waiting.clone()) else {
        return;
    };
    let settings = settings();
    let done = with(|s| s.is_loaded(&key));
    if !done && started < settings.load_timeout_ms {
        return;
    }
    timer::cancel(id);
    timers().load = None;
    if done {
        log::write(Level::Info, &format!("loaded {key}"));
    } else {
        let name = with(|s| s.name_of(&key));
        report(
            &format!("Could not load {name}"),
            &format!(
                "no model after {} minutes; see {}",
                settings.load_timeout_ms / 60_000,
                settings.load_log
            ),
        );
    }
    with(|s| {
        s.busy = None;
        s.waiting = None;
    });
    refresh(false).await;
}

async fn pick_model() {
    refresh(true).await;
    let (keys, labels) = with(|s| s.choices());
    if keys.is_empty() {
        toast("No chat models downloaded in LM Studio".into());
        return;
    }
    with(|s| s.picks = keys);
    let _ = ui::show_panel(&ui::Panel {
        kind: ui::PanelKind::ChoiceList,
        title: "Load a model".into(),
        text: String::new(),
        choices: labels,
        timeout_ms: None,
    });
}

async fn unload_all() {
    if with(|s| s.busy.is_some()) {
        return;
    }
    set_busy(Some("Unloading everything…".into()));
    draw();
    let result = lms(&["unload", "--all"]).await;
    set_busy(None);
    if let Err(e) = result {
        report("Could not unload", &e);
    }
    refresh(false).await;
}

async fn on_button(id: &str) {
    match id {
        "pick" => pick_model().await,
        "unload" => unload_all().await,
        "ttl" => {
            if let Err(e) = kv::set("ttl", &domain::next_ttl(kv::get("ttl").as_deref())) {
                log::write(Level::Warn, &format!("cannot save the ttl: {e:?}"));
            }
            draw();
        }
        _ if with(|s| s.busy.is_none()) => refresh(true).await,
        _ => {}
    }
}

fn button(id: &str, label: &str) -> ui::Button {
    ui::Button {
        id: id.into(),
        label: label.into(),
    }
}

fn draw() {
    let (status, running) = with(|s| (s.status(), s.server.running));
    let tile = ui::Tile {
        title: "LM Studio".into(),
        status_text: status,
        switches: vec![ui::Switch {
            id: "server".into(),
            label: "Server".into(),
            on: running,
        }],
        buttons: vec![
            button("pick", "Load a model…"),
            button("unload", "Unload all"),
            button(
                "ttl",
                &format!("Unload when idle: {}", domain::TTLS[ttl()].0),
            ),
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
        let found = shell::run(
            "/bin/test".into(),
            vec!["-x".into(), settings.lms.clone()],
            vec![],
        )
        .await
        .is_ok_and(|r| r.code == Some(0));
        if !found {
            let _ = ui::set_tile(&ui::Tile {
                title: "LM Studio".into(),
                status_text: format!("lms not found at {}", settings.lms),
                switches: vec![],
                buttons: vec![],
            });
            log::write(Level::Warn, &format!("lms not found at {}", settings.lms));
            return Ok(());
        }
        refresh(true).await;
        let mut t = timers();
        t.poll = Some(timer::every(settings.poll_ms));
        t.catalog = Some(timer::every(settings.catalog_ms));
        drop(t);
        log::write(
            Level::Info,
            &format!(
                "tile ready, {} models downloaded",
                with(|s| s.catalog.len())
            ),
        );
        Ok(())
    }

    async fn on_event(event: Event) {
        match event {
            Event::Timer(id) => {
                let (poll, catalog, load) = {
                    let t = timers();
                    (t.poll, t.catalog, t.load.map(|l| l.0))
                };
                let waiting = with(|s| s.waiting.is_some());
                if load == Some(id) {
                    load_tick().await;
                } else if poll == Some(id) && !waiting {
                    refresh(false).await;
                } else if catalog == Some(id) && !waiting {
                    refresh(true).await;
                }
            }
            Event::Ui(UiEvent::Switch(s)) => toggle_server(s.on).await,
            Event::Ui(UiEvent::Button(id)) => on_button(&id).await,
            Event::Ui(UiEvent::Choice(c)) => {
                let key = c
                    .index
                    .and_then(|i| with(|s| s.picks.get(i as usize).cloned()));
                if let Some(key) = key {
                    load(key).await;
                }
            }
            _ => {}
        }
    }

    async fn dispose() {}
}

export!(Plugin);
