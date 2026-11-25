# Design

This document records the design agreed on 2026-11-16. It refines [architecture.md](architecture.md), which is the original idea. Where the two disagree, this document wins.

## Goal

Build a personal automation system where small reusable building blocks (speech, TTS, files, microphone, hotkeys) combine into automations, and each one runs on whichever of your devices suits it best. This replaces a setup tied to one Mac, with Lua scripts and hand-wired sidecars.

## Big picture

Every device runs the same Rust node. Automations never talk to the OS or the network directly. They call the standard library, a set of WIT interfaces such as `speech`, `tts`, `disk` and `mic`. The node's router reads your rules and sends each call to a local provider or to a peer node over iroh. Providers do the real work, either with native per-OS code or by calling the existing speech and TTS servers.

Automations come in two kinds. Reusable building blocks are Rust compiled to WASM components. Quick personal scripts are Lua and call the same standard library. A small UI hub turns plugins on and off and shows logs.

```
        Mac node                                     Desktop node (GPU)
┌─────────────────────────────────┐          ┌─────────────────────────────────┐
│  Lua scripts    WASM plugins    │          │  WASM plugins                   │
│        \            /           │          │        |                        │
│     standard library (WIT)      │          │  standard library (WIT)         │
│               |                 │          │        |                        │
│     router ── rules.toml        │          │  router                         │
│      /              \           │  iroh    │        |                        │
│  local providers   remote ──────┼──QUIC────┼─► local providers               │
│  hotkey, mic, tts               │          │     speech (Whisper/Parakeet)   │
│  sidecars :8765 :8790           │          │                                 │
└─────────────────────────────────┘          └─────────────────────────────────┘
                    UI hub: plugin cards, toggles, logs
```

Build order:

1. One Mac node working locally, with the voice inbox ported from Hammerspoon.
2. A second node reached over iroh.
3. Linux and Windows providers.

## Layers

bricolo is built like an operating system's standard library. Read from the bottom up: the hardware, the OS, a standard library written in Rust with one implementation per OS, one interface on top of it, then the apps. [design.canvas](design.canvas) draws it with every part named; open it in Obsidian.

```
5. Apps                 Lua scripts, WASM plugins            written once, run on every OS
4. One interface        wit/ (bricolo:stdlib), the `bricolo` global    the calls; no code, no OS names
   In between           runtime, Lua tier, router, mesh      checks permissions, picks this device or another
3. Standard library     crates/providers/                    in Rust: one trait per capability,
   in Rust                macos/  linux/  windows/             and one implementation per OS
                          portable code, sidecars
2. Operating system     macOS, Linux, Windows                not ours
1. Hardware             camera, mic, keyboard, screen, GPU   not ours
```

Rules:

- An app sees only the interface. A WASM plugin can't reach the OS at all. Lua scripts are trusted, but they also reach the OS only through `bricolo`.
- The interface never names an operating system. A WIT comment says what a call does and how a node that can't do it answers. Which systems support what goes in [apps.md](apps.md), [scripts.md](scripts.md) and [linux.md](linux.md).
- The standard library is the only code that knows the OS. Each capability is a Rust trait in `crates/providers/src/<capability>.rs`, implemented under `crates/providers/src/<os>/`, behind `#[cfg(target_os)]`.
- An OS without an implementation answers `unavailable`. Each desktop capability has a stub that does this (`NoClipboard`, `NoHotkeys`), so every node offers the same interface and only the answers change. Binding `"fn"` on Linux answers `unavailable`, and a node that doesn't see key releases never sends `hotkey-released`.
- The router sits between the interface and the standard library, so a call for `tts`, `speech`, `disk`, `clipboard` or `notify` can run on another device. The app can't tell the difference, except that it may get `timeout`.
- A new capability, such as a camera, is one WIT file, one Rust trait with its stub, and one implementation per OS. The canvas lists the steps.
- A device that can't run a node, such as a microcontroller or a phone, has no place yet. It could join as a peer that offers a few capabilities, or sit behind a sidecar.

## Repository layout

```
bricolo/
├── Cargo.toml            workspace root
├── wit/                  standard library interfaces (one source of truth)
│   ├── log.wit  hotkey.wit  tts.wit  speech.wit  disk.wit  mic.wit
│   └── world.wit         "plugin" world: what a plugin may import and export
├── crates/
│   ├── node/             the binary: starts everything, CLI flags
│   ├── runtime/          Wasmtime embedding, loads and runs WASM plugins
│   ├── lua/              Lua script tier (mlua), same standard library
│   ├── router/           reads rules.toml, picks local or remote provider
│   ├── mesh/             transports (iroh, Tailscale), peer list, remote calls
│   ├── providers/        trait per capability + local implementations
│   │   └── macos/ linux/ windows/
│   └── ui/               Tauri 2 hub app
├── plugins/
│   ├── hello/            smallest possible plugin, used in tests
│   └── voice-inbox/      first real port from Hammerspoon
└── docs/
```

- The project is one Cargo workspace on Rust edition 2024, with one shared `Cargo.lock`.
- `wit/` holds one WIT package, `bricolo:stdlib@0.1.0`. The host and the plugins both generate their bindings from it.
- Only `node` produces a binary. Each other crate does the one job listed above.
- Each capability is a Rust trait in `providers`. A local backend and the mesh client both implement it, so the router doesn't care where a call runs.
- OS-specific code lives only under `providers/<os>/`, gated with `#[cfg(target_os)]`.
- Each plugin in `plugins/` is its own crate. It builds to a `.wasm` component with `wit-bindgen`, outside the host build.
- `plugins/hello` is the fixture for the runtime's integration tests.

## Files on disk

```
~/.config/bricolo/          what you edit, safe to keep in dotfiles
├── node.toml          this node's name, settings, sidecars, plugin overrides
├── rules.toml         routing rules ("speech → desktop, else local")
├── peers.toml         trusted peers: name → iroh public key + allowed capabilities
├── permissions.toml   capabilities granted to each plugin
├── plugins.lock       source, version and sha256 of each installed plugin
├── plugins/
│   └── voice-inbox/
│       ├── plugin.toml      manifest: name, version, imports, permissions, settings
│       └── plugin.wasm
└── scripts/
    └── *.lua           quick personal automations

~/.local/share/bricolo/     what the node manages, not in dotfiles
├── node.key           iroh secret key, file mode 0600
├── data/<plugin>/     each plugin's private folder (its only file access)
└── logs/              one log file per day
```

- Every OS uses the same layout. On Windows the two roots map to `%APPDATA%` and `%LOCALAPPDATA%`.
- All config files are TOML.
- The config folder can go in a dotfiles repo. The secret key lives in the other folder, so it never does.
- Installing a plugin means placing its folder, with `plugin.toml` and `plugin.wasm`, under `plugins/`.
- WASI gives each plugin access to its own `data/<plugin>/` folder and nothing else on disk. Anything wider goes through the `disk` capability.
- The node creates `node.key` on first run. That key is the node's identity on the mesh.
- A peer not listed in `peers.toml` is refused at connection time.

## Plugin lifecycle

```
 scan plugins/ ──► read plugin.toml ──► check ──► order ──► start ──► running ──► stop
                                          │          │         │          │          │
                     imports match manifest?    sort by    init()   on-event()  dispose()
                     permissions granted?       imports             one at a time
                     hash matches lock?                                │
                     if not: skip and log                       trap or timeout
                                                                       │
                                                         restart once, then stay off
```

- The node loads plugins only from `plugins/` and scripts only from `scripts/`.
- The node checks the imports inside `plugin.wasm` against `plugin.toml`, `permissions.toml` and the hash in `plugins.lock`. On any mismatch it skips that plugin, logs why, and loads the rest.
- A plugin can import interfaces exported by another plugin. Plugins start in dependency order, and the node refuses plugins that form an import cycle.
- Each plugin has its own Wasmtime store and memory.
- A plugin exports `init(config)`, `on-event(event)` and `dispose()`, defined in `world.wit`.
- Each plugin handles one event at a time from its own queue.
- Wasmtime epoch interruption stops a call that runs past its time limit. Other plugins keep running.
- After a trap or timeout, the node restarts the plugin once. If it fails again, it stays off until you turn it back on in the UI, and the error shows on its tile.
- Each Lua script gets its own Lua state and the same start, run and stop steps.
- A plugin's settings have defaults in `plugin.toml`. You override them in `node.toml` under `[plugins.<name>]`, and the plugin receives them in `init`.

## Tooling

```
 edit ──► lefthook (pre-commit) ──► push ──► GitHub Actions
              │                                  │
        cargo fmt --check                  macOS runner first
        cargo clippy -D warnings           just ci  (same steps as local)
                                           + build plugins/*.wasm
                                           + cargo nextest
                                           + cargo deny
```

- `rust-toolchain.toml` pins stable Rust and adds the WASI component target.
- `just` is the task runner: `just build`, `just plugins`, `just test`, `just ci`, `just dev <plugin>`, `just install <plugin>`. CI runs `just ci`.
- Lefthook runs `cargo fmt --check` and `cargo clippy -- -D warnings` on every commit.
- Tests run with `cargo nextest`. The runtime's integration tests load `plugins/hello` through the real Wasmtime runtime.
- `cargo deny` checks licenses and advisories. It fails the build on any GPL dependency, which keeps Zed's GPL code out.
- Async runs on Tokio. Wasmtime, iroh and WASI 0.3 async support all use it.
- Logging uses `tracing`, written to the daily log files and shown in the UI.
- Library crates use `thiserror`. The `node` binary uses `anyhow`.
- CI runs `just ci` on macOS and Ubuntu, so the Linux build stays green before Linux providers exist.
- `AGENTS.md` at the root lists the `just` commands and the crate map.

## Decisions

### 1. Remote failures are part of every interface

Every capability function is async (WASI 0.3) and returns `result<T, call-error>`:

```wit
variant call-error {
    unavailable,      // no node offering this capability is online
    timeout,          // deadline passed
    denied,           // permission or peer trust refused
    failed(string),   // the provider ran and reported an error
}
```

- Plugins never name a device. The router picks one from `rules.toml` and tries the fallbacks before it returns an error.
- Each call can take an optional deadline. If the plugin drops the future, the call is cancelled, and the router passes the cancellation on to the remote node.

```
plugin ──► speech.transcribe(audio, deadline) ──► router
                                                   │
                         rules: desktop, then laptop, then local
                                                   │
                     desktop asleep ─► laptop ok ─► Ok(text)
                     all fail ──────────────────► Err(unavailable)
```

Rejected:

- Fully transparent calls, because a sleeping desktop would trap the plugin.
- A device argument on every call, because it moves routing into every plugin.
- Separate local and remote interfaces, because they double the WIT files.

### 2. iroh for transport, our own small RPC

- Each node's iroh public key is its identity. Only peers listed in `peers.toml` get through the handshake.
- Each call opens its own QUIC stream. It sends a header (capability, method, deadline, call id) and `postcard` arguments, and gets back a `Result`. Resetting the stream cancels the call.
- A microphone feed is one long-lived stream of framed audio chunks.
- Both ends run our `node` binary, so the RPC only has to serialize the provider trait methods. The ALPN `bricolo/1` stops mismatched versions from talking.
- Everything stays behind the provider trait in `mesh`, so switching to wRPC later touches one crate.

```
 Mac node                         iroh connection (QUIC, keyed by node id)                  Desktop node
┌──────────┐   stream 1: tts.speak       header + postcard args ──►  Result          ┌──────────┐
│  mesh    │   stream 2: speech.transcribe  ...                                       │  mesh    │
│  client  │═══stream 3: mic feed ════ audio frames ══════════════════════════════►   │  server  │
└──────────┘   (reset a stream = cancel that call only)                               └──────────┘
```

Rejected:

- wRPC, because its wire spec is still `v0.0.1-draft.1` and its releases often break.
- Tailscale or Headscale with quinn as the only transport, because they need a separate daemon and a phone app. iroh does this inside the node. Tailscale is now an optional second transport for devices that already run it (see "Transports" in the implementation notes).
- gRPC or HTTP over Tailscale, because they add protobuf next to WIT and still need Tailscale.
- Cap'n Proto, because it's a second interface language next to WIT.

### 3. Lua 5.4 for quick automations

- `mlua` embeds Lua 5.4, the same version Hammerspoon uses.
- Each script runs in a sandbox without `io`, `os.execute`, C module `require` or `debug`. Scripts reach the machine through the `bricolo` global (`bricolo.speech.transcribe`, `bricolo.tts.speak`, `bricolo.hotkey.bind`).
- Async calls map to Lua coroutines: `local text, err = bricolo.speech.transcribe(audio)`.
- Errors return `nil, err`, where `err` is the `call-error` name.
- Scripts get every capability without permission prompts, because you wrote them. Rules and peer trust still apply.
- Saving a file in `scripts/` reloads that script only, in under a second.
- Pure Lua helpers and their `busted` tests carry over from Hammerspoon. Code that calls `hs.*` gets rewritten to call `bricolo.*`.

Rejected:

- Luau, because it's a Lua 5.1 dialect, and existing code uses 5.4 features.
- Rhai, because none of the existing code carries over.
- JavaScript through QuickJS, for the same reason, plus more glue code.
- WASM only, because every small automation would need a Rust build.

### 4. Keep the model sidecars

