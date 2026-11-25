//! world-clock: the time in other cities, in a widget on the hub. The
//! switches in its card pick the cities; the choice survives reloads in kv.
//!
//! A port of the world_clock.lua script. `domain.rs` holds the logic as
//! plain functions, tested with `cargo test`. This file is the adapter: it
//! calls the capabilities and passes their results to the domain.
//!
//! Build:   cargo build --target wasm32-wasip2 --release
//! Check:   bricolo check .
//! Install: bricolo install . -y

// The generated `event` bindings take more than seven arguments.
#![allow(clippy::too_many_arguments)]

mod domain;

wit_bindgen::generate!({
    path: "wit",
    world: "bricolo:stdlib/plugin",
});

use bricolo::stdlib::log::{self, Level};
use bricolo::stdlib::types::UiEvent;
use bricolo::stdlib::{kv, shell, timer, ui};

struct Plugin;

fn setting() -> String {
    kv::get("cities").unwrap_or_else(|| domain::DEFAULT.into())
}

/// One `/bin/date` run per zone, with TZ picking the zone.
async fn times(cities: &[(&str, &str)]) -> Vec<Option<String>> {
    let mut out = Vec::with_capacity(cities.len());
    for (_, zone) in cities {
        let env = vec![("TZ".to_string(), zone.to_string())];
        match shell::run("/bin/date".into(), vec!["+%H:%M".into()], env).await {
            Ok(r) if r.code == Some(0) => out.push(Some(r.stdout.trim().to_string())),
            Ok(r) => {
                log::write(
                    Level::Warn,
                    &format!("cannot read the time in {zone}: {}", r.stderr.trim()),
                );
                out.push(None);
            }
            Err(e) => {
                log::write(Level::Warn, &format!("cannot read the time: {e:?}"));
                out.push(None);
            }
        }
    }
    out
}

async fn draw() {
    let picked = domain::parse(&setting());
    let rows: Vec<ui::WidgetRow> = domain::rows(&picked, &times(&picked).await)
        .into_iter()
        .map(|(label, value)| ui::WidgetRow { label, value })
        .collect();
    let shown = if rows.is_empty() {
        ui::clear_widget()
    } else {
        ui::set_widget(&ui::Widget {
            title: "World clock".into(),
            rows,
        })
    };
    if let Err(e) = shown {
        log::write(Level::Warn, &format!("cannot set the widget: {e:?}"));
    }
    let switches = domain::CITIES
        .iter()
        .map(|(name, _)| ui::Switch {
            id: name.to_string(),
            label: name.to_string(),
            on: picked.iter().any(|(p, _)| p == name),
        })
        .collect();
    let tile = ui::Tile {
        title: "Cities".into(),
        status_text: String::new(),
        switches,
        buttons: vec![],
    };
    if let Err(e) = ui::set_tile(&tile) {
        log::write(Level::Warn, &format!("cannot set the tile: {e:?}"));
    }
}

impl Guest for Plugin {
    /// `config` is this plugin's settings as JSON.
    async fn init(config: String) -> Result<(), String> {
        let settings = domain::Settings::from_json(&config);
        draw().await;
        timer::every(settings.every_ms);
        log::write(Level::Info, &format!("showing {}", setting()));
        Ok(())
    }

    async fn on_event(event: Event) {
        match event {
            Event::Timer(_) => draw().await,
            Event::Ui(UiEvent::Switch(s)) => {
                if let Err(e) = kv::set("cities", &domain::toggled(&setting(), &s.id, s.on)) {
                    log::write(Level::Warn, &format!("cannot save the cities: {e:?}"));
                }
                draw().await;
            }
            _ => {}
        }
    }

    async fn dispose() {}
}

export!(Plugin);
