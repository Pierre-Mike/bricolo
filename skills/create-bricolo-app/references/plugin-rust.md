# Rust plugin patterns

Everything here matches `bricolo:stdlib@0.1.0`. When this page and the WIT disagree, `bricolo wit <name>` wins.

## Layout

`bricolo new <name> --template <t> --dir <parent>` writes a standalone crate:

```
<name>/
├── Cargo.toml           cdylib; wit-bindgen 0.63 and serde_json; its own [workspace]
├── rust-toolchain.toml  the pinned toolchain with the wasm32-wasip2 target
├── plugin.toml          name, version, imports, permissions, allow lists, wasm path, [settings]
├── wit/                 a copy of the bricolo:stdlib WIT (`bricolo wit all --out wit` refreshes it)
└── src/
    ├── lib.rs           the adapter: bindings, Guest impl, capability calls
    └── domain.rs        pure logic and its #[cfg(test)] tests
```

| File | May use | Must not use |
|---|---|---|
| `domain.rs` | std, serde_json | the generated bindings, capability calls |
| `lib.rs` | the bindings, `domain` | logic worth testing; move it to `domain` |

`domain.rs` gets its own types (`Area`, `Settings`, `Health`...). The adapter converts WIT records to them and back, so `cargo test` runs natively with no node and no Wasm.

## Bindings and exports

```rust
mod domain;

wit_bindgen::generate!({
    path: "wit",
    world: "bricolo:stdlib/plugin",
});

use bricolo::stdlib::log::{self, Level};
use bricolo::stdlib::{clipboard, hotkey, notify};

struct Plugin;

impl Guest for Plugin {
    /// `config` is the settings as JSON: [settings] in plugin.toml,
    /// overridden by [plugins.<name>] in node.toml.
    async fn init(config: String) -> Result<(), String> { ... }
    async fn on_event(event: Event) {
        match event {
            Event::Hotkey(id) if id == "show-clipboard" => { ... }
            Event::Timer(timer_id) => { ... }
            _ => {}
        }
    }
    async fn dispose() {}
}

export!(Plugin);
```

- Each WIT interface is `bricolo::stdlib::<name>`. Names convert to snake_case: `transcribe-file` is `speech::transcribe_file`, `set-frame` is `window::set_frame`, `%list` is `disk::list` and `window::list`.
- `async func` calls are awaited. Their string arguments are owned: `hotkey::bind("id".into(), chord.clone()).await`. Sync calls (`log::write`, `kv::get`, `kv::set`, `timer::after`) take `&str`.
- Results are `Result<T, CallError>`, with `CallError::{Unavailable, Timeout, Denied, Failed(String)}`. Log them with `{e:?}`.
- Records are plain structs: `window::Rect { x, y, width, height }`, `http::Request { method, url, headers, body, timeout_ms }`.
- `Event` has `Hotkey(String)` (the binding id), `Timer(u64)` (the id `timer::after` or `timer::every` returned) and `Ui(UiEvent)` (a tile switch or button, a choice, a review). Always keep a `_ => {}` arm.
- To call a sidecar, import `http`, list `127.0.0.1:<port>` under `[http] allow`, and keep the URL in `[settings]`. [sidecars.md](sidecars.md#calling-a-sidecar-from-a-plugin) has the request code; `bricolo new <name> --template sidecar-client` starts from it.
- Each plugin handles one event at a time. `init` has a 2 s CPU budget and each event 500 ms; time spent awaiting a capability doesn't count.

## Settings

Parse the JSON once in `init` and keep it in a `static`:

```rust
static SETTINGS: OnceLock<domain::Settings> = OnceLock::new();

async fn init(config: String) -> Result<(), String> {
    let settings = SETTINGS.get_or_init(|| domain::Settings::from_json(&config));
    ...
}
```

`domain::Settings::from_json` falls back to defaults for missing keys, and its tests cover that.

## Hotkeys

Bind in `init`, and don't fail `init` when a bind fails: log it, so the plugin stays `running` and `bricolo fire <plugin> <id>` still reaches it.

```rust
if let Err(e) = hotkey::bind(SHOW.into(), settings.chord.clone()).await {
    log::write(Level::Warn, &format!("cannot bind {}: {e:?}", settings.chord));
}
```

## plugin.toml

```toml
name = "clip-tool"          # lowercase letters, digits, dashes
version = "0.1.0"
description = "One line."
imports = ["log", "hotkey", "clipboard", "notify"]   # every interface lib.rs uses
permissions = ["disk:~/Notes", "disk:read:~/Documents"]   # only with "disk" in imports
wasm = "target/wasm32-wasip2/release/clip_tool.wasm"     # the build output; bricolo install reads it

[shell]                     # only with "shell" in imports
allow = ["/usr/bin/say"]    # exact absolute paths, no shell, no `..`
env = ["VOICE"]             # variables run() may set; not HOME or PATH

[http]                      # only with "http" in imports
allow = ["api.example.com:443", "127.0.0.1:8765"]   # lowercase host:port; redirects are not followed

[settings]                  # defaults passed to init as JSON
chord = "ctrl+alt+shift+c"
voice = "alba"

[settings_ui.voice]         # optional: how the hub's Settings tab shows it
label = "Voice"
help = "Used from the next call."
choices = ["alba", "marius"]

[settings_ui.chord]
hidden = true               # the Shortcuts tab already holds the chord
```

The hub draws a Settings tab from `[settings]`, so put options the user may want to change there, not in switches or buttons: a switch or button is for acting now. Saving the tab writes `[plugins.<name>]` in node.toml and reloads the plugin, which reads the new values in `init`.

`bricolo check .` refuses a component that imports an interface not in `imports`, a `disk` import without a `disk:` permission, `shell` or `http` without a non-empty allow list, and a section without its import. Listing an import the component doesn't use is allowed, but it is one more thing the user approves.

## Build, check, install, iterate

```bash
cargo test                                      # the domain tests, natively
cargo build --target wasm32-wasip2 --release
bricolo check .                                      # JSON plan: name, version, requested, imports, sha256
bricolo install . -y                                 # approves everything requested; a running node loads it
bricolo dev .                                        # rebuild and reinstall on each save; the node reloads
```

`bricolo dev` refuses to install a build that asks for a capability not yet approved; run `bricolo install . -y` for that.

## Sharing

`bricolo pack .` writes `<name>.rhplugin.tar.gz` (the installed `plugin.toml` and `plugin.wasm`). Attach it to a GitHub release; `bricolo install github:<owner>/<repo>[@tag]` installs it, records `github:<owner>/<repo>@<tag>` and the sha256 in `plugins.lock`, and `bricolo update` finds newer tags.
