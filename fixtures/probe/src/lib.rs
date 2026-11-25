//! Test fixture driven by `hotkey(id)` events:
//! - `spin` loops forever, to hit the deadline;
//! - `trap` traps;
//! - `read:<path>` reads a file through `disk` and logs the outcome.

// The generated `event` bindings take more than seven arguments.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../wit",
    world: "bricolo:stdlib/plugin",
});

use bricolo::stdlib::disk;
use bricolo::stdlib::log::{self, Level};

struct Probe;

impl Guest for Probe {
    async fn init(_config: String) -> Result<(), String> {
        log::write(Level::Info, "init");
        Ok(())
    }

    async fn on_event(event: Event) {
        let Event::Hotkey(id) = event else {
            return;
        };
        if id == "spin" {
            let mut n: u64 = 0;
            loop {
                n = std::hint::black_box(n.wrapping_add(1));
            }
        } else if id == "trap" {
            std::process::abort();
        } else if let Some(path) = id.strip_prefix("read:") {
            let outcome = match disk::read(path.to_string()).await {
                Ok(bytes) => format!("{} bytes", bytes.len()),
                Err(e) => format!("{e:?}"),
            };
            log::write(Level::Info, &format!("read {path}: {outcome}"));
        }
    }

    async fn dispose() {}
}

export!(Probe);
