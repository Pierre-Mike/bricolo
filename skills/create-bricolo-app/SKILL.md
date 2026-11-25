---
name: create-bricolo-app
description: >
  Builds the Bricolo app the user describes, from the idea to
  a running, checked app made of Lua scripts, sandboxed WASM plugins in
  Rust, and sidecars: local HTTP servers in Python, Node.js or Rust that the
  bricolo node starts and restarts, for models, the GPU, native libraries or
  anything WASM can't do. Maps the idea to the bricolo standard library (hotkey,
  clipboard, window, app, notify, timer, kv, disk, shell, http, ui,
  keyboard, mic, audio, speech, tts), picks the parts, scaffolds with bricolo new,
  bricolo new-script and bricolo new-sidecar, tests, installs, verifies with
  bricolo status, bricolo logs, bricolo fire and bricolo sidecar list, and shares with bricolo pack.
  Use when the user wants a bricolo automation, a
  Hammerspoon-style hotkey, clipboard, window, voice, OCR, local LLM or
  HTTP tool, a sidecar for bricolo (a local model or other server), a change to an existing bricolo
  app, or to share one. Works in any agent that can run shell commands.
  Never edits the bricolo node itself.
license: MIT
compatibility: Needs the bricolo command from bricolo (macOS or Linux) and a shell. Plugins need rustup; sidecars need python3, node or cargo.
metadata:
  repository: https://github.com/Pierre-Mike/bricolo
---

# Create a bricolo app

A bricolo node runs apps built from three kinds of parts. Scripts and plugins call the same standard library; sidecars are servers they call over HTTP.

| Part | What it is | Where it lives |
|---|---|---|
| Lua script | One Lua 5.4 file. No build. Trusted: every capability, no prompt. Reloads on save | `~/.config/bricolo/scripts/<name>.lua` |
| WASM plugin | A Rust crate built for `wasm32-wasip2`. Sandboxed: only the capabilities in its `plugin.toml`, approved at install. Shareable through GitHub releases | Any folder; `bricolo install` copies it into the node |
| Sidecar | A local HTTP server on `127.0.0.1:<port>` in any language. The node starts it, checks `GET /health`, restarts it when it dies, and stops it with the node | Any folder; registered in `node.toml` with `bricolo sidecar add` |

`BRICOLO_CONFIG_DIR` moves the config folder, and `scripts/` and `node.toml` with it.

## Before you start

- Run `bricolo --version`. In a bricolo checkout without an installed `bricolo`, build it with `cargo build -p bricolo-node` and use `./target/debug/bricolo`. Put rustup's cargo first: `export PATH="$HOME/.cargo/bin:$PATH"`.
- `bricolo status` exits 2 when no node runs. [references/live-run.md](references/live-run.md) starts one in a temporary home for testing.
- This skill uses plain shell commands and files. Run the commands in a shell, read the reference files when a step points to them, and edit files with whatever your agent uses for that.

## Workflow

### 1. Understand the app

Restate the goal in one sentence: the trigger (a chord such as `ctrl+alt+shift+v`, a timer, a menu-bar button, start-up), what happens, and what the user sees. Then propose a shape in two or three lines instead of asking open questions.

Ask at most three targeted questions, and only when the answer changes the build. Examples: which model or engine a sidecar should run, whether the app will be shared (plugin) or stays on this machine (script), which chord to use when the request names none. For everything else pick a sensible default and say which one you picked.

Search before creating. Run `bricolo status`, `bricolo sidecar list`, and list the scripts folder and `~/.config/bricolo/plugins/`. Extend an app that covers the same domain instead of adding a second one. Two apps can't bind the same chord, and two sidecars can't share a port.

### 2. Map features to capabilities

List each feature and the capability it needs. [references/capabilities.md](references/capabilities.md) starts with a table from "what you want" to capability, followed by every interface's WIT and the matching Lua calls (`bricolo wit <name>` prints the WIT too).

Write down the gaps: features no capability covers. For each gap, pick one:

- **Sidecar**: work that is computation or a library, like OCR, a local LLM, image processing, embeddings, a database, a Python or Node.js package. This is the usual answer.
- **`shell`**: an existing command-line program already does it (`/usr/bin/say`, `ffmpeg`, `tesseract`). Needs the user's agreement, and a plugin needs the program's absolute path in `[shell] allow`.
- **Missing**: the node can't do it. Tell the user which capability is missing and stop. Don't work around it with `shell`, osascript or a sidecar that drives the OS unless the user agrees.

