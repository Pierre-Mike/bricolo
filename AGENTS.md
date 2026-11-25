# AGENTS.md

bricolo is a personal automation node in Rust. Plugins are WebAssembly components that call a standard library defined in `wit/`. Lua scripts in `scripts/` call the same library through the `bricolo` global ([docs/scripts.md](docs/scripts.md)). Read [docs/design.md](docs/design.md) before changing behaviour; its "Implementation notes" section records what the code does today.

## Domain

@Domain.md

## Commands

Run everything through `just`. The justfile puts rustup's cargo first on `PATH`, because Homebrew's cargo ignores `rust-toolchain.toml`.

| Command | What it does |
|---|---|
| `just build` | Build the node and the `bricolo` binary (debug) |
| `just plugins` | Build every crate in `plugins/` and `fixtures/` for `wasm32-wasip2` |
| `just test` | `just plugins`, then `cargo nextest run --workspace` |
| `just lint` | `cargo fmt --check` and `cargo clippy -D warnings` |
| `just deny` | `cargo deny check`: licenses (no GPL) and advisories |
| `just ci` | `lint`, `test` and `deny`, the same steps CI runs |
| `just check-linux` | `cargo check` for `x86_64-unknown-linux-gnu`; on macOS it needs `zig`, used as C compiler and archiver (`scripts/linux-cc`, `scripts/linux-ar`) for the vendored Lua. It skips `bricolo-ui`, which needs GTK; CI builds that on Ubuntu |
| `just check-windows` | `cargo clippy` for `x86_64-pc-windows-gnu`, without `bricolo-ui`; needs `rustup target add x86_64-pc-windows-gnu`, and off Windows `zig` as C compiler and archiver (`scripts/windows-cc`, `scripts/windows-ar`). It checks that the code compiles and runs nothing; CI's `windows` job tests on `windows-latest` ([docs/windows.md](docs/windows.md)) |
| `just linux-test` | The whole suite on Linux in Docker: clippy and the tests with no display, the X11 tests under Xvfb, and a smoke test of `bricolo run` ([docs/linux.md](docs/linux.md)). `x11`, `smoke` or `shell` runs one part |
| `just ui` | Build `bricolo-ui` (the hub) and `bricolo` in release mode, side by side in `target/release/` |
| `just hub-dev` | Serve the hub front end on http://localhost:8770/dev/hub.html with fixtures and a fake Tauri instead of bricolo-ui and a node, for design work in a browser ([crates/ui/dev/README.md](crates/ui/dev/README.md)) |
| `just hub-shots` | Render `docs/screenshots/hub-*.png` from the dev harness with headless Chrome; the panels' shots come from `scripts/ui-smoke.sh --save` |
| `just bundle-macos` | Build `target/bundle/bricolo.app` (`bricolo-ui`, with `bricolo` in the helper `Contents/Helpers/bricolo.app`), signed with the local "bricolo dev" identity from `scripts/macos-dev-identity.sh`, else ad-hoc ([docs/install.md](docs/install.md)) |
| `just install-macos` | `just bundle-macos`, copy the app to `~/Applications`, and run `bricolo service install --app` on it: the node runs now and at each login |
| `just bundle-linux` | Build `dist/bricolo-<version>-linux-<arch>.tar.gz` (`bricolo`, `install.sh`); on macOS inside the linux-test image |
| `just dev <plugin>` | Rebuild and reinstall `plugins/<plugin>` on each save; a running node reloads it |
| `just install <plugin>` | Build `plugins/<plugin>` and install it, asking you to approve its capabilities |
| `just smoke-templates` | Generate every `bricolo new`, `bricolo new-script` and `bricolo new-sidecar` template outside the repo, test, build, check and install each into a temp home (sidecars through `bricolo sidecar add`), and check that `bricolo run` runs them all, every sidecar is ready, and the sidecar-client plugin and script reach theirs |
| `just skill-refs` | Rewrite `skills/create-bricolo-app/references/capabilities.md` from `wit/` and docs/scripts.md; a test fails while it is out of date |
| `just install-skill` | Link `skills/create-bricolo-app` into `~/.agents/skills` and `~/.claude/skills` (`bricolo skill install --link --source`) |
| `just demo-computer-use` | Claude Code, with only the MCP gateway's tools, edits and saves a TextEdit document through `accessibility`; checks the file on disk (macOS, unlocked; `DEMO_ACTIVATE=1` brings TextEdit forward; costs one agent run) |
| `just demo-computer-use-linux` | The same on Linux in Docker: a zenity dialog on Xvfb, answered through AT-SPI over the HTTP gateway (costs one agent run) |
| `just eval-create-app [--agent claude\|gemini\|codex] "<request>"` | Run a headless coding agent with the skill on a request in a temp Bricolo home, then check that the app runs, and that a requested sidecar is ready and called (costs one agent run) |