- The `sidecar` module in `providers` starts each server, frees its port, polls `/health`, restarts it if it dies and stops it with the node.
- Sidecars are listed in `node.toml` under `[sidecars.<name>]` with a command, port and health URL.
- The `speech` provider calls `parakeet_server.py` on :8765. The `tts` provider calls pocket-tts on :8791 and :8793.
- The `:8790` speech queue (FIFO, sentence chunking, stop, status) moves into the node's `tts` provider.
- On Linux or Windows, a different server can sit behind the same HTTP endpoints.

```
plugin or script ──► router ──► speech provider ──HTTP──► parakeet_server.py :8765 (MLX)
                              ► tts provider ──── queue ──HTTP──► pocket-tts :8791 / :8793
                                                         │
                              node.sidecar: start, port sweep, /health, restart, stop
```

Rejected:

- Models inside the node (`candle`, `ort`, `whisper-rs`), because it means giving up parakeet-mlx and rewriting working code. Worth revisiting per platform.
- Models inside WASM plugins, because WASM can't reach the GPU.
- Sidecars managed by launchd or systemd, because the node couldn't restart them or show their state.

### 5. Permissions are granted at install

- At install, the node shows the capabilities in `plugin.toml`. You approve them all or cancel. The result goes to `permissions.toml`.
- If an update asks for a new capability, the plugin stays off until you approve it.
- `disk` grants name a path (`disk:~/Notes`, `disk:read:~/Documents`).
- While a plugin uses the microphone, camera or screen, the hub and menu bar show a dot with its name.
- Each peer in `peers.toml` lists what it may call: `phone = { key = "…", allow = ["tts", "speech"] }`.
- The node ships as a signed `.app` bundle started by a LaunchAgent, so macOS keeps its microphone, Accessibility and Input Monitoring grants. Development builds use one stable local signing identity.

```
install voice-inbox ──► "wants: mic, speech, disk:~/Notes" ──► approve ──► permissions.toml
                                                                              │
runtime call ──► granted? ── no ──► Err(denied)                               │
                    │ yes                                                     │
                 router ──► remote? ──► peer's allow list on the other node ──┘
```

Rejected:

- Asking per capability on first use, because prompts would interrupt hotkey actions and block calls from other devices.
- Trusting every plugin, because it throws away the sandbox.
- Per-call capability tokens, because the iroh key already proves which peer is calling.
- Running the node as a bare binary, because macOS would record the permissions against the terminal app.

### 6. Reload one plugin at a time, without keeping state

- When `plugin.wasm` changes, the node calls `dispose()`, gives in-flight calls 2 seconds, cancels the rest, loads and checks the new component, and calls `init()`.
- Plugins keep anything that should survive a reload in their `data/` folder.
- Plugins that import the reloaded plugin's interfaces restart too, in dependency order.
- If the new version fails its checks or traps in `init()`, the old version keeps running.
- `just dev <plugin>` rebuilds on save, which triggers the reload.
- Sidecars and mesh connections stay up during a plugin reload.

```
save src ──► just dev voice-inbox ──► plugin.wasm changes
                                           │
               node: dispose old ─► drain calls (2 s) ─► load new ─► check ─► init
                                           │                            │
                                 dependents restart              fails? keep old version
```

Rejected:

- Restarting the whole node, because it reloads the warm models and drops mesh connections.
- Hot reload that keeps state, because Wasmtime can't move memory between builds.
- No reload at all, because building plugins would be too painful.

### 7. Install prebuilt plugins, pinned by hash

- `bricolo install ./plugins/voice-inbox` copies a built plugin folder. `just install <plugin>` builds and installs plugins from this repo.
- `bricolo install github:owner/repo@v1.2.0` downloads `plugin.toml` and `plugin.wasm` from that release. Devices don't need Rust.
- `plugins.lock` records each plugin's source, version and sha256. The node refuses a `.wasm` whose hash doesn't match.
- `bricolo update` checks for newer tags and asks you to approve any new capabilities.
- `bricolo install --from-lock` installs the same plugins at the same hashes on a new device.

```
this repo ──just install──┐
                          ├──► plugins/<name>/{plugin.toml, plugin.wasm}
GitHub release ──bricolo install┘              │
                                  plugins.lock: source, version, sha256
                                          │
                       node start: hash matches? ── no ──► refuse, log
```

Rejected:

- Building from source on each device, because every device would need Rust.
- An OCI registry with `wkg`, because it means hosting a registry for plugins only you use. `plugins.lock` can record an OCI source later.
- Our own registry, because it only pays off once other people publish plugins.

### 8. Tauri 2 UI in its own process

- The `.app` bundle holds the node and the UI. The node runs as the LaunchAgent and holds the macOS permissions. The UI talks to it over a local socket. Headless nodes run only the node.
- Plugins and scripts describe tiles as data through a `ui` capability: title, status text, switches, buttons. Plugins never ship HTML or JavaScript.
- The `ui` capability offers a few kinds of floating panel: caption, choice list, toast and review panel. On macOS they're non-activating `NSPanel`s through `tauri-nspanel`, so they never take focus.
- The menu bar item uses Tauri's tray, with the recording dot and a quick on/off control per plugin.
- The hub shows plugin cards, `rules.toml`, peers and their status, and the logs.

```
┌──────────── bricolo.app ─────────────┐
│  node (LaunchAgent, holds TCC permissions)    │
│     ▲  local socket: tiles, panels, logs      │
│     ▼                                         │
│  ui (Tauri 2): hub window, menu bar,          │
│                NSPanel captions / review      │
└───────────────────────────────────────────────┘
headless nodes (desktop, server): node only
```

Rejected:

- Slint, because it has no tray or non-activating panels built in.
- egui, because focus-free floating panels are hard and it looks like a tool.
- Iced, because it has no tray, and panels would need platform code.
- GPUI, because its docs are mostly Zed's own code and its API follows Zed's needs.
- One process for the node and UI, because a UI crash would stop every automation.

## Implementation notes

Phase 1, the foundation, built on 2026-11-16. These notes record what the code does where the design left a choice open or the code differs from it.

### Async capability calls (Decision 1)

Capability calls use WASI 0.3 async: the WIT functions are `async func`, and the plugin awaits them. We chose this over the spike's pattern (return an id, deliver the result as a later event) after a probe. A guest built with `wit-bindgen` 0.63 for `wasm32-wasip2` and a host on Wasmtime 50 with `component-model-async` ran awaited host calls on the first try, about 15 minutes of work. Epoch interruption still trapped a spinning guest.

One catch showed up in the probe. The epoch deadline counts wall time, so time a plugin spends waiting on an awaited host call counted against its budget. The host now restarts the plugin's budget after each awaited call returns (`resume` in `crates/runtime/src/host.rs`). The budget therefore limits time spent running guest code.

Which calls are async:

- Async: `hotkey.bind` and `unbind`, `tts`, `speech`, `disk`, `clipboard`, `timer.sleep`. These go through the router or wait on the OS.
- Sync: `log.write`, `timer.after`, `every` and `cancel`, and `kv`. They are local and quick. `timer.after` and `every` return an id at once, and a `timer(id)` event follows.
- The exports `init`, `on-event` and `dispose` are async too, so a plugin can await capabilities inside them.

Calls take no deadline argument yet. The router gives each target `timeout_secs` (default 30) before it tries the next one. We have not tested what happens to a host call when a plugin drops its future. Cancellation, and passing it to a remote node, come with the mesh.

### Budgets

`node.toml` sets `[runtime] event_budget_ms` (default 500) and `init_budget_ms` (default 2000). The spike used 100 ms. Under heavy parallel test load, a trivial event overran 100 ms of wall time and triggered a restart, so the default is now 500 ms.

### Files

