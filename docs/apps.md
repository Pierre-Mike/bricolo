# Build your own app

An app is one or more Lua scripts or WASM plugins, plus any sidecars they call. [Domain.md](../Domain.md) defines these terms. Scripts and plugins call the same standard library, so the same automation can start as a script and become a plugin later. A sidecar is a local HTTP server, in any language, that the node runs for work a script or plugin can't do, such as running an ML model. This page covers picking the parts, the templates `bricolo` ships, the capabilities, sidecars, the edit and reload loop, sharing apps through GitHub releases, and the skill that lets a coding agent build all of it.

## Script or plugin

| | Lua script | WASM plugin |
|---|---|---|
| Source | one `.lua` file in `scripts/` | a Rust crate built for `wasm32-wasip2` |
| Build step | none | `cargo build --target wasm32-wasip2 --release` |
| Access | every capability, any file, program and host, no prompt | only the capabilities in `plugin.toml`, approved at install |
| Reload | on save, about 200 ms | on reinstall; `bricolo dev` rebuilds and reinstalls on save |
| Tests | pure helpers with busted, outside the node | `cargo test` on `src/domain.rs`, natively |
| Sharing | `bricolo pack` a folder of scripts | `bricolo pack` the crate; devices need no Rust |

Write a script for a personal automation on your own machine. Write a plugin when the app will run on other devices or for other people, when it should be sandboxed, or when its logic deserves typed code and unit tests. Add a [sidecar](#sidecars) when the app needs Python or Node.js packages, a model, the GPU or a native library.

## Quick start

A script:

```bash
bricolo new-script clip-notify --template hotkey-tool   # writes scripts/clip-notify.lua
bricolo check ~/.config/bricolo/scripts/clip-notify.lua
bricolo status                                          # scripts[]: clip-notify is "running"
bricolo logs --plugin clip-notify.lua
```

A running node starts the new script at once and reloads it each time you save it.

A plugin:

```bash
bricolo new clip-tool --template hotkey-tool --dir ~/src
cd ~/src/clip-tool
cargo test                                         # the domain tests
cargo build --target wasm32-wasip2 --release
bricolo check .                                         # what it imports and asks for, as JSON
bricolo install . -y                                    # approve and install; a running node loads it
bricolo fire clip-tool show-clipboard                   # as if its chord was pressed
bricolo dev .                                           # rebuild and reinstall on every save
```

`bricolo new` writes a standalone crate: its own `[workspace]`, a `rust-toolchain.toml` with the `wasm32-wasip2` target, and a copy of the standard library WIT in `wit/`. It builds anywhere, without this repository. Inside this repository it uses the shared `wit/` and `target/` instead.

## Templates

`bricolo new --list` and `bricolo new-script --list` print these. The templates are compiled into `bricolo`, so an installed `bricolo` has them all.

| Template | Capabilities | What it does | Plugin chord | Script chord |
|---|---|---|---|---|
| `plugin` / `starter` | log, kv (the script adds hotkey, clipboard, tts, timer) | Counts its starts; the script is a commented tour | none | cmd+alt+s |
| `hotkey-tool` | hotkey, clipboard, notify | Shows the clipboard text in a notification | ctrl+alt+shift+c | ctrl+alt+cmd+c |
| `voice` | hotkey, disk, speech, tts | Transcribes the newest recording in ~/Recordings and says how many words it heard | ctrl+alt+shift+t, x to stop | ctrl+alt+cmd+t, x to stop |
| `window-tool` | hotkey, window | Centers the focused window at two thirds of its screen | ctrl+alt+shift+m | ctrl+alt+cmd+m |
| `http-service` | timer, http, kv | Checks a URL every minute and logs when it goes up or down | none | none |
| `dictation` (plugin only) | hotkey, timer, mic, speech, clipboard, keyboard, ui | Records on a chord, transcribes on the next press and pastes the text. The script version is [examples/apps/dictation](../examples/apps/dictation/dictation.lua) | ctrl+alt+cmd+d | ctrl+alt+shift+d |
| `sidecar-client` | hotkey, clipboard, http, ui | Sends the clipboard text to a sidecar's `POST /process` and shows the answer in a toast. `--port` sets the sidecar's port (default 8800) in the URL and the plugin's `[http] allow`. The script also waits for the sidecar's `/health` on load and logs a sample answer | ctrl+alt+shift+p | ctrl+alt+cmd+p |

The plugin and script chords differ so both versions of a template can run side by side.

Each plugin template has the same two files:

- `src/domain.rs` holds the logic as plain functions and types, with `#[cfg(test)]` tests. It makes no capability calls, so `cargo test` runs it natively.
- `src/lib.rs` is the adapter. It generates the bindings, calls the capabilities, converts WIT records to domain types, and passes the results to the domain.

Plugin settings live under `[settings]` in `plugin.toml` and reach `init` as JSON. Override them per device in `node.toml` under `[plugins.<name>]`, by hand or in the app's Settings tab in the hub ([ui.md](ui.md#settings)). The templates parse them once in `init` into a `domain::Settings`. `[settings_ui.<key>]` gives the hub a label, help text, a list of choices, `advanced = true` or `hidden = true` for one setting.

Every template binds its chords so that a failure is logged and the app keeps running. A plugin whose `init` fails is turned off, and `bricolo fire` can no longer reach it.

## Capabilities

| Capability | What it does | A plugin also needs | Where it works | Can run on a peer |
|---|---|---|---|---|
| `log` | Lines in the node's daily log | | everywhere | |
| `timer` | `after`, `every`, `cancel`, `sleep` | | everywhere | |
| `kv` | A small key/value store per app, kept across reloads | | everywhere | |
| `disk` | Files outside the app's data folder | `permissions = ["disk:~/Notes"]` or `disk:read:` | everywhere | yes |
| `tts` | Queued text to speech | | `say` on macOS, espeak-ng on Linux, SAPI on Windows, or a TTS server | yes |
| `speech` | Speech to text | | a `[speech]` server in `node.toml` | yes |
| `hotkey` | Global chords; releases on macOS and Windows, the Fn key on macOS | | macOS, Linux on X11, Windows | |
| `clipboard` | Text, images (saved as PNG in the app's data folder) and copied files | A `disk:` grant for images and files outside the app's data folder | macOS, Linux (X11 and Wayland), Windows | yes |
| `window` | Windows and screens: list, focus, move, resize, close, minimize | | macOS (Accessibility permission), Linux on X11, Windows | |
| `app` | Launch, open a URL, running apps, the frontmost app; quit and hide on macOS, Linux X11 and Windows; `app.watch` delivers `app` events when an app launches, quits or comes to the front (macOS and Windows) | | macOS, Linux, Windows | |
| `notify` | Notifications | | macOS, Linux over D-Bus, Windows toasts | yes |
| `shell` | Run a listed program, with no shell | `[shell] allow = ["/usr/bin/say"]` | everywhere | |
| `http` | HTTP requests to listed hosts | `[http] allow = ["api.example.com:443"]` | everywhere | |
| `ui` | Controls in the app's hub card (a tile), a display-only widget, and caption, choice list, toast and review panels; what the user does arrives as the `ui` event | | where `bricolo-ui` runs: macOS, Linux ([ui.md](ui.md)) | |
| `keyboard` | Press a shortcut, type text into the focused app | | macOS (Accessibility permission), Linux on X11, Windows (not into apps run as administrator) | |
| `mouse` | Read the pointer position, move it, click and scroll | | macOS (Accessibility permission), Linux on X11, Windows | |
| `mic` | Record the microphone to a WAV file in the app's data folder | | ffmpeg on macOS (Microphone permission), Linux (PulseAudio or ALSA) and Windows (DirectShow) | |
| `system` | Idle time, battery and power source, keeping the device awake, and `system` events: sleep, wake, lock, unlock, screens changed | | macOS (needs `bricolo run`'s main-thread loop); Linux (power everywhere, the rest through X11 and logind); Windows | |
| `files` | Watch a folder under a `disk:` grant; each change arrives as a `file` event | A `disk:` grant for the folder | everywhere (FSEvents on macOS, inotify on Linux, ReadDirectoryChangesW on Windows) | |
| `screen` | A PNG of the screen or an area, saved in the app's data folder | | macOS (Screen Recording permission); Linux on X11, or Wayland with `grim`; Windows (GDI) | |
| `selection` | The text selected in the frontmost app | | macOS (Accessibility permission), Linux (the PRIMARY selection); not Windows | |
| `audio` | Play sound files on the default output, looped or once, and change their volume while they play | A `disk:` grant for files outside the app's data folder | macOS (AVAudioPlayer), Linux (ffmpeg to PulseAudio or ALSA); not Windows | |
| `camera` | Take one photo with the default camera, saved as a JPEG in the app's data folder | | ffmpeg on macOS (Camera permission), Linux (V4L2) and Windows (DirectShow) | |
| `secrets` | Keep small secrets such as API keys in the Keychain, Secret Service or Credential Manager, one set per app | | macOS (Keychain), in a node started by `bricolo run`; Linux (Secret Service: GNOME Keyring, KWallet, KeePassXC); Windows (Credential Manager) | |
| `claude-code` | Start Claude Code sessions on a task in a folder, follow their state and last result, send them more work, stop them | | where the `claude` CLI is installed and `[claude-code] dirs` in `node.toml` lists the folders | yes |
| `accessibility` | Read an app window's UI tree with numbered elements, find elements, click, set values, type, press keys, pick menu items and wait for text, in the background where the system allows; each action reads the result back ([computer-use.md](computer-use.md)) | | macOS (Accessibility permission; screenshots need Screen Recording), Windows (UI Automation), Linux (AT-SPI) | by name, when the peer allows it |
| `process` | List processes with CPU and memory, stop or kill one; the node never stops itself or its parent | | everywhere | by name, when the peer allows it |
| `device` | Volume and mute, brightness, Wi-Fi, dark mode, Focus or Do Not Disturb, lock, sleep | | macOS, Linux (PipeWire or PulseAudio, brightnessctl, NetworkManager, GNOME settings, logind), Windows (no Focus) | by name, when the peer allows it |
| `scripting` | AppleScript, JavaScript for Automation and Shortcuts; PowerShell | | macOS (each app a script drives asks for Automation once); PowerShell on Windows, or `pwsh` anywhere | by name, when the peer allows it |
| `virtual-screen` | Screens no monitor shows: start one, launch a program on it, capture it, click and type on it | `[shell] allow` for each program it launches | Linux (Xvfb); Windows Server (a Remote Desktop session, with an account stored as secrets); other Windows (a hidden desktop; keys without modifiers); not macOS yet | by name, when the peer allows it |

"By name" means a peer, or the MCP gateway on a peer, calls that node's capability directly; rules.toml never sends these elsewhere on its own, because they act on one device's screen and programs.

Events reach `on-event` next to timers and hotkeys: `app` (after `app.watch(true)`), `system` (after `system.watch(true)`) and `file` (from `files.watch`, carrying the watch id). Match with a fallback arm, because later versions add kinds.

`bricolo wit` lists the interfaces, `bricolo wit <name>` prints one, `bricolo wit all` prints the whole `bricolo:stdlib` package as one file, and `bricolo wit all --out wit` writes it into a folder. [scripts.md](scripts.md) has every Lua call. The skill's [capability cheat-sheet](../skills/create-bricolo-app/references/capabilities.md) starts with a table from "what you want" to the capability, then puts the WIT and the Lua calls side by side.

## Sidecars

A sidecar is a local HTTP server that the node starts, health-checks, restarts when it dies, and stops with the node. Scripts and plugins call it through `http`. Use one for what WASM and Lua can't do: Python or Node.js packages, ML models, the GPU, native libraries, threads, or anything that should stay loaded between calls.

```bash
bricolo new-sidecar --list                                     # python, node, rust
bricolo new-sidecar rev --template python --port 8810 --dir ~/src
cd ~/src/rev && python3 -m unittest
bricolo sidecar add rev --command /opt/homebrew/bin/python3 --port 8810 -- ~/src/rev/server.py
bricolo sidecar list                                           # rev  8810  ready  ...
curl -s --data 'hello' 127.0.0.1:8810/process             # HELLO
bricolo new-script rev-clip --template sidecar-client --port 8810
```

`bricolo new-sidecar` writes a server bound to `127.0.0.1` with `GET /health` (`{"ready": true}`, or `false` while it loads) and an example `POST /process` that upper-cases text, its tests, and a README. Python uses `http.server`, Node.js `node:http`, Rust its standard library; none needs a package. It prints the `bricolo sidecar add` line with absolute paths, because the node runs the command from another folder and, as a login service, with a short `PATH`.

| Command | What it does |
|---|---|
| `bricolo sidecar add <name> --command <program> --port <n> [--args ...] [-- args] [--cwd] [--env K=V] [--health-url] [--startup-secs] [--force]` | Adds `[sidecars.<name>]` to `node.toml`, or replaces it with `--force`. Keeps the file's comments. Refuses a port another sidecar uses |
| `bricolo sidecar remove <name>` | Removes it; refuses while `[speech]` or `[tts]` names it |
| `bricolo sidecar list` | Each sidecar's port, state and command; the last error if any |
| `bricolo sidecar reload` | Makes a running node reread `node.toml` after a hand edit |
| `bricolo sidecar restart <name>` | Stops and starts it, after a code change |
| `bricolo sidecar logs <name> [-n N]` | Its stdout and stderr, from `<data>/logs/sidecar-<name>.log` |

`add` and `remove` tell a running node at once: new sidecars start, removed ones stop, changed ones restart, and the rest keep running. `[speech]` and `[tts]` are read when the node starts, so a change there needs a node restart.

The conventions a sidecar should follow:

- Listen on `127.0.0.1` only, on a port from 8800 to 8899 that nothing else uses. The node kills whatever holds a sidecar's port before starting it.
- Answer `GET /health` with 200, and `{"ready": false}` while a model loads. The node shows `starting` until it is ready; `startup_secs` (default 120) only sets when a slow start shows an error.
- One `POST` endpoint per action. Plain text in and out suits Lua, which has no JSON library; plugins can parse JSON with `serde_json`.
- Errors are a 4xx or 5xx status with the reason as the body. Logs go to stdout or stderr.
- A plugin needs `http` in `imports` and the exact `127.0.0.1:<port>` under `[http] allow`; `localhost` is a different entry. Scripts reach any host.

A speech or TTS server can be a sidecar too: name it in `[speech] sidecar` or `[tts] sidecar`, and apps keep calling `speech` and `tts`, which the mesh can route to another device. The contracts are in [Set up speech](#set-up-speech) and in the skill's [sidecars.md](../skills/create-bricolo-app/references/sidecars.md#speech-and-tts-sidecars). `http` itself is not routable, so a script or plugin that calls a sidecar runs on the sidecar's machine.

`bricolo pack` bundles scripts and plugins, not sidecars. Keep a sidecar's folder in the app's repository with its README; on another device, clone it and run its `bricolo sidecar add` line.

## Dictation

Dictation is the worked example of an app built from several capabilities: press a chord to start recording, press it again to stop, and the text appears at the cursor. It ports the core of `apps/dictation` from the Hammerspoon setup. It comes in two forms that do the same thing:

- [examples/apps/dictation/dictation.lua](../examples/apps/dictation/dictation.lua), a script. The chord is ctrl+alt+shift+d. Change `CHORD`, `MAX_SECS` and `PASTE` at the top.
- `bricolo new <name> --template dictation`, a plugin. The chord is ctrl+alt+cmd+d. `src/domain.rs` is the state machine (idle, recording, transcribing) with its tests, and `src/lib.rs` calls the capabilities. Settings: `chord`, `max_secs` and `paste` (`"cmd+v"`; set `"ctrl+v"` on Linux, or `""` to leave the text on the clipboard).

One take goes like this:

1. The chord calls `mic.record-start`. ffmpeg records 16 kHz mono WAV into the app's data folder, and `bricolo status` lists the app under `active_sensors`, so the hub and the menu bar item show the recording dot. The script also shows a caption until the take is done.
2. The chord again, or the safety stop after 60 seconds, calls `mic.record-stop`, which returns the file's path.
3. `speech.transcribe-file` sends the path to the speech server. A plugin needs no `disk:` grant for it, because the file is in its own data folder.
4. `clipboard.set` copies the text and `keyboard.keystroke` presses cmd+v (ctrl+v on Linux). If the keystroke fails, the text stays on the clipboard and a toast says so.

Errors at any step show a toast and go back to idle. Panels need `bricolo-ui` running ([ui.md](ui.md)); without it they wait in the node, and the errors are still in `bricolo logs`.

### Set up speech

`speech` needs a server with parakeet's HTTP contract. The Hammerspoon setup's `parakeet_server.py` is one. Run it as a sidecar, which the node starts, watches and stops:

```toml
# ~/.config/bricolo/node.toml
[sidecars.parakeet]
command = "~/.local/share/uv/tools/parakeet-mlx/bin/python"
args = ["~/GitHub/hammerspoon/parakeet_server.py"]
port = 8765
startup_secs = 180
env = { STT_ENGINE = "parakeet", STT_MODEL_ID = "mlx-community/parakeet-tdt-0.6b-v3", STT_MODEL = "/Users/you/.cache/huggingface/hub/models--mlx-community--parakeet-tdt-0.6b-v3/snapshots/<snapshot>", STT_PORT = "8765", HF_HUB_OFFLINE = "1" }

[speech]
sidecar = "parakeet"

[mic]
device = "MacBook Pro Microphone"   # optional; ffmpeg's default input otherwise
```

`STT_MODEL` is the snapshot folder of the model in the Hugging Face cache, and `env` values are passed as they are, so write the full path there instead of `~`. The interpreter is the one `lib/stt_server.lua` uses for parakeet models, from `uv tool install parakeet-mlx`.

A starting sidecar first kills whatever listens on its port. If Hammerspoon already runs the same server on port 8765, point `[speech]` at it instead and leave out `[sidecars.parakeet]`:

```toml
[speech]
url = "http://127.0.0.1:8765"
```

`curl -s localhost:8765/health` shows whether a server is up and its model is loaded (`"ready": true`).

### Install and try it

```bash
bricolo install examples/apps/dictation/dictation.lua -y
bricolo status        # scripts[]: dictation is "running"
```

On macOS, whatever runs `bricolo run` needs the Microphone permission for ffmpeg and the Accessibility permission for the paste: "bricolo node" when the node runs as the installed service ([install.md](install.md#permissions)), or your terminal when you start `bricolo run` yourself. Without Accessibility the text still reaches the clipboard.

To check the speech server without speaking, transcribe a file that `say` wrote:

```bash
say -o /tmp/hello.aiff "hello from bricolo"
ffmpeg -y -i /tmp/hello.aiff -ar 16000 -ac 1 /tmp/hello.wav
bytes=$(python3 -c 'print(list(open("/tmp/hello.wav", "rb").read()))')
bricolo call speech transcribe "[$bytes, \"wav\"]"   # {"ok": "Hello from Bricolo."}
```

## Checking that an app runs

```bash
bricolo status                          # plugins[] and scripts[], each with "state": "running"
bricolo logs --plugin clip-tool -n 20   # a plugin's lines; add .lua for a script
bricolo fire clip-tool show-clipboard   # plugins only: delivers hotkey(<id>)
bricolo sidecar list                    # each sidecar's state: "ready" once /health answers
```

A plugin's status lists its bound chords under `hotkeys`. `refused` means it failed its checks (the reason is in `error`), and `off` means it failed twice or failed in `init`. See "Lifecycle" in [design.md](design.md#lifecycle) and [scripts.md](scripts.md#lifecycle).

To try an app without touching your own setup, point `BRICOLO_CONFIG_DIR` and `BRICOLO_DATA_DIR` at a temporary folder, put `[mesh] enabled = false` in its `node.toml`, and start `bricolo run` there. Every `bricolo` command in that shell then talks to that node.

## Sharing through GitHub releases

`bricolo pack` turns an app into one file to attach to a release:

| Folder | Bundle | Holds |
|---|---|---|
| a plugin crate (has `plugin.toml`) | `<name>.rhplugin.tar.gz` | `plugin.toml` (without its `wasm` key) and `plugin.wasm` |
| a folder of `.lua` files | `<folder>.rhscripts.tar.gz` | the `.lua` files at the root, which run, and modules in subfolders |

```bash
bricolo pack . --out dist
gh release create v0.1.0 dist/clip-tool.rhplugin.tar.gz
```

The bundles are gzip-compressed tar files with sorted names, mtime 0 and mode 0644, so packing the same files twice gives the same bytes. One release may carry several bundles. A release with plain `plugin.toml` and `plugin.wasm` assets also installs, as one plugin.

On any device:

```bash
bricolo install github:me/clip-tool             # the latest release
bricolo install github:me/clip-tool@v0.1.0      # that tag
bricolo update                                  # newer releases of everything installed from GitHub
bricolo update clip-tool -y                     # one app, approving new capabilities
bricolo install --from-lock                     # a new device: everything in plugins.lock
bricolo install peer:mac/<sha256>               # the plugin the peer "mac" has installed with that hash
```

- `bricolo install` also takes a built plugin folder, a `.lua` file, a folder of scripts, or a bundle file.
- `peer:<peer>/<sha256>` fetches a plugin another node has installed, over the mesh, by the sha256 of its `plugin.wasm` (the `sha256` in that node's `plugins.lock`). It needs a running node, and the other node must list `plugins` in this node's `allow` in its `peers.toml`. The hash is checked before the usual approval. See "Plugin sharing" in [mesh.md](mesh.md).
- It shows what each plugin asks for and asks you to approve, unless `-y`. Scripts are trusted, so for scripts it prints a warning that they run with full access, lists the files, and asks before copying them into `scripts/`.
- `GITHUB_TOKEN` is sent to the GitHub API for private repositories and a higher rate limit. `BRICOLO_GITHUB_API` replaces `https://api.github.com`; the tests use it to serve a fake API.
- Every install records its source, version and sha256 in `plugins.lock`. A plugin's source is `github:<owner>/<repo>@<tag>`, `peer:<peer>/<sha256>` or `path:<folder or bundle>`, and its hash is the hash of `plugin.wasm`, which the node checks at every start. Scripts go under `[scripts.<name>]` with the list of files written.
- `bricolo update` compares the locked tag with the latest release. For a plugin it prints the capabilities added and removed. A new version that asks for a capability you have not granted is installed, but the node refuses to start it until you approve with `bricolo update <name> -y`; a running node keeps the old version running until then.
- `bricolo install --from-lock` downloads each entry at its locked tag, or fetches it again from its peer, and refuses one whose hash differs from the lock. Entries already installed with the locked hash are skipped.
- `bricolo uninstall <name>` removes a plugin, or the files a script bundle installed.

## Writing apps with a coding agent

The `create-bricolo-app` skill in [skills/create-bricolo-app](../skills/create-bricolo-app/SKILL.md) teaches a coding agent the whole loop, from an app idea to a running app: restate the goal and propose a shape, map features to capabilities and name the gaps, pick scripts, plugins and sidecars, show a short plan, then scaffold, test, install, check with `bricolo status`, `bricolo fire`, `bricolo logs` and `bricolo sidecar list`, and share with `bricolo pack`. It follows the open Agent Skills format and names no agent's tools, so Claude Code, Codex CLI, Gemini CLI, OpenCode, Cursor and others can use it.

The skill is compiled into `bricolo`:

```bash
bricolo skill install                  # ~/.agents/skills and ~/.claude/skills: reaches all five agents above
bricolo skill install --agent gemini   # also an agent's own folder: claude, codex, gemini, opencode, cursor, agents
bricolo skill install --dir .agents/skills   # a project's folder
bricolo skill install --dry-run        # print the folders only
bricolo skill path                     # where it is installed
bricolo skill snippet                  # a paragraph for AGENTS.md or GEMINI.md, for agents without a skills folder
```

It replaces an older copy of the skill and refuses any other folder in its place. In this repository, `just install-skill` links both folders to `skills/create-bricolo-app`, so edits show at once. [agents.md](../skills/create-bricolo-app/references/agents.md) lists where each agent reads skills, with sources.

`scripts/eval-create-app.sh [--agent claude|gemini|codex] "<request>"` runs a headless agent with the skill on a one-line request, in a temporary Bricolo home, and checks that exactly one new app is running and binds the chord the request names. When the request asks for a sidecar, it also checks that one is ready and that the app calls its port. `evals/evals.json` holds plan-level cases and the requests used end to end.