With a node running, `bricolo status` prints plugins (with their bound hotkeys) and sidecars, and `bricolo fire <plugin> <id>` delivers `hotkey(<id>)` to a plugin, as if its chord was pressed. `bricolo permissions [--request]` prints (and asks macOS for) the Accessibility and Input Monitoring grants the running node holds. `bricolo-ui` opens the hub for the node that `BRICOLO_DATA_DIR` points at ([docs/ui.md](docs/ui.md)).

Apps: `bricolo new <name> --template <t>`, `bricolo new-script <name> --template <t>` and `bricolo new-sidecar <name> --template python|node|rust` scaffold from `templates/` (`--port` sets a sidecar's port, and the sidecar-client templates' URL); `bricolo install` takes a plugin folder, a `.lua` file, a scripts folder, a bundle from `bricolo pack`, `github:owner/repo[@tag]`, or `peer:<peer>/<sha256>` (a plugin a peer has installed, fetched over the mesh); `bricolo update` and `bricolo install --from-lock` work from `plugins.lock`; `bricolo wit` prints the standard library WIT. See [docs/apps.md](docs/apps.md).

MCP: `bricolo run` hosts the MCP gateway (`[mcp]` in node.toml): one front door to every node in the mesh, as `list_nodes`, `search_capabilities`, `call_capability` and one direct tool per method on each online node, with `notifications/tools/list_changed` when nodes join or leave. `bricolo mcp` pipes stdio to it over the control socket's `mcp-session`; `[mcp] listen` adds Streamable HTTP with a token (`bricolo mcp token`), and `bricolo mcp funnel` publishes it through Tailscale Funnel. `bricolo mcp connect claude-code|claude-desktop` (or the hub's Connect buttons, control command `mcp-connect`) adds it to Claude. Every call is logged with the target `audit`. See [docs/mcp.md](docs/mcp.md).

Computer use: `accessibility` (UI trees and actions: AXUIElement, UI Automation, AT-SPI), `process`, `device`, `scripting` and `virtual-screen`, plus images and files on `clipboard`. The engines live in portable files in `crates/providers/src` behind a backend trait each (`AxBackend`, `ProcessBackend`, `ScreenBackend`, `Runner`), tested with fakes; the gateway marks `process.stop`/`kill`, `device.lock`/`sleep`/`set-wifi`, `scripting.run-*` and `virtual-screen.launch`/`destroy` destructive. See [docs/computer-use.md](docs/computer-use.md).

Sidecars: `bricolo sidecar add <name> --command ... --port N [-- args]`, `list`, `remove`, `reload`, `restart` and `logs` edit `[sidecars.<name>]` in `node.toml` with `toml_edit` (comments stay) and tell a running node through the control socket's `reload-sidecars`, which starts, stops or restarts only what changed.

Skill: `bricolo skill install [--agent claude|codex|gemini|opencode|cursor|agents|all] [--dir D] [--link [--source S]] [--dry-run]`, `bricolo skill path` and `bricolo skill snippet`. `build.rs` embeds `skills/create-bricolo-app` (without `evals/`), so an installed `bricolo` writes it without the repository.