- `plugin.toml` has `name`, `version`, `description`, `imports`, `permissions`, `wasm`, `[settings]` and `[settings_ui.<key>]` (label, help, choices, advanced, hidden: how the hub's Settings tab shows a setting; each must name a key of `[settings]`). `imports` lists standard library interfaces (`"log"`) or another plugin's interface (`"voice/inbox"`). `permissions` lists extra grants such as `"disk:~/Notes"` and `"disk:read:~/Documents"`. `wasm` is the build output path, read only by `bricolo install`.
- Every standard library import is a capability you approve at install, `log` included. A plugin that imports `disk` must ask for at least one `disk:` path.
- `permissions.toml` has one table per plugin: `[hello] granted = ["log", "kv", "timer"]`.
- `plugins.lock` has `[plugins.<name>]` with `source`, `version` and `sha256`. Phase 5a adds `[scripts.<name>]`; see below.
- `rules.toml` has an optional `timeout_secs` and a `[routes]` table, such as `speech = ["desktop", "local"]`. Unlisted capabilities run locally. Only `tts`, `speech`, `disk` and `clipboard` can be routed. In this phase every peer counts as offline, so a peer target answers `unavailable` and the router moves on. Phase 2 connects peers; see below.
- `kv` stores each plugin's keys in `data/<plugin>/kv.json`.
- WASI mounts the plugin's `data/<plugin>/` folder at `/data` inside the plugin.
- The control socket is `<data>/bricolo.sock`, a Unix socket that speaks JSON lines. It answers `{"cmd":"status"}` and `{"cmd":"reload","plugin":"<name>"}`. A reload request also turns an `off` plugin back on. The socket is Unix only for now.

### Lifecycle

- A plugin's state is `running`, `refused` (failed its checks), `off` (failed twice, or failed in `init`) or `stopped` (shut down on purpose). `bricolo status` shows the state, the last error and the restart count.
- On reload the node checks the new files first. If they fail, the old version keeps running and the status shows `reload refused`. If they pass, the node disposes the old instance (2 seconds for the current event), then runs the new `init`. If that `init` fails, the node starts the old version again, which runs the old `init` a second time.
- `bricolo install` writes `plugins.lock` and `permissions.toml` before the plugin files. A running node that sees the new `plugin.wasm` then finds the matching hash.

### Not done in phase 1

- Imports between plugins. The manifest syntax and the start order (with cycle refusal) exist and have tests, but the runtime refuses a component that imports another plugin's interface. Restarting dependents on reload waits for this.
- `bricolo update`, `bricolo install --from-lock` and `github:` sources (Decision 7). Phase 5a adds them; see below.
- No plugin can call `hotkey`, `tts`, `speech` or `clipboard` successfully yet. Their providers answer `unavailable`. Phase 2 adds them.
- Wasmtime runs without its `cache` and `profiling` features. Both pull in C libraries that make the Linux cross-check need a full C toolchain. Without the cache, the node compiles each plugin on every start.

### Additions

- `bricolo dev <dir>` rebuilds a plugin and reinstalls it on each save. `just dev` uses it. It refuses to install when the plugin asks for a capability you have not approved.
- `bricolo new <name>` writes a standalone crate with its own `[workspace]`, toolchain file and a copy of `wit/`. Inside this repository it uses the shared `wit/` and `target/` instead.

### Phase 2: Lua scripts (Decision 3)

Built on 2026-11-16 in `crates/lua`, with mlua 0.12 and a vendored Lua 5.4. [scripts.md](scripts.md) is the script-facing reference. These notes record the choices behind it.

Files and names:

- Only `.lua` files directly in `scripts/` run. Files in subfolders are modules, loaded with `require("lib.util")`, which reads `scripts/lib/util.lua` or `scripts/lib/util/init.lua`. A script name uses ASCII letters, digits, `-` and `_`.
- Providers know a script as `<name>.lua`. Its `kv` file is `data/<name>.lua/kv.json` and its log lines carry `plugin=<name>.lua`. A plugin's name can't contain a dot, so the two never clash.

Providers and routing:

- Scripts use the runtime's providers, already wrapped by the router, so `rules.toml` and peers apply. The runtime's `Options` gained `other_events`, a sink for provider events whose caller isn't a plugin. The node points it at the Lua tier's `Mailbox`.
- Scripts are trusted. They skip `permissions.toml`, and their `disk` grant is `/` (each drive root on Windows). Their `shell` and `http` grants are `*`; see "Phase 2: the three parts together" below.
- `bricolo.speech.transcribe(path)` reads the audio file on this node, then calls `speech`, which may route to a peer. `bricolo.speech.transcribe_audio(bytes, format)` matches the WIT call.
- `bricolo.hotkey.bind` also takes Hammerspoon's `(mods, key, fn)` form and joins it into a chord.

Coroutines:

- With mlua's `async` and `send` features, the script body, `init`, `dispose` and each callback run through `call_async`, each in its own Lua thread. A waiting `bricolo` call yields that thread until its provider future completes.
- One task per script polls all of its coroutines. They take turns on one Lua state, but a callback waiting on a provider no longer holds up the next event. Plugins handle one event at a time, so this is a difference between the tiers.
- Lua code that yields inside `coroutine.create` or `coroutine.wrap` would hand mlua's "pending" marker to the script. The sandbox replaces `coroutine.resume` and `coroutine.wrap` with versions that pass the marker up to the node, so waiting calls work there too. This uses mlua's `Lua::poll_pending()`, which is public but hidden from the docs. Check it when upgrading mlua.
- A module's top-level code runs inside `require`, a Rust function, so it can't make waiting calls.

Budgets and limits:

- A count hook checks the CPU budget every 1000 instructions. Each poll of a coroutine starts the budget again, so waiting time doesn't count, as with the plugins' `resume`. The budgets are the `[runtime]` values from `node.toml`: `init_budget_ms` for the body and `init` together, `event_budget_ms` for each callback and `dispose`.
- Lua can catch any error with `pcall`, so after the first budget error the hook alternates between yielding, which passes through `pcall`, and raising again. The poll wrapper fails any call that overran. This matches the epoch trap, which plugins can't catch either.
- Each Lua state may allocate 256 MiB.

Lifecycle:

- States are `running`, `off` and `stopped`. A script that doesn't compile is `off`, since there is no separate check step to refuse it. `bricolo status` adds a `scripts` array, and the status error is the first line of the Lua error; the log has the traceback.
- On stop or reload, the node drops a script's callbacks that are still waiting instead of giving them the 2 seconds plugins get. A callback in a long `bricolo.timer.sleep` would otherwise delay every reload past the one-second target. `dispose()` still gets 2 seconds.
- Reload follows Decision 6. The node compiles the new file first; if that fails, the old version keeps running and the status shows `reload refused`. If the new version fails while starting, the old source starts again and the status shows `reload failed`. A changed module reloads every script that loaded it.
- The watcher waits 100 ms for writes to settle (plugins wait 300 ms). It skips a script, or a module, whose content matches what the running script loaded. FSEvents sometimes reported a file written just before the watcher started, which restarted a script nobody had touched. Measured reloads take 120 to 200 ms from save to the new version running.
- The control socket accepts `{"cmd":"reload-script","script":"<name>"}`, which also turns an `off` script back on. `bricolo new-script <name>` writes `templates/scripts/starter.lua` to `scripts/<name>.lua`.

Tooling:

- `just check-linux` now also sets `AR_x86_64_unknown_linux_gnu` to `scripts/linux-ar` (`zig ar`). Apple's `ar` and `ranlib` dropped the ELF objects zig built for the vendored Lua and left an empty archive. `cargo check` never links, so it passed either way.

Not done in phase 2:

- The Lua tests use fake providers for the desktop capabilities, `tts` and `speech`, so they need no GUI session. `shell` and `http` run the real providers against `/bin/echo`, `/bin/sh` and a local test server.
- `busted` isn't wired into `just ci`. Pure helpers in `scripts/lib/` can be tested with it outside the node.

### Phase 2: the mesh (Decision 2)

Built on 2026-11-16 in `crates/mesh`. [mesh.md](mesh.md) covers pairing, the file formats and the security model. These notes record the choices the design left open and where the code differs from it. They replace the phase 1 notes that say every peer counts as offline and that cancellation waits for the mesh.

Transport:

- The mesh uses iroh 1.4.1, the current 1.x release, rather than 1.0. The workspace turns off iroh's `portmapper` feature: it pulls in `attohttpc`, which is MPL-2.0. Hole punching and relays work without it; only UPnP and NAT-PMP port mapping is gone.
- `deny.toml` now allows `CDLA-Permissive-2.0` (Mozilla's CA list in `webpki-roots`, which iroh uses to reach its relays). It ignores RUSTSEC-2024-0436, an "unmaintained" notice for `paste`, which iroh's `netwatch` pulls in. The notice reports no vulnerability.
- `node.toml` has a `[mesh]` table: `enabled`, `relays` and `port`. `relays = true` (the default) uses iroh's `N0` preset, with n0's relays and DNS discovery. `relays = false` uses no relays and no discovery, so peers need `addrs` in `peers.toml`. Tests use loopback with relays off.
- `node.key` holds the secret key as 64 hex characters, created with mode 0600. The node warns if the file is readable by other users.
- The node refuses an unknown key in an iroh `after_handshake` hook, the first point where iroh knows the remote key. It closes the connection with code 1, and the other side shows `refused`.
- Each node dials every peer itself and keeps that connection open, so two paired nodes hold two connections. A node sends its calls on its own connection and serves calls on the other. One supervisor task per peer reconnects with a backoff from 0.5 to 30 seconds. An incoming connection from the peer cuts the wait short, so a restarted node is back within a second.
- A peer is online while this node's own connection to it is up. The router asks the mesh on every call, and an offline peer answers `unavailable` without waiting for the timeout.
- iroh sends keep-alives every 5 seconds. The idle timeout is 15 seconds, so a peer that vanishes without closing goes offline within 15 seconds. A node that stops cleanly closes its connections, and its peers see it at once.

Calls:

- The header carries `id`, `kind` (call or stream), `capability`, `method`, `deadline_ms` and the calling plugin's name. The arguments travel as a `postcard` tuple in a second frame, and the answer is `Result<bytes, call-error>`.
- Plugins cannot set a deadline yet. Each remote call gets `timeout_secs` from `rules.toml` as its deadline, sent as relative milliseconds. The serving node answers `timeout` when it passes.
- Cancellation works by dropping the stream. When the client drops its receiving side, QUIC tells the server to stop sending. The server waits on that signal next to the provider call and drops the provider's future when it fires. The router's own timeout and a plugin dropping its future both end up here.
- A broken connection answers `unavailable`, so the router tries the next target. A call that broke after it started on the peer may therefore run twice.
- On the serving node, a peer's call runs as caller `<peer>/<plugin>`. Its disk grants come from the `disk:` entries in that peer's `allow` list, with `~` expanded to the serving node's home folder. The calling node first checks the path against the calling plugin's own grants.
- Peers' calls go to the serving node's local providers, never through its router. `Runtime::start_with_peers` takes the mesh as the router's `Peers` and `Runtime::local_providers` returns the unrouted providers for the mesh to serve.
- `capabilities.rs` is a macro table. One entry per capability generates the client type and its branch of `dispatch`. `dispatch` is generic over the codec, so `bricolo call` reuses it with JSON.
- `allow` entries are checked for form, not against the capability list, so a capability added later, or the test-only `stream-test`, needs no change in `bricolo-mesh`'s validation.

Additions:

- `bricolo id` prints the endpoint id, an `bricolo peer add` line and a `peers.toml` block. `bricolo peer add` appends to `peers.toml` and keeps comments. `bricolo peer list` shows the peers, with their state when a node runs. `bricolo peer reload` makes a running node reread the file; `bricolo peer add` does that by itself.
- `bricolo call <capability> <method> [json-args] [--grant disk:…]` runs one call through the running node's router as the plugin `bricolo-call`, for debugging. It covers the capabilities in the remote table.
- The control socket answers `{"cmd":"reload-peers"}` and `{"cmd":"call",…}`, and `status` has a `mesh` key.
- Media streams (`Mesh::open_stream`, `StreamHandler`) exist for the microphone. No capability uses them yet.

Not done in phase 2:

- `peers.toml` is not watched. Edits by hand need `bricolo peer reload`.
- `bricolo status` prints whether each peer is connected, not whether the path is direct or relayed.
- iroh's local network discovery (mDNS) is a separate crate and is not used. With relays off, peers need `addrs`.
### Phase 2: macOS providers and sidecars

Built on 2026-11-16.

#### The main thread

On macOS, hotkeys and the screen list only work on the thread that runs the AppKit event loop, and that must be the process's first thread. `bricolo run` therefore calls `bricolo_providers::desktop::run`. On macOS it builds a tao event loop on the main thread with the `Accessory` activation policy (no Dock icon, no menu bar), starts the node and its Tokio runtime on a thread named `bricolo-node`, and runs the loop with `run_return`. When the node returns, it posts a quit event, the loop returns, and `main` exits normally. On Linux `desktop::run` runs the node on the calling thread, so `bricolo run` stays headless there.

Providers reach the main thread through `MainHandle`, which sends a message to the loop and awaits the reply on a Tokio oneshot channel. Only hotkey registration and `window.screens` use it. The Accessibility calls, NSPasteboard and NSWorkspace run on Tokio's blocking pool; `open` and `osascript` run as child processes. Tests have no main-thread loop, so in tests `hotkey` and `window.screens` answer `unavailable`.

#### New interfaces

The package stays `bricolo:stdlib@0.1.0`. New interfaces: `window`, `app`, `notify`, `shell` and `http`. `tts` gained `status`, and `speech` gained `transcribe-file`. Every new function is async and returns `result<T, call-error>`.

- `window`: `list`, `focused`, `focus(id)`, `set-frame(id, rect)` and `screens`. Points are measured from the top-left corner of the main display. Window ids come from the private `_AXUIElementGetWindow`, as in Hammerspoon and yabai. Without the Accessibility permission, calls answer `denied`.
- `app`: `launch` takes a bundle id or an app name and runs `open -b` or `open -a`; it refuses paths. `open-url` refuses `file:` URLs, because `open` would run a `.command` file. `running` and `frontmost` read NSWorkspace and list only apps that show in the Dock.
- `notify`: `osascript` with `display notification`. The title and body go in as script arguments, so they cannot inject AppleScript.
- `shell` and `http` are portable and need allow lists in `plugin.toml` (below).
- `hotkey`, `clipboard`, `window`, `app` and `notify` have macOS backends under `crates/providers/src/macos/`. On other systems they answer `unavailable`.
- `notify` is routable, next to `tts`, `speech`, `disk` and `clipboard`. `Peers::notify` has a default body that returns `None`, so a `Peers` implementation written before phase 2 still compiles. The desktop capabilities, `shell` and `http` always run on this node.
- `bricolo status` lists each plugin's bound hotkeys (`"hotkeys": {"left-half": "cmd+alt+left"}`) and the sidecars.

Hotkey chords are parsed in `providers/src/hotkey.rs`: modifiers `cmd`, `ctrl`, `alt` and `shift` (with aliases such as `option`), and keys `a` to `z`, `0` to `9`, `f1` to `f20`, the arrows, `space`, `enter`, `tab`, `escape`, `delete`, `home`, `end`, `pageup`, `pagedown` and punctuation. Two plugins cannot bind the same chord; the second gets `failed`. Rebinding an id to a new chord drops the old one.

#### Allow lists in plugin.toml

```toml
imports = ["log", "shell", "http"]

[shell]
allow = ["/usr/bin/say"]    # exact absolute paths, no `..`
env = ["VOICE"]             # variables the plugin may set; not HOME or PATH

[http]
allow = ["api.example.com:443", "127.0.0.1:8765"]   # lowercase host:port
```

- A plugin that imports `shell` or `http` must have the matching section with a non-empty `allow`, and a section needs the import.
- Each entry is a grant you approve at install. `bricolo install` shows them, and `permissions.toml` records them, as `shell:/usr/bin/say`, `shell:env:VOICE` and `http:api.example.com:443`. An update that adds an entry stays off until you approve it.
- `shell.run` starts the program directly, with no shell, in `/`. The child gets `HOME`, a fixed `PATH` and the listed variables the plugin passes, nothing else. A program that runs past 60 seconds is killed and the call answers `timeout`. Each output stream keeps its first MiB.
- `http.send` checks the URL's `host:port` against the list, with 80 or 443 filled in. `localhost` and `127.0.0.1` are different entries. Redirects are not followed, so a server cannot send the plugin to a host off its list; the plugin gets the 3xx. The body limit is 8 MiB, the default timeout 30 s and the maximum 300 s. HTTPS uses rustls with ring and the OS certificate store (`rustls-platform-verifier`), so there is no OpenSSL and `just check-linux` still needs only zig.

#### Sidecars

`node.toml` lists them:

```toml
[sidecars.parakeet]
command = "~/.local/share/uv/tools/parakeet-mlx/bin/python"
args = ["~/.hammerspoon/parakeet_server.py"]
cwd = "~"                                      # optional
port = 8765
health_url = "http://127.0.0.1:8765/health"   # optional, this is the default
startup_secs = 180                             # optional, default 120
env = { STT_ENGINE = "parakeet", STT_MODEL = "…", STT_PORT = "8765" }

[sidecars.pocket-en]
command = "~/.hammerspoon/.venv-tts/bin/python"
args = ["~/.hammerspoon/pocket_tts_server.py"]
port = 8791
env = { POCKET_TTS_PORT = "8791", POCKET_TTS_VOICE = "alba" }

[speech]
sidecar = "parakeet"        # or url = "http://127.0.0.1:8765"

[tts]
sidecar = "pocket-en"       # or url = "…"; neither means `say` on macOS
voice = "alba"
player = ["/usr/bin/afplay"]   # the WAV path is appended; default afplay, aplay on Linux
max_chunk = 220
[tts.profiles]
alerts = "marius"

[claude-code]
dirs = ["~/Github"]               # folders sessions may work in; none means unavailable
command = "claude"                # optional, default "claude" on PATH
permission_mode = "acceptEdits"   # optional, this is the default
extra_args = ["--model", "opus"]  # optional, added to every run
```

`~/` expands in `command`, `args` and `cwd`. The child inherits the node's environment plus `env`. `[tts]` and `[speech]` name a sidecar or a URL, not both, and a named sidecar must exist; `bricolo run` refuses a `node.toml` that breaks this.

One Tokio task supervises each sidecar:

1. Free the port: `lsof` finds the processes listening on it, and the node kills them with SIGKILL, never itself. Without `lsof` this step does nothing.
2. Start the command in its own process group. Its stdout and stderr go to `<data>/logs/sidecar-<name>.log`.
3. Poll the health URL every 250 ms until it is healthy, then every 5 s. Healthy means a 200 whose body, if it is a JSON object with a `ready` key, has `ready: true`. parakeet answers 200 with `"ready": false` while it loads; pocket-tts answers 503.
4. If the process exits, wait and start it again. The wait starts at 1 s and doubles up to 60 s. A sidecar that ran for 60 s before dying restarts after 1 s.
5. On shutdown, after the plugins stop, send SIGTERM to the group, then SIGKILL after 3 s.

The states are `starting`, `ready`, `unhealthy` (was ready, now fails its check, still running), `restarting` and `stopped`. `startup_secs` only sets when a slow start gets an error line in the status; the node does not kill a sidecar that is still loading its model. `bricolo status` shows each sidecar's state, pid, port, restarts and last error. The error stays after a recovery.

#### Speech

`speech` calls the parakeet contract: `POST /transcribe` with the path of an audio file as the body. A 200 carries the text. An error status with a body starting `__ERROR__` becomes `failed` with the rest of the body. A refused connection is `unavailable`, so the router tries the next target. `transcribe` writes the bytes to `$TMPDIR/bricolo-speech/` under the format's extension, posts that path and deletes the file. `transcribe-file` checks the path against the plugin's `disk:` grants first. The server reads the file itself, so it must run on this machine. The streaming endpoints (`/start`, `/partial`, `/finish`, `/cancel`) and `/words` are not used yet.

#### TTS

The `:8790` queue from `apps/tts.lua` lives in the node's `tts` provider:

- `speak` splits the text with the rules of `lib/tts_core.lua` (sentence ends, then merging up to `max_chunk`, then word wrapping), queues the chunks and returns. The Rust version counts characters where the Lua one counts bytes.
- One worker speaks one chunk at a time. With a TTS server it posts the chunk to `/speak` with `X-Format: path` and `X-Voice`, then plays the returned WAV with the player. Without one, on macOS, it pipes the chunk to `say` (with `-v` when a voice is set).
- The voice argument resolves like `M.resolveVoice`: none means `[tts] voice`, a profile name maps to its voice, anything else is used as is.
- A chunk whose synthesis fails is logged and skipped. `stop` clears the queue and kills the player or `say` mid-chunk. A synthesis request already sent finishes on the server, and its result is dropped.
- `status` returns `speaking`, `queued` and the default voice.

#### Claude Code

`claude-code` runs the `claude` CLI headless (`crates/providers/src/claude_code.rs`), so a voice agent or a script can hand a coding task to the Mac and follow it:

- `start` canonicalizes the folder (after expanding `~/` on the node that runs it) and refuses with `denied` one that is not under a `[claude-code] dirs` root. It runs `<command> -p <prompt> --output-format stream-json --verbose --permission-mode <mode> <extra_args>` in that folder, with stdin closed, and reads stdout as JSON lines. The `system` `init` line gives the session id; `start` answers then, with the session `working`. No id within 25 seconds is `timeout` and the child is killed. That stays under the router's default 30-second timeout, so a lower `timeout_secs` in rules.toml can cut a start short.
- A `result` line ends the turn: `is_error` or a subtype other than `success` is `failed`, a non-empty `permission_denials` is `waiting`, anything else `finished`. Its `result` text becomes `last-result`. An exit without a result line is `failed`, with the end of stderr as `last-result`.
- `send` resumes a session that is not working with `--resume <id>` in the same folder and answers once the child starts. `stop` kills the running child and sets `idle`.
- The sessions live in the node's memory: `list` shows the ones started since the node started, by any plugin, script or peer. A command that is not found is `unavailable`, and so is every call without `dirs`.
- It is routable: a phone or a laptop can start sessions on the Mac with `claude-code = ["mac"]` in its rules.toml, subject to the Mac's peer allow list.

#### Command line

- `bricolo fire <plugin> <id>` delivers `hotkey(<id>)` to a running plugin, as if the chord was pressed. The control socket accepts `{"cmd":"fire","plugin":"…","hotkey":"…"}`.
- `fixtures/caps` is a test plugin that calls `clipboard`, `window`, `app`, `notify`, `tts`, `hotkey`, `shell` and `http` on command (`bricolo fire caps clip-roundtrip`, `apps`, `windows`, `speak:hello`, `bind:ctrl+alt+f19`, `shell:/bin/echo` …).
- `plugins/window-tiler` binds cmd+alt+left, right and up, and tiles the focused window into the visible frame of the screen it is on.

#### Tests

- The providers' tests run fake servers: a small HTTP server thread for `http`, `speech` and `tts`, and a Python HTTP server as a sidecar. The sidecar tests skip themselves without `python3`, and the port test without `lsof`.
- `crates/runtime/tests/plugins.rs` loads `fixtures/caps` and `plugins/window-tiler` through the real runtime: allow lists, a refused update with a new allow-list entry, the desktop capabilities without the main-thread loop, and the tiler going off when it cannot bind hotkeys.
- Three tests are `#[ignore]`: `clipboard_round_trip` (touches your clipboard and needs a GUI session), `real_parakeet_transcribes_say_output` and `real_pocket_tts_writes_a_wav` (need the servers; set `BRICOLO_TEST_PARAKEET_URL` and `BRICOLO_TEST_POCKET_URL`). Run them with `cargo nextest run --run-ignored all`.
- Hotkey presses have no automated test, because a test cannot press keys without the Accessibility permission. By hand: bind a chord with `bricolo fire caps bind:ctrl+alt+shift+cmd+k`, then post the key with a CGEvent. A synthetic F19 did not trigger Carbon hotkeys on macOS 26, even in a plain Swift app; letter chords did.

#### cargo deny

- `[graph] targets` lists the platforms we build for. Without it, cargo deny walks tao's Linux dependencies (gtk), which we never build.
- RUSTSEC-2024-0370 (`proc-macro-error`, unmaintained) is ignored for the same reason: only those gtk crates reach it.
- `webpki-root-certs`, Mozilla's root list, has a data license (CDLA-Permissive-2.0), allowed for that crate only.

#### Not done in phase 2

- The Fn key through a CGEventTap. `hotkey` covers modifier chords only, and only key presses, not releases.
- The `:8790` HTTP intake, the `hs -c 'speak()'` entry point and "speak the selection" (Fn+S).
- Live dictation and word alternatives from the parakeet server.
- A list of voices from the TTS server.
- The signed `.app` bundle (Decision 5). Run from a terminal, macOS records the Accessibility permission against the terminal app.
- Desktop capabilities on Linux and Windows.

### Phase 2: the three parts together

Merged on 2026-11-16: the Lua tier, the mesh and the macOS providers were built on separate branches. These notes record what the merge decided.

Startup and shutdown:

- `bricolo run` calls `desktop::run`, and the node, Tokio included, runs inside it on the `bricolo-node` thread. `Node::start(paths, NodeOptions { watch, mesh, desktop })` is the only constructor. It starts the mesh (unless `[mesh] enabled = false`), then the runtime with `Options { watch, other_events, desktop }` and the mesh as its `Peers`, hands the runtime's local providers to the mesh, then starts the scripts on the routed providers. `NodeOptions::default()` (no watcher, no mesh, no main-thread loop) is what the tests use.
- `Node::shutdown` stops the scripts, then the runtime, which stops the plugins and then the sidecars, then closes the mesh.
- tao activates the app when its event loop launches, unless told not to. That took focus from the app in front, so right after `bricolo run` started, `app.frontmost` answered `bricolo` and the window tiler had no focused window to tile. The main loop now calls `set_activate_ignoring_other_apps(false)`. In the merge smoke test, the app in front stayed in front.
- The control socket serves `status`, `reload`, `reload-script`, `reload-peers`, `call` and `fire`. `status` has `plugins` (with their hotkeys), `sidecars`, `scripts` and `mesh`. `mesh` is `{"enabled": false}` when the mesh is off, and has `"enabled": true` next to the mesh's own fields when it runs.

The mesh:

- `notify` is in the remote table, and `impl Peers for Mesh` returns a `RemoteNotify`.
- `tts.status` is in the remote table. `TtsStatus` gained `Deserialize` for it.
- `speech.transcribe-file` is not in the remote table. A remote `speech` provider is a `GuardedSpeech`: its `transcribe_file` checks the path against the calling plugin's `disk` grants, reads the file on the calling node and sends the bytes as `transcribe`, with the format taken from the extension. The other choice was sending the path and checking it against the peer's `disk:` grants, but the path names a file on the calling node, and a peer would read its own disk at the caller's request. With bytes, allowing `speech` grants no disk access.
- A peer's call runs with empty `shell` and `http` grants. Neither capability is routable.

Scripts:

- Scripts are trusted, so they get no `[shell]` or `[http]` allow list. `ShellGrant::any()` and the `http` entry `*` (`shell::ANY` and `http::ANY`) stand for every program and every host. `plugin.toml` cannot express them, because its entries must be absolute paths, variable names and `host:port`. The `*` grant still requires an absolute program path and still keeps `HOME` and `PATH` for the node.
- New tables: `bricolo.window`, `bricolo.app`, `bricolo.notify`, `bricolo.shell.run` and `bricolo.http.request`, and `bricolo.tts.status`. Frames use Hammerspoon's `x`, `y`, `w` and `h`; `set_frame` also accepts `width` and `height`. `bricolo.http.request` takes a URL or a table, and returns header names in lowercase with repeated headers joined. `bricolo.speech.transcribe(path)` still reads the file in the Lua tier and calls `transcribe`, rather than `transcribe-file`.

cargo deny:

- Both phase 2 branches allowed `CDLA-Permissive-2.0`: the mesh for every crate, the macOS branch for `webpki-root-certs` only. The merge keeps it per crate, for `webpki-roots` (iroh's relays) and `webpki-root-certs`. cargo deny reports the `webpki-root-certs` exception as not encountered, because no crate reaches it for the targets in `[graph]`; it warns and passes.

### Phase 3: Linux

Built on 2026-11-16 in `crates/providers/src/linux/`. [linux.md](linux.md) is the user-facing page: what works on X11, on Wayland and with no display, the packages, and the tests. These notes record the choices behind it. They replace the phase 2 note that the desktop capabilities answer `unavailable` on Linux.

Backends:

- `window`, `hotkey`, `app.running` and `app.frontmost` use X11 through x11rb's pure Rust connection, with the `randr` feature. One lazily opened connection serves `window` and `app` (`x11.rs`), and reconnects after the server goes away. Calls run on Tokio's blocking pool. The pure EWMH parts (text properties, `WM_CLASS`, frame extents, the client message data, work areas) live in `ewmh.rs` with unit tests.
- Frames are the outer frame, decorations included, to match the macOS backend. `set-frame` sends `_NET_MOVERESIZE_WINDOW` as a pager with north-west gravity after removing the maximized state. In the tests, openbox put the frame's corner exactly where asked.
- `hotkey` grabs keys with `XGrabKey` on its own connection, read by a thread named `bricolo-hotkeys`. We did not use global-hotkey's X11 backend, which the macOS code uses: it registers on its own thread and does not report a chord another program already holds, and we wanted that as an error. Chords map to keysyms in `hotkey.rs`, then to keycodes through the server's keyboard map. Each chord is grabbed with and without Caps Lock and Num Lock. `cmd` is Mod4 and `alt` is Mod1.
- `clipboard` is arboard 3 with `wayland-data-control` and no default features (no image support). arboard wants one thread per clipboard, so a thread named `bricolo-clipboard` owns it and keeps it for the node's lifetime: on X11 the clipboard only holds what we set while the owner lives.
- `notify` calls `org.freedesktop.Notifications.Notify` with zbus 6 on Tokio. zbus 5 pulled in `zvariant` 5, which RustSec lists as unmaintained. A missing session bus or notification server is `unavailable`; a call that takes over 10 seconds is `timeout`.
- `app.launch` resolves desktop files itself (`app.rs`, with tests) and starts them with `gio launch`, then `gtk-launch`, then the parsed `Exec` line. If a window of the app is open, it raises that window instead, like `open -a`. `app.open-url` runs `xdg-open` and refuses `file:`, as on macOS. `check_app` in `providers/src/app.rs` now holds the name check both backends share.
- `tts` without a server uses the first of `espeak-ng`, `espeak` and `spd-say`. `Engine::Say` covers the two espeaks, since they take the text on stdin and `-v` as `say` does. `Engine::SpdSay` is new: `spd-say` takes the text as an argument, and `stop` also runs `spd-say -C`. The default WAV player is the first of `pw-play`, `paplay` and `aplay` on `PATH`.

Sessions:

- The providers read the session from the environment on each call (`linux::Session`): `WAYLAND_DISPLAY` or `XDG_SESSION_TYPE=wayland` is Wayland, else `DISPLAY` is X11, else headless. Wayland wins over `DISPLAY`, because XWayland only shows X11 apps.
- On Wayland and without a display, the X11 capabilities answer `unavailable` and log the reason once per provider. The XDG GlobalShortcuts portal (through `ashpd`) is not wired in: it needs a portal backend and a user confirmation dialog we could not test in a container.
- `desktop::run` still runs the node on the calling thread on Linux and passes no `Desktop`. Unlike macOS, the Linux providers need no main-thread loop, so they also work in tests. The two runtime tests that expect `unavailable` (`desktop_capabilities_without_the_main_thread_loop`, `window_tiler_needs_hotkeys`) skip themselves on Linux when a display is set.

Tests and tooling:

- `just linux-test` runs `scripts/linux-test.sh`, which builds the image in `scripts/linux-test/` (`rust:1-bookworm`, the pinned toolchain, nextest, Xvfb, openbox, dunst, xterm, xdotool, xclip, espeak-ng) and runs `scripts/linux-test/inside.sh` in it. The checkout is mounted at `/src`, with Docker volumes over `/src/target` and the cargo registry. Inside: clippy and the whole suite with no display; then, inside `dbus-run-session`, Xvfb at 1280x800 with openbox and dunst, the `x11_*` tests and `clipboard_round_trip` one at a time; then `smoke.sh`, which runs `bricolo run` with the tiler, `fixtures/caps` and a Lua script.
- The X11 tests are `#[ignore]`, so `cargo nextest run` stays green without a display. They are in `crates/providers/tests/linux_desktop.rs` (clipboard against xclip, windows, screens, hotkeys with xdotool, apps and desktop files, notifications read back from dunst's history) and `crates/runtime/tests/plugins.rs` (the tiler end to end).
- CI has a `linux-desktop` job that installs the same packages on `ubuntu-latest` and runs `inside.sh desktop`. The matrix job still runs `just ci` there with no display.
- cargo deny walks arboard's Windows dependencies (`clipboard-win`, `error-code`) because `[graph] targets` lists Windows. Since the Windows port, arboard is built there too. Both are BSL-1.0, the permissive Boost license, allowed for those two crates only.

Not done in phase 3:

- Hotkeys on Wayland (the GlobalShortcuts portal), and windows on Wayland through GNOME or KDE interfaces.
- Key releases and the Fn key, as on macOS. A held chord repeats with the keyboard's auto-repeat.
- Hotkeys do not follow a keyboard layout change made while the node runs: the keycodes come from the keyboard map read at connection time.
- `app.running` has no `bundle-id` on Linux; matching windows to desktop files is left out.
- Screen coordinates are pixels, not points as on macOS. Scaled X11 desktops are not handled.

### Phase 4: UI (Decision 8)

Built on 2026-11-16 in `crates/ui`, with Tauri 2.12 and `tauri-nspanel` 2.1. [ui.md](ui.md) is the user-facing reference. These notes record the choices the design left open.

The process and the socket:

- `bricolo-ui` is its own binary and links no Wasmtime: it finds the socket through `BRICOLO_DATA_DIR` with a copy of the few lines of `Paths::from_env` it needs.
- The control socket gained `{"cmd":"subscribe"}`, which turns the connection into a stream of JSON lines: `status`, `ui` and `logs` at once, then `status` when it changes, `ui` after each tile or panel change, and one `log` per line. The node compares the status, without `uptime_s`, after each request that changes something, when a sensor changes, and once a second, which catches crashes and sidecar changes nobody announces. The UI does not poll. A node without `subscribe` answers it with an error, and `bricolo-ui` then asks for `status` every 2 seconds.
- `bricolo-ui` reconnects after 250 ms, doubling to 2 s, and reports an outage once. Start node runs `bricolo run` from next to `bricolo-ui` or from `PATH`, in its own process group, with output in `<data>/logs/bricolo-run.log`.
- Log lines reach the hub through `bricolo_providers::log::feed()`, a process-wide buffer of 500 lines with a broadcast channel. The `log` provider pushes plugin and script lines there. The node's own `tracing` lines get there through a layer in `logs.rs`, filed under the `plugin` or `sidecar` field they carry, or under `node`. A global was the simplest way to collect lines from code that never sees the node, and `tracing` is global anyway.
- The hub may send `status`, `reload`, `reload-script`, `enable`, `disable`, `restart-sidecar`, `ui-action` and `fire`. `bricolo-ui` refuses any other request from its pages.

The `ui` capability:

- `wit/ui.wit` has `set-tile`, `clear-tile`, `set-widget`, `clear-widget`, `show-panel` and `hide-panel`. They are sync, like `kv`: they only change state in the node. The package stays `bricolo:stdlib@0.1.0`.
- What the user does arrives as a new `event` case, `ui(ui-event)`, with `switch`, `button`, `choice` and `review`. Adding a case changes the type of `on-event`, so plugins built before this phase need a rebuild; `just plugins` rebuilds the ones in this repository.
- One tile per owner, drawn in its card as its controls. One widget per owner, display only (a title and up to 16 label and value rows), drawn in the Widgets view; `set-widget` came after `set-tile`, and adding a function keeps plugins built before it loading. One panel per owner and kind: showing a kind again replaces the panel and keeps its id, so a caption can update many times a second. Each show gets a sequence number, and the UI draws the newest panel of each kind. Toasts hide after 4 seconds unless they set `timeout-ms`.
- Limits keep one plugin from flooding the hub: 200 bytes for titles, labels and choices, 64 KiB of panel text, 16 switches and 16 buttons with unique ids, 50 choices.
- `ui` is not routable and is not in the mesh's table. `Providers::release` drops the owner's tile, widget and panels when a plugin or script stops.
- Scripts get `bricolo.ui` with callbacks rather than events: `on_switch` and `on_button` per tile, `on_choice` and `on_review` per panel, kept in the script's callback registry. `on_choice` gets a 1-based index and the label.

Turning plugins and scripts off:

- Both tiers gained the state `disabled`. A disabled plugin or script does not start, a reload or a saved file leaves it off, and `fire` answers "not running".
- The node keeps the list in `<data>/state.toml`, not in `node.toml`. `node.toml` is configuration you edit and may keep in dotfiles, and the node never writes it. The node writes `state.toml` through a temporary file and a rename.

Changing shortcuts:

- `hotkey::Remap` wraps the platform's hotkey backend in `Providers::local`. A plugin or script still binds its own chord; `Remap` binds the chord the user picked in its place, so no app needs code for it. Changes are keyed by owner and the chord the app asks for, not by id, because script ids (`h1`, `h2`) follow the order of `bind` calls.
- The `set-hotkey` request moves one binding (`HotkeyProvider::rebind`); when the backend refuses the new chord, the old one stays bound. `Remap` writes `<data>/hotkeys.toml` through a temporary file and a rename, like `state.toml`.
- `status` gives scripts `hotkeys` too, and plugins and scripts `hotkey_defaults` for the bindings that were moved. The hub draws every hotkey with one control, `hotkeyChip` in `crates/ui/ui/js/chord.js`.

Other additions:

- `Sidecars::restart` stops a sidecar's process group and starts it again at once, skipping the backoff.
- `status` has `active_sensors` (Decision 5) and `rules`, the routes the router read at start. The `mic` provider sets the sensor since phase 5b. The `sensor` request sets one by hand, for testing the recording dot.

Panels on macOS:

- Each panel kind has one window, made hidden at startup and turned into a non-activating `NSPanel` (`tauri-nspanel`, from crates.io) at the status window level, on every Space and over full-screen apps. Captions and toasts ignore the mouse and show with `orderFrontRegardless`. Choice lists and review panels show with `show_and_make_key`: they get the keyboard while the app in front stays active.
- `bricolo-ui` runs with the `Accessory` activation policy, so it has no Dock icon. Opening the hub activates it with `activateIgnoringOtherApps`, because `activate` needs macOS 14. macOS 14 and later honour that only after the user acts on the app, so the hub opened from the menu comes to the front, and the hub a terminal launch opens may stay behind the terminal.
- The hub window has the `MoveToActiveSpace` and `FullScreenAuxiliary` collection behaviours. Without them it opened on the desktop Space while a full-screen app was in front, and stayed out of sight.
- Captions, toasts and review panels sit at the bottom center of the work area of the screen the user works on, and choice lists in its upper third (`crates/ui/src/placement.rs`). On macOS that screen is `NSScreen.mainScreen`, the one with the focused window, else the one with the pointer; elsewhere it is the one with the pointer, else the primary monitor.

Front end:

- `crates/ui/ui/` is static HTML, CSS and ES modules. `withGlobalTauri` provides `window.__TAURI__`, so there is no npm and no bundler. The CSP allows only the app's own files.
- Pages build the DOM with a small `h()` helper and set text with `textContent`, because tiles and panels carry strings from plugins.

Tooling:

- `just ui` builds `bricolo-ui` and `bricolo` in release mode. `just check-linux` skips `bricolo-ui`: Tauri needs GTK and WebKitGTK through `pkg-config`, which a zig cross-check does not have. The Ubuntu CI job installs them and builds and tests `bricolo-ui` natively.
- cargo deny allows MPL-2.0 for five crates only. `cssparser`, `cssparser-macros`, `selectors` and `dtoa-short` reach only `tauri-codegen` and `tauri-build`, at compile time. `option-ext` is linked through `dirs`. MPL-2.0 is copyleft per file; we use the crates unmodified, so `bricolo-ui` stays MIT. The reason for ignoring RUSTSEC-2024-0370 now names Tauri's GTK on Linux as well.

Not done in phase 4:

- The signed `.app` with the node inside, and starting it at login (phase 5).
- The control socket on Windows, so `bricolo-ui` there always shows the node as down. The Windows port added a named pipe; see "Windows" below.
- Editing `rules.toml` or `peers.toml` from the hub; it only shows them.

### Phase 5a: building and sharing apps (Decision 7)

Built on 2026-11-16. [apps.md](apps.md) is the user-facing page. These notes record the choices behind it. They replace the phase 1 note that `bricolo update`, `bricolo install --from-lock` and `github:` sources are not done.

Installing:

- `bricolo install` takes a source: a built plugin folder (as before), a `.lua` file, a folder of scripts, a bundle file, or `github:owner/repo[@tag]`. `Source` in `crates/runtime/src/source.rs` parses it; `install::resolve` turns it into packages, each a checked plugin `Plan` or a `ScriptPlan`.
- The release convention is one bundle per app: `<name>.rhplugin.tar.gz` (the installed `plugin.toml`, without its `wasm` key, and `plugin.wasm`) or `<name>.rhscripts.tar.gz` (`.lua` files; those at the root run). `bricolo pack` writes them. We picked bundles over two bare assets because one release can then carry several apps, and scripts need more than one file. A release with bare `plugin.toml` and `plugin.wasm` assets, as Decision 7 first described, still installs as one plugin.
- Bundles are gzip-compressed tar (the `tar` crate without default features, and `flate2` with its pure Rust backend). `pack` sorts names and zeroes mtimes, so the same files give the same bytes. `unpack` refuses links, absolute paths, `..`, repeated paths, and more than 64 MiB in total.
- The GitHub client (`github.rs`) uses ureq with rustls and ring, the same setup as the `http` capability, so there is still no OpenSSL. It reads `releases/latest` or `releases/tags/<tag>` and downloads each asset through its API `url` with `Accept: application/octet-stream`, which works for private repositories. `GITHUB_TOKEN` goes only to URLs under the API base; ureq drops the header when GitHub redirects the download to its storage host. `BRICOLO_GITHUB_API` replaces the API base, for tests.
- Tag pinning: `github:owner/repo` resolves to the latest release, and the lock records `github:owner/repo@<tag>`. Plugins keep `version` from `plugin.toml`; script bundles use the tag, or `local`.

The lock:

- `plugins.lock` gained `[scripts.<name>]` with `source`, `version`, `sha256` and `files`, written only when a script bundle is installed, so older locks read and write unchanged. A script bundle's `sha256` hashes its files' paths and contents (`bundle::content_hash`), not the archive, so it doesn't depend on compression.
- `bricolo install --from-lock` downloads each entry from its locked source and refuses it if the hash differs. Entries already on disk with the locked hash are skipped. It asks for approval like `bricolo install`, unless `-y`.

Updates:

- `bricolo update` compares the locked tag with the latest release. Tags compare as dotted numbers after an optional `v`; other tags count as newer when they differ. Path sources are skipped.
- For a plugin it prints the capabilities added and removed. When the new version asks for something not granted, it asks (or takes `-y`). Without approval it still writes the new files and lock entry but leaves `permissions.toml` alone, so the node refuses to start the new version: it stays off, as Decision 5 asks. A running node keeps the old version, because a reload whose checks fail keeps the old one (Decision 6). `bricolo update <name> -y` later grants what the installed version asks for.
- Script bundles update without a capability question; `bricolo update` prints the scripts warning and the changed files.

Scripts:

- Scripts installed with `bricolo install` are copied into `scripts/`, replacing files with the same path; files that an earlier version of the same bundle installed and the new one lacks are removed. `bricolo install` prints a warning that scripts run with full access and asks unless `-y`. `bricolo uninstall <name>` removes a bundle's files.
- `bricolo check <file>.lua` compiles the script with a plain Lua state (`bricolo_lua::check_syntax`). It catches syntax errors only.

Templates:

- `templates/` holds one folder per `bricolo new` template (`plugin`, `hotkey-tool`, `voice`, `window-tool`, `http-service`), `templates/common/` for the files every crate gets, and `templates/scripts/<name>.lua` for `bricolo new-script`. `crates/node/build.rs` embeds every file with `include_str!`, so an installed `bricolo` needs no checkout.
- Each plugin template splits `src/domain.rs` (pure logic, settings parsing and unit tests) from `src/lib.rs` (bindings and capability calls), as in the plugin-host spike. Templates depend on `serde_json` to read their settings.
- Template plugins log a failed hotkey bind instead of failing `init`, so they stay `running` and reachable through `bricolo fire` when a chord is taken or hotkeys are unavailable. Script templates do the same. Plugin and script chords differ so both versions of a template can run together.
- `bricolo wit [name|all] [--out dir]` prints or writes the standard library WIT. `types.wit` comes first in the list, so `bricolo wit all` is one valid WIT file. A test checks the list in `scaffold.rs` against `wit/`.
- `scripts/smoke-templates.sh` (`just smoke-templates`) generates every template in a temp folder outside the repository, runs its `cargo test`, builds it, runs `bricolo check`, installs it into a temp home, runs `bricolo run` and checks that `bricolo status` shows all of them running.

The skill:

- `skills/create-bricolo-app` is a Claude Code skill that builds an app from a one-line request. Its `references/capabilities.md` is generated from `wit/` and the `bricolo` table in scripts.md by a test in `crates/node/tests/skill.rs`; `just skill-refs` rewrites it, and the test fails while it is out of date. The same test file compiles the skill's Lua examples.
- `scripts/eval-create-app.sh` runs `claude -p` with the skill in an empty folder against a node in a temp Bricolo home, then checks that exactly one new app runs and binds the chord the request names.

Not done in phase 5a:

- `bricolo update` doesn't list release notes or check signatures. A release's integrity rests on the sha256 in `plugins.lock`.
- Prereleases and drafts are ignored, because GitHub's `releases/latest` ignores them.
- `bricolo check` for scripts doesn't catch unknown `bricolo` calls or wrong argument types.

### Phase 5b: keyboard, microphone and dictation

Built on 2026-11-16. [apps.md](apps.md#dictation) is the user-facing walkthrough. These notes record the choices behind it.

New interfaces:

- `keyboard`: `keystroke(mods, key)` and `type-text(text)`. Key and modifier names go through `Chord::from_parts` in `providers/src/hotkey.rs`, so they are the hotkey names. `type-text` refuses more than 20000 characters, as `apps/keystroke_typer` did.
- `mic`: `record-start(options)` returns an id, `record-stop(id)` returns the file's path and `record-cancel(id)` deletes it. `options` has `max-secs` (at most 600; 0 means 600) and `format`, whose only case is `wav16k-mono`. `level` was left out.
- Both are grants like any other import (`keyboard`, `mic` in `permissions.toml`). Neither is routable, and neither is in the mesh's remote table: a peer typing on another device's screen is too dangerous to allow, and a recording's path names a file on the recording node. A remote microphone will use media streams ([mesh.md](mesh.md)).

The keyboard:

- macOS posts CGEvents at the HID event tap (`objc2-core-graphics`). `CGPreflightPostEventAccess` is checked first, and its absence answers `denied`, because macOS drops posted events silently without the Accessibility permission. That is the permission `window` uses, the terminal's when `bricolo run` runs in one. Each event sets its own modifier flags, so keys the user still holds (the chord that triggered the call) don't leak in.
- macOS keys are virtual key codes, which are positions on an ANSI keyboard. On a layout that moves V, such as Dvorak, `keystroke({"cmd"}, "v")` presses the key in V's US position.
- `type-text` on macOS types one key press per character, about 12 ms apart (4 ms between events, 4 ms more after each character), so text lands where a paste is refused. Enter and Tab are keys. A character with a key on a US layout presses that key, with Shift when it needs it, and carries the character as the event's Unicode string too, so local apps type it on any layout. Any other character goes out as the Unicode string of its own key event (key code 0).
- Linux uses XTEST on a connection opened for each call, on Tokio's blocking pool. Keys go through the server's keyboard map, so they follow the layout. Modifiers held when the call starts are released first. A character the map lacks is mapped onto an unused keycode first, as xdotool does; those keycodes keep the mapping. Restoring them right after the press could change a key before a slow client has looked it up. Wayland answers `unavailable`.
- The keystroke path was not run on the Mac, because a test key press would land in the app in front. `keyboard_mic_and_os` (Lua) and the dictation tests use a fake; the X11 tests run the real backend.

The camera:

- `camera.snapshot` runs one ffmpeg child: `-hide_banner -loglevel error <input> -frames:v 1 -y <out>`, with the input `-f avfoundation -framerate 30 -i <device>` on macOS (device defaults to `0`) or `-f v4l2 -i <device>` on Linux (default `/dev/video0`). `[camera] device` replaces the device; `[camera] ffmpeg` names the program, found as `[mic] ffmpeg` is. No ffmpeg is `unavailable`.
- Photos are `<data>/<plugin>/camera/<ms>.jpg`; the newest 20 stay. A non-zero exit is `failed` with the last line of ffmpeg's stderr and leaves no file. ffmpeg is killed after 10 s and the call answers `timeout`. The caller shows under `active_sensors` as `camera` while ffmpeg runs.
- `NSCameraUsageDescription` is in the `bricolo` Info.plist next to the microphone string.

The microphone:

- One ffmpeg child per recording: `-f avfoundation -i :<device>` on macOS, `-f pulse -i <device>` then `-f alsa -i <device>` on Linux, with `-t <max-secs> -ar 16000 -ac 1 -c:a pcm_s16le -flush_packets 1`. `[mic] device` defaults to `default`; `[mic] ffmpeg` names the program, otherwise the node looks on `PATH`, then in `/opt/homebrew/bin`, `/usr/local/bin` and `/usr/bin`, since a node started by launchd has no Homebrew folder on its `PATH`.
- `record-start` answers once samples follow the header of the WAV `data` chunk. ffmpeg's header is 78 bytes, since it adds a `LIST` chunk; a first version compared the file with the classic 44 bytes and accepted a header-only file as a recording. Without `-flush_packets 1`, ffmpeg buffered the WAV and the file showed no audio for over 3 seconds; with it, the first audio arrived after 0.6 s from a terminal and 2.1 s inside the node in the measured runs. If ffmpeg exits first, the input counts as missing and the next one is tried; after 3 s with neither, the call answers anyway and logs a warning. In one run on the Mac the default input sent no audio at all, for reasons not found; `record-stop` now answers `failed` with "recorded no audio" in that case.
- `record-stop` sends SIGINT, which makes ffmpeg finish the file, waits 3 s, then kills it. A file with no samples is `failed`, with the last line of ffmpeg's stderr. `record-cancel` kills ffmpeg and deletes the file.
- One recording at a time per plugin or script; a second `record-start` is `failed`. Files are `<data>/<plugin>/recordings/rec-<ms>-<id>.wav`, and a new recording deletes all but the newest 9 before it starts, so 10 remain.
- The runtime now gives every plugin a read-write `disk` grant on its own data folder, which WASI already mounts at `/data`. That is what lets `speech.transcribe-file` take the path `record-stop` returns without a `disk:` permission. It adds no access the plugin lacked.
- `Providers::release` kills a plugin's or script's recordings and deletes their files, and `Providers::shutdown` kills every recording before the sidecars stop. In the run on the Mac, SIGTERM to `bricolo run` during a recording left no ffmpeg behind.
- `bricolo status` has `active_sensors`, a list of `{"plugin": "<name>", "sensor": "mic"}`. Scripts appear as `<name>.lua`. The list is the phase 4 `Sensors` registry, the one the hub's recording dot, the menu bar item and the `sensor` request use. `FfmpegMic` holds the same `Arc<Sensors>` and turns the caller's `mic` on when `record-start` succeeds, and off on stop, cancel, release and shutdown. A recording that reached `max-secs` has stopped by itself: a task checks its ffmpeg every 100 ms and turns the sensor off, though the file waits for `record-stop`. Each change wakes the `subscribe` stream, which sends the new status at once.
- Merging with phase 4 renamed the entry's `owner` field to `plugin`, the name the voice branch used. The `sensor` request takes `plugin` and still accepts `owner`.

Scripts:

- New calls: `bricolo.keyboard.keystroke(mods, key)` (also `keystroke("cmd+v")`), `bricolo.keyboard.type_text`, `bricolo.mic.record_start`, `record_stop` and `record_cancel`, and `bricolo.os()`, which returns `std::env::consts::OS` ("macos", "linux").
- `bricolo.speech.transcribe_file(path)` is new and calls `transcribe-file`, so the speech server reads the file itself. `bricolo.speech.transcribe` still reads the file in the Lua tier.

Dictation:

- `examples/apps/dictation/dictation.lua` ports the core of the Hammerspoon `apps/dictation`: a chord toggles recording, then transcribe, copy and paste. States are idle, starting, recording and transcribing; presses while starting or transcribing are dropped. A 60 s timer stops a forgotten recording, backed by the mic's own `max_secs` of 65. A caption from `bricolo.ui.caption` stays up while it records and transcribes, and a toast reports errors. `crates/lua/tests/scripts.rs` runs the file unchanged against fakes.
- `bricolo new --template dictation` is the same app as a plugin. `domain.rs` is the state machine (inputs in, actions out) with its tests; `lib.rs` runs the actions and feeds results back. A plugin can't see the OS, so the paste chord is the `paste` setting, `cmd+v` by default. Its chord is ctrl+alt+cmd+d, so it can run next to the script on ctrl+alt+shift+d. It shows its messages in a `ui` toast. Plugin events are handled one at a time, so a press during transcription waits and then starts a new take, unlike the script, which drops it.
- `just smoke-templates` records through a fake ffmpeg set in its `node.toml`, fires `toggle` twice and prints `active_sensors` in between.

Tests:

- The `mic` unit tests, the runtime test (`a_recording_lands_in_the_plugins_folder_and_transcribes`) and the stream test (`a_recording_shows_in_active_sensors` in `subscribe.rs`) run a fake ffmpeg shell script. On a loaded Mac, a script written a moment earlier took over 3 s to start while macOS checked it, so the tests wait for the fake's file instead of assuming a start time. `real_mic_records_two_seconds_of_16k_mono` is `#[ignore]` and needs a microphone.
- `scripts/linux-test.sh` passes `CARGO_BUILD_JOBS` into the container. In colima's 8 GiB VM the linker was killed for lack of memory twice with the default job count, and the run passed with `CARGO_BUILD_JOBS=2`.
- `x11_keyboard_keystroke_reaches_a_hotkey_grab` presses a chord with XTEST and sees the node's own grab fire. `x11_keyboard_types_into_an_xterm` types shifted characters, an é the US map lacks, and Enter into an xterm whose shell writes the line to a file.

Not done in phase 5b:

- An input level meter (`level`) and a remote microphone.
- Keyboard layouts on macOS (translating a key name through the current layout) and keyboard input on Wayland.
- Live dictation, the review panel and the rewrites of the Hammerspoon app.

### Phase 5c: packaging and the service (Decisions 5 and 8)

Built on 2026-11-16. [install.md](install.md) is the user-facing page. These notes record the choices behind it. They replace the phase 2 and phase 4 notes that the signed `.app` and starting at login are not done.

The bundle:

- `scripts/bundle-macos.sh` (`just bundle-macos`) runs `cargo tauri build --bundles app --no-sign` for `bricolo-ui`, puts `bricolo` in a helper app, and signs the helper, then the app. `crates/ui/Info.plist` is merged into the app's Info.plist by Tauri: `LSUIElement` and the microphone, camera and Apple Events usage strings.
- The node lives in `Contents/Helpers/bricolo.app`, bundle id `com.pierre-mike.bricolo.node`, not in `Contents/MacOS` next to `bricolo-ui`. Decision 8 asked for one bundle id, and the first build did that: `bricolo` in `Contents/MacOS`, signed with the app's identifier. But `bricolo run` starts an NSApplication loop for the main thread, and LaunchServices registered the running node as bricolo.app. `open` and Finder then only brought the node forward and never started the hub. With the helper, the app and the node are two registered apps signed by the same identity. The permissions macOS grants the node attach to the helper, under the name "bricolo node".
- The node checks 6 grants (`crates/providers/src/permissions.rs`): Accessibility, posting events and Input Monitoring through the APIs above, Microphone and Camera through `AVCaptureDevice.authorizationStatus`, and Screen Recording through `CGPreflightScreenCaptureAccess`. The ffmpeg and `screencapture` children record under the node's grants, so the node's answer is the one that counts. Only the first 3 make the startup warning, because the others matter only to apps that use them. The hub's Library view shows all 6 and names their holder: the bundle around `bricolo`, else the terminal from `TERM_PROGRAM` ([ui.md](ui.md#the-library)).
- `crates/node/Info.plist` is the helper's Info.plist. `crates/node/build.rs` also links it into every macOS `bricolo` as the `__TEXT,__info_plist` section, so a plain `bricolo` that a LaunchAgent runs (`--binary`) carries the usage strings too.
- `bricolo-ui` finds `bricolo` next to itself, then at `../Helpers/bricolo.app/Contents/MacOS/bricolo`, then on `PATH` (`launch::find_near`).
- Signing: `$BRICOLO_SIGN_IDENTITY`, else "bricolo dev" from the login keychain, else ad-hoc. The hardened runtime is off, so there are no entitlements. A self-signed identity gives the designated requirement `identifier "<id>" and certificate leaf = H"<sha1>"`, which every rebuild keeps. TCC checks grants against that requirement, so grants survive rebuilds. Ad-hoc requirements name the code hash and change with every build.
- `scripts/macos-dev-identity.sh` makes the identity with macOS's LibreSSL (its PKCS#12 output is what `security import` reads), imports it with `-T /usr/bin/codesign` so signing never prompts, and asks for code-signing trust with `security add-trusted-cert`. On this Mac the trust step finished without a dialog. Signing does not need it; `find-identity -v` only lists trusted identities.

`bricolo service` (`crates/node/src/service.rs`):

- Each action is a list of `Step`s (make a folder, write a file, remove a file, run a command with `OnFail::Stop`, `Ignore` or `Retry(n)`), built by pure functions from a `Context` (home, the node's folders, the XDG folders, the `BRICOLO_*` variables that are set) and a `Target` (`bricolo`, `bricolo-ui`, the app, the Linux session). `apply` carries the steps out through a `Runner`, or prints them with `--dry-run`. The tests run the plans in a temp folder with a fake runner.
- macOS: `install` runs `launchctl bootout` (ignored if not loaded), writes the plist, `launchctl enable` (a disabled service refuses to bootstrap), then `launchctl bootstrap gui/<uid>`, retried 10 times half a second apart, because launchd can still be tearing the old instance down right after `bootout`. `stop` is `bootout`, which keeps launchd from restarting the node; the file stays, so it starts at the next login. `start` is `bootstrap` (ignored when loaded) then `kickstart`.
- The plist sets `ProcessType` `Interactive`, so macOS does not throttle the node that handles hotkeys, `LimitLoadToSessionType` `Aqua`, and `AssociatedBundleIdentifiers` with the app's id, which shows the app's name in Login Items.
- `KeepAlive` is `SuccessfulExit` false: `bricolo run` exits 0 on SIGTERM, so a stop stays a stop, and a crash restarts it after launchd's 10-second throttle.
- launchd gives agents only the system folders on `PATH`. The plist sets `~/.local/bin`, `/opt/homebrew/bin` and `/usr/local/bin` before them, because sidecar commands in `node.toml` often name programs there. That is a fixed list, not the installing shell's `PATH`, so the service does not depend on the terminal it was installed from.
- `getuid` for the `gui/<uid>` domain comes from `libc`, a new direct dependency of `bricolo-node` on Unix (already in the lock through `bricolo-providers`).
- Linux: the unit passes the session variables through `PassEnvironment`. The session decides the target: with `DISPLAY` or `WAYLAND_DISPLAY` set at install time, or `--session desktop`, the unit is part of `graphical-session.target`. Otherwise it is wanted by `default.target`. On GNOME the user manager reaches `default.target` before the desktop imports `DISPLAY`, so a desktop node on `default.target` would run headless until restarted.
- `install` on Linux runs `disable` before `enable`, so a reinstall that changes the target leaves no link in the old target's `.wants` folder.
- `--ui` adds a second LaunchAgent for `bricolo-ui --background` with no `KeepAlive`, so quitting the hub sticks; on Linux it adds the desktop entry and an autostart entry.

Other changes:

- `bricolo run` writes colour codes to stderr only when stderr is a terminal. Under launchd and systemd it is a file.
- `scripts/bundle-linux.sh` builds in the linux-test image on macOS and natively on Linux, and adds `bricolo-ui` to the tarball only when `target/release/bricolo-ui` exists. We did not add a Tauri stage to the test image: WebKitGTK and GTK make it much larger, and the release workflow builds `bricolo-ui` on Ubuntu anyway.
- `.github/workflows/release.yml` runs on `v*` tags: the macOS app (ad-hoc), the Linux tarballs for x86_64 and aarch64 in `rust:1-bookworm` containers, and the `bricolo-ui` `.deb` (with `bricolo` added under `/usr/bin` through `bundle.linux.deb.files`) and AppImage on Ubuntu x86_64. A last job creates the release with `gh release create --generate-notes`.

Not done in phase 5c:

- Notarization and a Developer ID. Releases are ad-hoc signed, so a downloaded app needs its quarantine flag removed, and its grants do not survive an update.
- `bricolo-service.log` is not rotated. It gets the same lines as the daily log, which `tracing-appender` rotates.
- We did not click through the Privacy & Security grants for the helper in this phase. The signature and the designated requirement were checked with `codesign`.
- The systemd path was tested only with `--dry-run` and the unit tests: the Linux test image has no systemd.
- Windows has no service. The Windows port added a logon task; see "Windows" below.

### Phase 6: sidecars and the agent skill

Built on 2026-11-16. [apps.md](apps.md#sidecars) is the user-facing page for sidecars and [apps.md](apps.md#writing-apps-with-a-coding-agent) for the skill. These notes record the choices behind them.

Reloading sidecars:

- Phase 2 started the sidecars once, so a new `[sidecars.<name>]` needed a node restart. `Sidecars` now keeps its entries behind a mutex, each with its own stop signal, and `Sidecars::reload(configs)` compares them with a fresh `node.toml`: unchanged sidecars keep their process, removed ones stop, changed ones stop and start again, new ones start. It waits for the stopped processes to exit before starting replacements, so a restarted sidecar finds its port free. After `shutdown` a reload starts nothing.
- The control socket gained `{"cmd":"reload-sidecars"}`. It loads `node.toml` through `NodeConfig::load`, so a file that fails validation is refused and nothing changes, and answers `{"sidecars": {"added", "removed", "restarted", "unchanged"}}`.
- `[tts]` and `[speech]` still resolve their sidecar's URL when the node starts. Reloading them would mean swapping the routed providers under running plugins, so a change there still needs a restart, and the docs say so.

`bricolo sidecar` (`crates/node/src/sidecar.rs`):

- `add` and `remove` edit `node.toml` with `toml_edit` 0.25, already in the lock through iroh's build dependencies, so comments, order and other sections stay. `[sidecars]` is created as an implicit table, so the file gets `[sidecars.<name>]` headers only. The result is parsed as a whole `NodeConfig` and validated before the file is replaced through a temporary file and a rename. This is the one place the node writes `node.toml`; it does so only on an explicit command, which keeps the phase 4 rule that the node never edits it on its own.
- `add` refuses a port another sidecar uses and an existing name without `--force`. A relative command or `cwd` with a slash becomes absolute, because the node starts sidecars from another folder. Arguments come from `--args` or after `--`.
- After an edit, `add` and `remove` send `reload-sidecars`; with no node running they say the file takes effect at the next start. `remove` of a sidecar that `[speech]` or `[tts]` names fails validation, with that section in the message.
- `list` reads `node.toml` and joins the running node's states; `logs` prints the end of `<data>/logs/sidecar-<name>.log`.

Templates:

- `bricolo new-sidecar <name> --template python|node|rust [--port N] [--dir D]` writes `templates/sidecars/<lang>/`. Each listens on `127.0.0.1`, answers `GET /health` with `{"ready": <bool>}` (false until `warm_up` finishes, to show the loading convention) and `POST /process` with the upper-cased text, and has tests and a README. All three use their standard library only: Python `http.server`, Node.js `node:http` with `node --test`, Rust `std::net` with one thread per connection. The Rust crate has its own `[workspace]`.
- Placeholders are `__NAME__`, `__PORT__`, `__DIR__` (the absolute folder), `__PYTHON__` and `__NODE__` (the absolute interpreter found on `PATH` at scaffold time, since a login service has a short `PATH`). The printed `bricolo sidecar add` line uses the same paths, shell-quoted.
- The default port is 8800. The sidecar-client templates, `bricolo new --template sidecar-client` and `bricolo new-script --template sidecar-client`, take `--port` for their URL and the plugin's `[http] allow`; `bricolo new` and `bricolo new-script` gained `--port` for that. The plugin sends the clipboard text, or a sample when there is none, so `bricolo fire` always makes one request. The script polls `/health` every 2 s on load and then sends a sample, so its log shows at once whether it reaches the sidecar.
- `scripts/smoke-templates.sh` scaffolds each sidecar on a free port, runs its tests, registers the Python and Node.js ones before `bricolo run` and the Rust one after, which exercises the reload, and requires every sidecar `ready` and both clients to log an answer. macOS ships bash 3.2, so the script keeps per-sidecar values in files rather than associative arrays.

The skill:

- `skills/create-bricolo-app` keeps its name and now follows the open Agent Skills format: front matter with `name`, `description` (at most 1024 characters), `license`, `compatibility` and `metadata`; no `allowed-tools`, since tool names differ between agents. `SKILL.md` stays under 250 lines and links to `references/`: `sidecars.md`, `app-design.md` (six worked decompositions), `troubleshooting.md` and `agents.md` (where each agent reads skills, with sources) are new.
- `capabilities.md` stays generated; the generator in `crates/node/tests/skill.rs` adds a "what you want" table and an index of interfaces with their full first doc comment. The same test file checks the front matter, that every relative link resolves, and that no file but `agents.md` names an agent's tools or holds an em dash.
- `build.rs` embeds the skill without `evals/`. `bricolo skill install` writes it to `~/.agents/skills/create-bricolo-app` and `~/.claude/skills/create-bricolo-app` by default. The research in `agents.md` found that Codex CLI reads `~/.agents/skills`, Gemini CLI, OpenCode and Cursor read it as well as their own folders, and Claude Code documents only `~/.claude/skills`; those two folders reach all five. `--agent` adds an agent's own folder, `--dir` any skills folder, `--link` makes the later targets links to the first (or to `--source`), `--dry-run` prints the plan. A target that exists must be a link or a folder whose `SKILL.md` is named `create-bricolo-app`; anything else is refused and left alone. A copy goes into a fresh folder that is then renamed into place, with `.bricolo-version` naming the `bricolo` that wrote it.
- `bricolo skill path` lists the installed copies; `bricolo skill snippet` prints a paragraph for an `AGENTS.md` or `GEMINI.md`, for agents without a skills folder. `just install-skill` now runs `bricolo skill install --link --source skills/create-bricolo-app`.
- `scripts/eval-create-app.sh --agent claude|gemini|codex` copies the skill to `.claude/skills` or `.agents/skills` in the agent's empty folder and names its path in the prompt. A request that mentions a sidecar must leave one `ready`, and the installed script or `plugin.toml` must name its port.

Evals run for this phase, one agent run each:

- Claude Code (`claude-sonnet-5-5`) with "a sidecar in Python that reverses text, and a hotkey script that sends the clipboard to it on ctrl+alt+shift+r and shows the result as a toast": every check passed. In 40 s, 8 turns and $0.18 it scaffolded a Python sidecar on port 8820, changed `process()` to reverse the text, ran its tests, registered it with `bricolo sidecar add`, and made the script from the sidecar-client template with the chord changed. The node showed the sidecar `ready`, the script `running` and binding ctrl+alt+shift+r, and the script's log had the reversed sample answer.
- Gemini CLI 0.37.0 with the default script request: it stopped at authentication with `IneligibleTierError` ("This client is no longer supported for Gemini Code Assist for individuals"), before any model call, so the skill was not exercised. Codex CLI and OpenCode are not installed on this Mac.

Not done in phase 6:

- `bricolo pack` does not bundle sidecars, and `bricolo install` does not register them. A sidecar runs outside any sandbox, so installing one from a release would need its own review step.
- `[tts]` and `[speech]` changes still need a node restart.
- The hub shows sidecars but cannot add or remove them.

### Phase 7: the audio capability

Built on 2026-11-17. Before it, no capability played sound, and `shell` kills a program after 60 seconds, so the noise machine looped its WAVs in a sidecar. `audio` (`wit/audio.wit`) moves playing into the node, for scripts and plugins alike.

- `audio::Player` (`crates/providers/src/audio.rs`) keeps every caller's sounds: ids, owners, the `MAX_SOUNDS` limit of 16 per caller, and the `disk:` check that `speech.transcribe-file` also uses, so a plugin reaches its own data folder and its grants, and a script any path. It drops sounds that have ended before it counts or lists them. `Providers::release` stops a plugin's sounds, so a reload stops them too: a script that should keep playing across reloads stores that in `kv` and plays again in `init`.
- A `Backend` starts each sound as a `Voice`; dropping the voice stops it. On macOS, `macos::audio::AvPlayer` uses AVAudioPlayer through `objc2-avf-audio`: `numberOfLoops = -1` loops with no gap, and the volume changes while the sound plays. The players live on one thread, `bricolo-audio`, because Objective-C objects are not `Send`; voices send it commands over a channel. AVAudioPlayer plays on its own audio threads, so `bricolo-audio` runs no run loop, and `isPlaying` turns false by itself when a sound ends.
- Elsewhere `FfmpegBackend` runs one ffmpeg per sound: `-stream_loop -1` to loop, `-af volume=`, and the output `-f pulse` then `-f alsa`, the next one tried when ffmpeg exits with an error within 300 ms. A volume change restarts ffmpeg with the new volume, so on Linux the sound starts again from the beginning. ffmpeg is found the same way as for `mic`.
- `audio` never runs on a peer and is not in `ROUTABLE`. A remote speaker would use the mesh's media streams.
- The real-output test (`real_output_plays_and_ends`) is `#[ignore]`, since CI machines may have no audio device.

### Transports: iroh, Tailscale or both (Decision 2)

Built on 2026-11-23. Before it, `bricolo-mesh` held iroh's `Endpoint` and `Connection` in `mesh.rs`, `client.rs`, `wire.rs` and `media.rs`. Now everything above `crates/mesh/src/transport/` sees only the `Transport` and `Conn` traits, and `[mesh] transports` in `node.toml` picks the backends.

- `Transport` has the two jobs: discovery (`discover` lists `Discovered { id: PeerId, name, online, addrs }`) and connections (`connect(PeerId, key)`, `accept`). `locate(&Peer)` maps a `peers.toml` entry to the transport's `PeerId`, or `None` when the transport has no way to it. `PeerId` is `Iroh { key }` or `Tailscale { host }`, never an IP.
- `Conn` gives boxed reliable streams (`SendHalf`, `RecvHalf`) and unreliable datagrams (`max_datagram_size`, `send_datagram`, `read_datagram`). Close codes and `Closed` are transport-neutral; `Closed::is_refused` replaces the check on iroh's `ConnectionError`.
- Both backends run QUIC through an iroh endpoint keyed by `node.key` (`transport/quic.rs`). The iroh backend uses n0's discovery and relays, or direct `addrs`. The Tailscale backend runs the endpoint bare (`presets::Minimal`, relays off, no address lookup), bound to this device's tailnet IPv4 on `[mesh.tailscale] port` (7787), and dials the peer's tailnet IPs from `tailscale status --json`. Reusing iroh's QUIC keeps one wire format and one handshake: TLS 1.3 with the nodes' raw public keys, which is the per-node keypair authentication Tailscale itself does not give. The same `after_handshake` hook refuses keys outside `peers.toml` on both.
- One identity: both backends authenticate `node.key`, so `peers.toml` lists a peer once. `tailnet = "host[:port]"` names its tailnet machine; without it the Tailscale backend looks for a machine named like the peer.
- The supervisor tries the transports in `transports` order and keeps the first connection that answers the ping. When it drops, the next attempt starts again from the first. Incoming connections arrive on every transport. `bricolo status` shows `transports` and each peer's `via`; `bricolo peer list` shows `connected (tailscale)`.
- iroh binds every interface, the tailnet's included, so `transports = ["iroh"]` already uses a tailnet path when iroh finds it. The Tailscale backend is for devices where iroh's discovery or relays are not wanted, or blocked.
- `Mesh::with_transports` runs the mesh over transports built elsewhere, for a future backend or a test.
- Checked end to end on 2026-11-23 with real binaries: an `bricolo run` node on Linux (aarch64, in the `linux-test` container) sent `speech.transcribe` with a WAV to an `bricolo run` node on macOS whose `[speech]` used the parakeet sidecar, and got the text back. First with `transports = ["iroh"]` (relays off, direct address), then with `transports = ["tailscale"]` on both, changing only `node.toml`. Tailscale was stopped on the Mac, so `[mesh.tailscale] command` pointed at a script printing a `tailscale status`; the dial, handshake and call went over the real network between the two systems.
- Checked again on 2026-11-23 on a private tailnet in Docker (`target/e2e-headscale/`, not checked in): headscale v0.29.6 as the control server and two containers running tailscaled 1.104.7 in TUN mode, joined with a preauth key. WireGuard went direct between them, with no DERP relay. `linux-side` sent `speech.transcribe` to `mac-side` over `transports = ["tailscale"]` and got the stub text back. `mac-side` was a Linux container whose `[speech]` used a stub sidecar, not a Mac.
- That check found a bug. After `tailscale down` and `tailscale up` on `linux-side`, the node never reconnected until `bricolo run` restarted. The endpoint was bound to the tailnet IP, which `tailscale down` removes. iroh failed to rebind it (`AddrNotAvailable`), all its transports failed to receive, and its QUIC driver shut down. The accept loop ended and every connect failed, while `bricolo status` still showed the dead binding. The fix lives in `crates/mesh/src/transport/`. `QuicEndpoint` can now bind again: the trusted keys, the channel of accepted connections and the waiters on `changed` outlive each socket, and a socket counts as dead once its accept loop ends or its endpoint closes. The Tailscale poller wakes when a socket dies, and on each status read it also checks that the bound IP is still one of this device's tailnet IPs. If not, it binds again on the current self IP and keeps the port. While the address is missing it tries again with backoff, from 250 ms up to 10 s. A new bind fires `Transport::changed`, so the supervisors try at once. `bound()` and `bricolo status` show nothing while no socket is live. The iroh backend binds its configured addresses again the same way, which matters when `bind` names one address. `reconnect.sh` now reconnects 2 seconds after `tailscale up`. The tests `a_dead_tailscale_socket_is_bound_again_and_the_link_comes_back` and its iroh twin close the endpoint under the transport through a hidden `kill_socket` hook and check that a speech call works again on the same port.
- `crates/mesh/tests/transports.rs` runs the same speech call over iroh, then Tailscale, then both in each order, with a fixed `tailscale status` that puts every machine on loopback. It also checks streams and datagrams on each backend, a tailnet machine with an untrusted key (`refused`), and discovery.

- Datagrams: `Mesh::send_datagram` and `Mesh::register_datagrams` (`datagram.rs`). A datagram carries its capability name in front of the payload; the receiving node checks the sender's allow list per datagram. Without datagrams on the connection, or for a payload that does not fit, the message goes on its own stream as `Kind::Datagram`, appended to the `Kind` enum so older headers still decode.
- Plugin sharing: `Mesh::share_plugins` and `Mesh::fetch_plugin` (`sharing.rs`) send an `bricolo pack` bundle on a media stream for the capability `plugins`, method = the sha256 of `plugin.wasm`. The first frame is `Result<u64, WireError>`, the length or why there is none. The node implements `PluginStore` from `plugins.lock` (`crates/node/src/share.rs`); `bricolo install peer:<peer>/<sha256>` asks the running node to fetch over the control socket (`fetch-plugin`), checks the hash, and installs with the usual approval. `Source::Peer` records it in `plugins.lock`, and `bricolo install --from-lock` fetches it again.
- `bricolo peer discover` asks the running node (`discover` on the control socket) for `Mesh::discover`.

- Watching the tailnet: the Tailscale backend rereads `tailscale status` every 10 seconds (`TailscaleConfig::poll`) and keeps the last read; connection attempts and discovery reuse a read under 2 seconds old. A read that differs from the one before (a machine joins or leaves, changes address, or comes online) fires `Transport::changed`, and every peer supervisor waiting out its backoff tries again at once. The default `changed` never fires; iroh does its own path discovery.

Not done:

- No capability uses datagrams yet.

### Windows

Written on 2026-11-23 in `crates/providers/src/windows/`, with the portable parts (key tables, modifier masks, the window filter, frame borders, app names and search, the app-event diff) in `crates/providers/src/win.rs`, tested on every system. [windows.md](windows.md) is the user-facing page. Nothing has run on a real Windows machine yet: `just check-windows` runs clippy for `x86_64-pc-windows-gnu` from macOS, and CI's `windows` job runs clippy and the tests on `windows-latest` without `bricolo-ui`.

- Backends use Win32 and WinRT through the `windows` crate. `platform()` in `registry.rs` wires them in; `selection` stays `NoSelection`, and `audio` answers `unavailable`, since ffmpeg has no Windows audio output.
- `desktop::run` runs the node on the calling thread, as on Linux. The backends that need a message loop (`hotkey`, `app.watch`, `system.watch`, SAPI) each run one on a thread of their own.
- The process is per-monitor DPI aware (v2), so coordinates are physical pixels with the primary monitor's corner at 0,0. Frames are DWM's visible bounds; `set-frame` adds the invisible borders back.
- The control socket is a named pipe, `\\.\pipe\bricolo-<FNV-1a of the lowercased data folder>`, with a DACL for the current user only (`crates/node/src/pipe_security.rs`) and remote clients refused. `bricolo-ui`'s `client.rs` computes the same name. `bricolo run` stops on Ctrl+C, Ctrl+Break, the console closing, logoff and shutdown.
- No signals: sidecars and ffmpeg lead their own console process group and get Ctrl+Break. Each sidecar is also in a kill-on-close job object (`windows/process.rs`), killed after the 3-second grace; `taskkill /T /F` is the fallback without one. `free_port` reads `netstat -ano` and tells listeners by their remote address, since the state column is translated.
- `bricolo service` on Windows is a Task Scheduler logon task registered from XML, running `conhost.exe --headless` on a `.cmd` wrapper that sets the `BRICOLO_*` folders, logs to `bricolo-service.log` and restarts after a crash. XML rather than `/SC ONLOGON` avoids the 72-hour limit, the battery rule and the 261-character command limit.
- `shell` children get the system variables Windows programs need (`HOST_ENV` in `shell.rs`) and a `PATH` of the System32 folders. `~/` expands to `%USERPROFILE%` (`disk::home`).
- `notify` registers its own AppUserModelID under HKCU on first use and falls back to Windows PowerShell's. `secrets` uses generic credentials in the Credential Manager. `mic` and `camera` use DirectShow, with the device list read once per node run.

Not done:

- Running on a real machine, and the live check there.
- Building `bricolo-ui` (WebView2), a release build and an installer.
- Hotkey releases come from polling the key every 15 ms. `fn` and `selection` are `unavailable`.

## Research behind these decisions

Checked on 2026-11-16:

- WASI 0.3 was ratified on 2026-06-11 and adds native async, `stream<T>` and `future<T>`. Wasmtime 46 turns it on by default. [Bytecode Alliance](https://bytecodealliance.org/articles/WASI-0.3), [wasi.dev](https://wasi.dev/releases/wasi-p3)
- wRPC is at 0.17 with a draft wire spec. [docs.rs](https://docs.rs/crate/wrpc/latest), [spec](https://docs.rs/crate/wrpc/0.16.0/source/SPEC.md)
- iroh 1.0 is stable, connects by public key with NAT traversal and relay fallback, and has Swift and Kotlin bindings. [iroh blog](https://iroh.computer/blog/v1), [docs.rs](https://docs.rs/crate/iroh/1.0.0)

### Echo cancellation for `mic`

Built on 2026-11-19. A recording used to hear the node's own speech from the speakers. The `tts` queue now notes each WAV it plays, and the time its player started, in `echo::EchoReference` (`crates/providers/src/echo.rs`). At `record-stop`, `FfmpegMic` subtracts the notes that overlap the recording from the file, in place, before it answers.

- The reference is the exact WAV the player gets, resampled to 16 kHz mono and laid on the recording's timeline from the clock times (the recording starts at `stop time - length`). Only the `Http` engine has a file; `say`, `espeak-ng` and `spd-say` play inside the system and are not cancelled.
- Player start-up and the audio buffers add an unknown delay, so a waveform correlation at 4 kHz, over at most 8 s of the reference, finds it between -400 ms and +1500 ms. A score under 0.05 means the microphone did not hear the reference, and the file is left alone.
- A 512-tap NLMS filter (32 ms) removes the echo. It adapts at full step for the first second of reference, then at a smaller step, and stops for 250 ms whenever the microphone is louder than 1.5 times the filter's own echo estimate, so the user's voice over the speaker is not learned as echo. No gate or suppressor follows, so the voice is not clipped.
- It runs once, on samples in memory, on the blocking pool: no delay while the user speaks, and about 0.1 s at stop for a few seconds of audio. A recording with no overlapping reference costs nothing.
- `[mic] echo_cancel = false` in `node.toml` turns it off. The tests in `echo.rs` use synthetic speech: at least 15 dB of echo removed, and the user's voice over the echo kept (correlation above 0.9). Not yet measured with a real speaker and microphone.

### Computer use: accessibility, process, device, scripting, virtual screens

Built on 2026-11-25, after studying Orca's computer use (MIT). Details and the per-system table are in [computer-use.md](computer-use.md).

- Five interfaces join the world: `accessibility`, `process`, `device`, `scripting`, `virtual-screen`; `clipboard` gains `formats`, `get-image`, `set-image`, `get-files` and `set-files`. All five, and the new clipboard methods, are in the mesh table, so the MCP gateway lists them and a peer can call them by name when its `peers.toml` entry allows them. None is in `ROUTABLE`: rules.toml never sends a click or a kill to another device.
- Each engine is portable and holds the policy; a backend trait holds the OS. `accessibility` keeps element indexes per caller and pid, keyed by an identity the backend computes (`CFHash` on macOS, the UIA runtime id, the AT-SPI bus name and path). An index is looked up before the pre-action read, so an agent cannot act on an index it never saw. Every action reads the window again and reports `verified`, `unverified`, `suspected-noop` or `mismatch`.
- `device` and `scripting` run each system's programs through `cmd::Runner`, with a 20 s and a 60 s deadline. The plans for all three systems compile everywhere, and `FakeRunner` checks them on any machine. macOS brightness and locking use private frameworks loaded at run time (`macos/device.rs`).
- `virtual-screen.launch` checks the caller's `[shell]` grant; the gateway's comes from `[mcp] shell`. Screens belong to their caller and end when the plugin stops.
- The router's clipboard fallback forwards the new methods; without that, routed nodes answered `unavailable` for images and files.
- The gateway marks `process.stop`/`kill`, `device.lock`/`sleep`/`set-wifi`, `scripting.run-*` and `virtual-screen.launch`/`destroy` destructive (`DESTRUCTIVE_METHODS` in `crates/mcp/src/catalog.rs`), and the device getters read-only by full id, so `window.focus` stays an action.
