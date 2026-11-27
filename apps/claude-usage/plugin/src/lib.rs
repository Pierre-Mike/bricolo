//! claude-usage: Claude plan usage in a widget on the hub. One gauge per
//! window, filled to the percent used: the 5-hour session in hour
//! segments, the week in day segments, each with a line at the time gone.
//!
//! It reads Claude Code's sign-in from the login Keychain with
//! `security`, then asks the same endpoint Claude Code's `/usage` asks.
//! The token stays in memory for one refresh and is never logged.
//!
//! `domain.rs` holds the logic as plain functions, tested with
//! `cargo test`. This file is the adapter.
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
use bricolo::stdlib::{http, shell, timer, ui};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

struct Plugin;

/// The access token from the first "Claude Code-credentials" item. With
/// several items of that name, `security` may return an old one: delete
/// the stale ones in Keychain Access.
async fn token() -> Result<String, String> {
    let args = [
        "find-generic-password",
        "-s",
        "Claude Code-credentials",
        "-w",
    ]
    .iter()
    .map(|a| a.to_string())
    .collect();
    let out = shell::run("/usr/bin/security".into(), args, vec![])
        .await
        .map_err(|e| format!("security: {e:?}"))?;
    if out.code != Some(0) {
        return Err("no Claude Code sign-in in the Keychain".into());
    }
    if let Some(at) = domain::expires_at(&out.stdout) {
        let left = at - now();
        log::write(
            Level::Debug,
            &format!("Keychain token expires {}", domain::until(left)),
        );
        if left <= 0 {
            return Err("the Keychain sign-in expired; run Claude Code once to refresh it".into());
        }
    }
    domain::token(&out.stdout).ok_or_else(|| "the Keychain item has no access token".into())
}

async fn fetch() -> Result<domain::Usage, String> {
    let token = token().await?;
    let req = http::Request {
        method: "GET".into(),
        url: USAGE_URL.into(),
        headers: vec![
            ("authorization".into(), format!("Bearer {token}")),
            ("anthropic-beta".into(), "oauth-2025-04-20".into()),
            ("accept".into(), "application/json".into()),
        ],
        body: vec![],
        timeout_ms: Some(15_000),
    };
    let res = http::send(req).await.map_err(|e| format!("{e:?}"))?;
    if res.status != 200 {
        let body = String::from_utf8_lossy(&res.body);
        let short: String = body.chars().take(300).collect();
        log::write(
            Level::Warn,
            &format!("usage answered {}: {short}", res.status),
        );
    }
    match res.status {
        200 => domain::usage(&String::from_utf8_lossy(&res.body)),
        401 => Err("sign-in expired; run Claude Code once to refresh it".into()),
        429 => Err("rate limited; raise every_ms".into()),
        s => Err(format!("HTTP {s}")),
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The last usage read, kept so a failed refresh (a rate limit, a
/// network blip) leaves the gauges up. Their `now` line still moves,
/// since it comes from the clock.
static LAST: std::sync::Mutex<Option<domain::Usage>> = std::sync::Mutex::new(None);

async fn draw() {
    let read = match fetch().await {
        Ok(u) => {
            *LAST.lock().unwrap() = Some(u.clone());
            Ok(u)
        }
        Err(e) => {
            log::write(Level::Warn, &format!("cannot read Claude usage: {e}"));
            LAST.lock().unwrap().clone().ok_or(e)
        }
    };
    let (gauges, rows) = match read {
        Ok(u) => {
            let gauges = domain::gauges(&u, now());
            let rows = if gauges.is_empty() {
                vec![("Usage".to_string(), "no data".to_string())]
            } else {
                vec![]
            };
            (gauges, rows)
        }
        Err(e) => (vec![], vec![("Usage".to_string(), e)]),
    };
    let widget = ui::GaugeWidget {
        title: "Claude usage".into(),
        gauges: gauges
            .into_iter()
            .map(|g| ui::Gauge {
                label: g.label,
                value: g.value,
                fill: g.fill,
                marks: g.marks,
                now: g.now,
                note: g.note,
            })
            .collect(),
        rows: rows
            .into_iter()
            .map(|(label, value)| ui::WidgetRow { label, value })
            .collect(),
    };
    if let Err(e) = ui::set_gauge_widget(&widget) {
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