Service: `bricolo service install|uninstall|status|start|stop` runs `bricolo run` at login, as the LaunchAgent `com.pierre-mike.bricolo` on macOS, the systemd user unit `bricolo.service` on Linux, or the Task Scheduler logon task `bricolo` on Windows. `--dry-run` prints the files and commands. See [docs/install.md](docs/install.md).

Tools: `just`, `cargo-nextest`, `cargo-deny` and `lefthook`. Run `lefthook install` once to get the pre-commit hook (fmt and clippy).

## Crate map

| Path | Job |
|---|---|
| `wit/` | The `bricolo:stdlib@0.1.0` package: `call-error`, one interface per capability, and the `plugin` world |
| `crates/node` | The `bricolo` binary: CLI, `bricolo run` (plugins and scripts; the main thread runs the macOS event loop), the control socket (`control.rs`, a named pipe on Windows with `pipe_security.rs`; `subscribe.rs` streams status, UI state and log lines), `state.toml` for what the hub turned off, daily log files, `bricolo new`, `bricolo new-script` and `bricolo new-sidecar` scaffolding (`build.rs` embeds `templates/` and `skills/create-bricolo-app`, and on macOS `Info.plist` in `bricolo`), `bricolo sidecar` (`sidecar.rs`), `bricolo skill` (`skill.rs`), `bricolo install`, `bricolo update`, `bricolo pack`, `bricolo wit`, `bricolo fire`, `bricolo id`, `bricolo peer` (including `bricolo peer discover`), `bricolo call`, `share.rs` (plugins served to and fetched from peers) and `bricolo service` (`service.rs`: LaunchAgent, systemd unit and Task Scheduler XML, built as plans of steps that the tests check without running `launchctl`, `systemctl` or `schtasks`) |
| `crates/runtime` | Wasmtime embedding: manifest, lock and permission checks, start order, one store and event queue per plugin, budgets, restart once, reload. Also installing: sources (`source.rs`), bundles (`bundle.rs`), GitHub releases (`github.rs`), `update.rs` for `bricolo update` and `--from-lock` |
| `crates/router` | Reads `rules.toml` and wraps routable capabilities in an ordered fallback over local and peer providers |
| `crates/providers` | One trait per capability and the local implementations; `sidecar` supervises the sidecars and reloads them from `node.toml`, `tts` holds the speech queue, `mic` records through an ffmpeg child process, `ui` holds the widgets (tiles) and panels the hub draws, `sensors` the plugins using the microphone, camera or screen, and `log::feed` every log line for the hub |
| `crates/providers/src/macos` | macOS backends: main-thread loop, hotkeys (and the Fn key through an event tap, `fn_key.rs`), clipboard, windows (Accessibility), apps, notifications, keyboard (CGEvent) |
| `crates/providers/src/linux` | Linux backends: windows, screens, hotkeys, the app list and the keyboard (XTEST) on X11 (x11rb), the same on Hyprland over its IPC socket (`hyprland/`; `desktop.rs` picks one per call), clipboard (arboard, X11 and Wayland), notifications (D-Bus), desktop files, espeak-ng. See [docs/linux.md](docs/linux.md) |
| `crates/providers/src/windows` | Windows backends on Win32 and WinRT (the `windows` crate): windows and screens, hotkeys (`RegisterHotKey`), apps, keyboard and mouse (`SendInput`), clipboard (arboard), toasts, GDI screenshots, system, Credential Manager, SAPI, DirectShow devices, and `process.rs` (Ctrl+Break and job objects for sidecars and ffmpeg). `crates/providers/src/win.rs` holds their pure parts, tested on every system. See [docs/windows.md](docs/windows.md) |
| `crates/lua` | The Lua 5.4 script tier (mlua): sandbox, the `bricolo` global (one `CAPABILITIES` entry per capability in `src/stdlib.rs`), coroutines for waiting calls, restart once, reload on save |
| `crates/mesh` | `node.key`, `peers.toml`, one supervised connection per peer, the call RPC and media streams. `transport/` holds the `Transport` trait and its iroh and Tailscale backends; nothing outside it touches iroh's connection types. `capabilities.rs` is the table of remote methods. See [docs/mesh.md](docs/mesh.md) |
| `crates/mcp` | The MCP gateway, independent of the node: the standard library WIT as tool schemas (`catalog.rs`, with `wit/` embedded), the allow and deny policy, the server over a `Backend` of nodes, and the stdio and Streamable HTTP transports. `crates/node/src/gateway.rs` is the backend over this node and the mesh; `crates/node/src/mcp.rs` is `bricolo mcp`. See [docs/mcp.md](docs/mcp.md) |
| `crates/ui` | `bricolo-ui`, the Tauri 2 hub in its own process: `client.rs` (control socket, reconnects), `launch.rs` (starts `bricolo run`), `tray.rs`, `panels.rs` (non-activating `NSPanel`s on macOS), and the static front end in `crates/ui/ui/`. See [docs/ui.md](docs/ui.md) |
| `plugins/hello` | The smallest plugin; the runtime's integration tests load it |
| `plugins/window-tiler` | Tiles the focused window with cmd+alt+left, right and up |
| `apps/<app>` | The source of the apps this node runs: `plugin/` (a standalone crate, outside the workspace and `just plugins`), or `script/`, and `sidecar/`; `apps/shared/sidecars/` holds sidecars several apps use. `node.toml` and `plugins.lock` point here. See [apps/README.md](apps/README.md) |
| `fixtures/probe` | Test plugin that spins, traps or reads a file on command |
| `fixtures/caps` | Test plugin that calls clipboard, window, app, notify, tts, hotkey, shell, http, ui, mic and audio on command |
| `templates/<name>` | One `bricolo new` template: `plugin.toml.tmpl`, `src/lib.rs.tmpl` (the adapter) and `src/domain.rs.tmpl` (pure logic and tests) |
| `templates/common` | What every `bricolo new` crate gets: `Cargo.toml`, the standalone workspace, the toolchain file |
| `templates/scripts` | One `bricolo new-script` template per `.lua` file |
| `templates/sidecars/<lang>` | One `bricolo new-sidecar` template per language: `python`, `node`, `rust`. Placeholders `__NAME__`, `__PORT__`, `__DIR__`, `__PYTHON__`, `__NODE__` |
| `examples/apps` | Finished apps to read and install. `dictation/dictation.lua` records on a chord, transcribes and pastes; `live-check/live_check.lua` checks the standard library's real backends on a machine (see Conventions); `crates/lua/tests/scripts.rs` runs both against fakes |
| `skills/create-bricolo-app` | The agent skill (open Agent Skills format) that builds apps from scripts, plugins and sidecars; `references/capabilities.md` is generated; `crates/node/tests/skill.rs` checks the format, the links and that no agent's tool names appear |
| `site/` | The project's GitHub Pages site: static HTML, CSS and JavaScript. `.github/workflows/pages.yml` deploys it with `docs/screenshots/`; `just site` previews it |
| `scripts/smoke-templates.sh`, `scripts/eval-create-app.sh` | The template smoke test and the skill's end-to-end eval |
| `scripts/bundle-macos.sh`, `scripts/install-macos.sh`, `scripts/macos-dev-identity.sh`, `scripts/bundle-linux.sh` | Packaging: the signed app, installing it for this user, the local signing identity, the Linux tarball. `.github/workflows/release.yml` runs them on a `v*` tag |