### 3. Choose the parts

| Need | Use |
|---|---|
| A personal automation on this machine, small logic, quickest path | Lua script |
| Share it, install it on other devices, or run it with only the capabilities it declares | WASM plugin |
| Logic that deserves unit tests and types | WASM plugin (or a script with pure helpers) |
| Python or Node.js packages, an ML model, the GPU, native libraries, threads, a long-running server, another language | Sidecar, called by a script or a plugin |
| Speech to text or text to speech with your own engine | Sidecar routed through `[speech]` or `[tts]` in `node.toml`; apps keep calling `speech` and `tts` |

With no signal either way, pick a script. Most apps that need a model are a script (or plugin) plus one sidecar: the script owns the trigger and the UI, the sidecar owns the heavy work. [references/app-design.md](references/app-design.md) decomposes six example apps this way.

### 4. Plan and confirm

Show a short plan before writing code:

```
Goal: <one sentence>
Parts: <script|plugin> <name>, sidecar <name> (<language>, port <n>)
Trigger: <chord or timer>, binding id <kebab-case-id>
Capabilities: <list>; [http] allow 127.0.0.1:<port>; [shell] allow <paths>
Needs on this machine: <permissions, packages, models, disk space>
Checks: <how you will prove it works>
```

When the user is present, wait for an OK. When the request already approves the plan, or nobody can answer, go ahead and say so.

### 5. Build

Build sidecars first, so the script or plugin has something to call.

**Sidecar.** [references/sidecars.md](references/sidecars.md) has the contract and the details.

```bash
bricolo new-sidecar --list                                   # python, node, rust
bricolo new-sidecar <name> --template python --port <n> --dir <parent>
cd <parent>/<name> && python3 -m unittest               # node --test, or cargo test
bricolo sidecar add <name> --command <program> --port <n> -- <script>   # bricolo new-sidecar prints this line
bricolo sidecar list                                         # wait for "ready"
curl -s --data 'hello' 127.0.0.1:<n>/process            # the example endpoint
```

Pick a port from 8800 to 8899 that `bricolo sidecar list` doesn't show and nothing listens on (`lsof -nP -iTCP:<n> -sTCP:LISTEN` prints nothing). The node kills whatever holds a sidecar's port before it starts it. Put the work in `process()` (and model loading in `warm_up()`), keep `GET /health` and keep binding to `127.0.0.1`.

**Script.** [references/scripts-lua.md](references/scripts-lua.md) has the patterns.

```bash
bricolo new-script --list
bricolo new-script <name> --template hotkey-tool               # or sidecar-client --port <n>
bricolo check ~/.config/bricolo/scripts/<name>.lua    # after each edit; syntax only
```

Pure helpers first (no `bricolo` calls), then the `bricolo` calls and bindings. Check the second return value of every waiting call.

**Plugin.** [references/plugin-rust.md](references/plugin-rust.md) has the binding patterns.

```bash
bricolo new --list
bricolo new <name> --template window-tool --dir <parent>     # or sidecar-client --port <n>
cd <parent>/<name>
cargo test                                              # src/domain.rs: pure logic, tests first
cargo build --target wasm32-wasip2 --release
bricolo check .                                              # requested capabilities and actual imports
```

Keep `imports` in `plugin.toml` equal to what `src/lib.rs` uses, and add the allow lists `shell`, `http` and `disk` need. A plugin calling a sidecar needs `http` in `imports` and `127.0.0.1:<port>` under `[http] allow`.

Show the user the capabilities, allow lists, chords and sidecar commands, and get an OK before installing, unless the request already approved them. Then `bricolo install . -y` from the crate folder.

### 6. Verify, then iterate

```bash
bricolo status                               # plugins[] and scripts[] "running"; sidecars[] "ready"
bricolo logs --plugin <name> -n 20           # a plugin; a script is <name>.lua
bricolo fire <plugin> <binding-id>           # plugins only: as if the chord was pressed
bricolo sidecar list                         # state, port, last error
bricolo sidecar logs <name>                  # the sidecar's stdout and stderr
```

