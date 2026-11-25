//! mac-status: battery, memory and free disk in a widget on the hub,
//! refreshed every 10 seconds. It has no controls, so it sets no tile. Memory is Activity Monitor's "Memory Used":
//! app memory, wired and compressed.
//!
//! A port of the mac_status.lua script. `domain.rs` holds the logic as
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
use bricolo::stdlib::{shell, system, timer, ui};

struct Plugin;

/// One program's stdout. The script ran the three in one `sh -c`; here
/// each runs directly, and a failed start fails the whole probe.
async fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let args = args.iter().map(|a| a.to_string()).collect();
    match shell::run(program.into(), args, vec![]).await {
        Ok(r) => Ok(r.stdout),
        Err(e) => Err(format!("{e:?}")),
    }
}

async fn status() -> Result<Vec<(String, String)>, String> {
    let vm = run("/usr/bin/vm_stat", &[]).await?;
    let mem = run("/usr/sbin/sysctl", &["-n", "hw.memsize"]).await?;
    let df = run("/bin/df", &["-k", "/"]).await?;
    let power = system::power().await.ok();
    Ok(domain::rows(&[
        power.and_then(|p| domain::battery(p.battery, p.plugged_in)),
        domain::memory(&vm, mem.lines().next().unwrap_or("")),
        domain::disk(&df),
    ]))
}

async fn draw() {
    let rows = match status().await {
        Ok(rows) => rows,
        Err(e) => {
            log::write(Level::Warn, &format!("cannot read the Mac's status: {e}"));
            vec![("Status".into(), "cannot read".into())]
        }
    };
    let widget = ui::Widget {
        title: "This Mac".into(),
        rows: rows
            .into_iter()
            .map(|(label, value)| ui::WidgetRow { label, value })
            .collect(),
    };
    if let Err(e) = ui::set_widget(&widget) {
        log::write(Level::Warn, &format!("cannot set the widget: {e:?}"));
    }
}

impl Guest for Plugin {
    /// `config` is this plugin's settings as JSON.
    async fn init(config: String) -> Result<(), String> {
        let settings = domain::Settings::from_json(&config);
        draw().await;
        timer::every(settings.every_ms);
        log::write(Level::Info, "widget ready");
        Ok(())
    }

    async fn on_event(event: Event) {
        if let Event::Timer(_) = event {
            draw().await;
        }
    }

    async fn dispose() {}
}

export!(Plugin);