## Conventions

- Rust 1.99.1, edition 2024, pinned in `rust-toolchain.toml`.
- Library crates return `thiserror` errors. The `node` binary uses `anyhow`.
- Async runs on Tokio. Logging uses `tracing`.
- OS-specific code goes only under `crates/providers/src/<os>/`, behind `#[cfg(target_os = "...")]`, with an `unavailable` stub for the other systems. The workspace must keep compiling for Linux (`just check-linux`) and Windows (`just check-windows`).
- A new capability touches `wit/<name>.wit` and `wit/world.wit`, a trait in `crates/providers`, `CAPABILITIES` (and `ROUTABLE` if it can run on a peer), the host bindings in `crates/runtime/src/host.rs`, the WIT list in `crates/node/src/scaffold.rs` (a test checks it against `wit/`), `CAPABILITIES` in `crates/lua/src/stdlib.rs` so scripts can call it too, and `ENTRIES` in `crates/providers/src/catalog.rs` with the macOS grant it needs, for the hub's Library view (a test checks it against `wit/world.wit`). Anything a provider keeps per plugin is dropped in `Providers::release`. Then run `just skill-refs` to regenerate the skill's cheat-sheet, and add the capability to the table in docs/apps.md. A routable one also needs its entry in `crates/mesh/src/capabilities.rs` (see "Adding a capability to the mesh" in docs/mesh.md).
- The hub's front end (`crates/ui/ui/`) is plain HTML, CSS and ES modules with no build step and no external URLs. It builds DOM nodes with `h()` from `js/dom.js` and sets text with `textContent`, never `innerHTML`, because tiles and panels carry plugin strings.
- `node.toml` sections (`[sidecars.<name>]`, `[tts]`, `[speech]`) and the `plugin.toml` allow lists (`[shell]`, `[http]`) are documented in the phase 2 implementation notes of docs/design.md; `[mesh]` is in docs/mesh.md.
- Tests never run `launchctl`, `systemctl` or `schtasks`: `service.rs` builds plans and the tests carry them out with a fake `Runner` in a temp folder.
- Tests never touch the real home folder. Point `BRICOLO_CONFIG_DIR` and `BRICOLO_DATA_DIR` at a temp folder.
- Install tests never reach GitHub: they serve a fake API from `crates/runtime/tests/support/fake_github.rs` and point `BRICOLO_GITHUB_API` (or `GitHub::new`) at it.
- A template change must keep `just smoke-templates` passing: every template builds outside the repo, passes `bricolo check` and runs.
- Mesh tests never touch the network: they use `MeshConfig::local_only()` (loopback, no relays, no discovery).
- After changing a plugin or fixture, run `just plugins` before the runtime tests. The tests build a missing component, but they do not rebuild a stale one.
- Tests never open the microphone. `mic` tests point `[mic] ffmpeg` (or `FfmpegMic::with_inputs`) at a fake ffmpeg script; the one real recording test is `#[ignore]`.
- Tests only reach fakes, so a desktop backend is not done until it works on a real machine. After adding a capability or porting one to an OS, add its check to `examples/apps/live-check/live_check.lua`, install the script on that OS (`bricolo install examples/apps/live-check/live_check.lua`), focus a window you can spare and press ctrl+alt+cmd+shift+l. The port is done when `bricolo logs` shows no `live_check: FAIL` line; `UNAVAILABLE` marks calls that OS does not have yet.
- Tests never depend on macOS permissions or a GUI session. A test that needs them is `#[ignore]` with a comment saying why; run those with `cargo nextest run --run-ignored all`. Tests that need `python3` or `lsof` skip themselves when the tool is missing.
- Linux tests that need an X server are named `x11_*` and are `#[ignore]`; `just linux-test` runs them one at a time under Xvfb. Tests that expect the desktop capabilities to be `unavailable` skip themselves on Linux when `DISPLAY` or `WAYLAND_DISPLAY` is set.
- Windows tests that need a desktop session are named `win_*`, live in `crates/providers/tests/windows_desktop.rs` and are `#[ignore]`; on a Windows desktop, close Notepad and run `cargo nextest run -p bricolo-providers --test windows_desktop --run-ignored only --test-threads 1`.
