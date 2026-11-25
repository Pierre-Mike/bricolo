//! The smallest plugin: logs in `init`, counts its starts in `kv`, and asks
//! for one timer tick.

// The generated `event` bindings take more than seven arguments.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../wit",
    world: "bricolo:stdlib/plugin",
});

use bricolo::stdlib::log::{self, Level};
use bricolo::stdlib::{kv, timer};

struct Hello;

impl Guest for Hello {
    async fn init(config: String) -> Result<(), String> {
        let starts = kv::get("starts")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
            + 1;
        kv::set("starts", &starts.to_string()).map_err(|e| format!("kv: {e:?}"))?;
        log::write(
            Level::Info,
            &format!("init: start {starts}, config {config}"),
        );
        let id = timer::after(tick_ms(&config));
        log::write(Level::Debug, &format!("timer {id} started"));
        Ok(())
    }

    async fn on_event(event: Event) {
        match event {
            Event::Timer(id) => {
                log::write(Level::Info, &format!("tick {id}"));
                if let Err(e) = kv::set("last-tick", &id.to_string()) {
                    log::write(Level::Error, &format!("kv: {e:?}"));
                }
            }
            other => log::write(Level::Debug, &format!("ignored {other:?}")),
        }
    }

    async fn dispose() {
        log::write(Level::Info, "dispose");
    }
}

/// Reads `"tick_ms": <n>` from the settings JSON without a JSON parser.
fn tick_ms(config: &str) -> u32 {
    config
        .split("\"tick_ms\":")
        .nth(1)
        .and_then(|rest| {
            let digits: String = rest
                .trim_start()
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse().ok()
        })
        .unwrap_or(1000)
}

export!(Hello);