Each fired binding must leave the log line you expect. A script must log its start-up line with no error; [references/live-run.md](references/live-run.md#exercising-a-script) shows how to run a script's callback once. A sidecar must be `ready` and answer its endpoint with `curl`. Check the whole chain: the app's log shows the sidecar's answer.

On an error, read [references/troubleshooting.md](references/troubleshooting.md), fix, and go round again. Saving a script reloads it in about 200 ms. `bricolo dev .` rebuilds and reinstalls a plugin on each save. `bricolo sidecar restart <name>` picks up sidecar code changes; `bricolo sidecar add <name> --force ...` changes its command, port or environment. Stop after five rounds and report what still fails.

### 7. Report

Say what you built and where: files, `bricolo check` and test results, the `bricolo status` states, the chords to press, the sidecar's port and how to see its logs. Name what the user must do: grant a permission (Accessibility for `window` and `keyboard`, Microphone for `mic`, on macOS for "bricolo node"), install a package or a model, open the hub (`bricolo-ui`) to see toasts.

### 8. Share

```bash
bricolo pack <plugin-crate-or-scripts-folder> --out dist   # <name>.rhplugin.tar.gz or <folder>.rhscripts.tar.gz
gh release create v0.1.0 dist/*
bricolo install github:<owner>/<repo>@v0.1.0               # on another device; no Rust needed for plugins
bricolo update                                             # newer releases; new capabilities need approval
```

Bundles carry scripts and plugins, not sidecars. Commit the sidecar folder to the same repository with its README; on the other device, clone it and run its `bricolo sidecar add` line. Set `GITHUB_TOKEN` for private repositories.

## Rules

- One app per domain. Extend before you create.
- Never edit the node: `crates/`, `wit/`, or another app unless the request is about it.
- Bind hotkeys so a failure is logged and the app keeps running. A failed `init` turns a plugin off, and then `bricolo fire` can't reach it.
- An app's switches, buttons and status line go in its tile (`set_tile`), which the hub draws in the app's card. Add a widget (`set_widget`) only for values worth a glance, like the time in other cities; most apps have none.
- Plugins: match `on_event` with a `_ => {}` arm. Never block in a handler; capability calls are `async` and awaited.
- Scripts: check `local v, err = bricolo.x.y()`, times are in milliseconds, and a module's top-level code can't make waiting calls.
- Sidecars: bind `127.0.0.1` only, answer `GET /health`, answer errors with a 4xx or 5xx status and the reason as the body, and log to stdout or stderr. Load models once, not per request.
- Keep secrets out of scripts, plugin settings and sidecar code that go into a release. Read them from `bricolo.kv`, `node.toml` settings, or the sidecar's `--env`.
- `denied` from `window` or `keyboard` on macOS is a missing permission for the user to grant, not a bug to work around.
- Driving another app's window (`accessibility`) is the last resort. Prefer `shell`, `disk`, `http`, the app's own CLI, or `scripting`. When you do use it: take a `snapshot`, act on its indexes, read each result's `verification` (`suspected-noop` means don't repeat the click), wait with `wait`, and never submit, send, buy or delete unless the user asked. The rules come from docs/computer-use.md in the bricolo repository.
- `process.stop` and `kill`, `device.lock`, `sleep` and `set-wifi`, and `scripting` change the machine: confirm with the user before an app calls them on its own.

## References

| File | Read it when |
|---|---|
| [references/capabilities.md](references/capabilities.md) | Step 2: mapping features to capabilities; any WIT or Lua call |
| [references/app-design.md](references/app-design.md) | Step 3: worked decompositions of six example apps |
| [references/sidecars.md](references/sidecars.md) | Any sidecar: the HTTP contract, health, ports, logs, `[speech]` and `[tts]`, the mesh |
| [references/scripts-lua.md](references/scripts-lua.md) | Writing a Lua script |
| [references/plugin-rust.md](references/plugin-rust.md) | Writing a Rust plugin |
| [references/live-run.md](references/live-run.md) | No node is running, reading `bricolo status`, exercising a script |
| [references/troubleshooting.md](references/troubleshooting.md) | Something fails |
| [references/agents.md](references/agents.md) | Installing this skill for another agent |
| [examples/](examples/) | Finished Lua scripts to copy from |
