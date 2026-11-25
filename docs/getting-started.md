# Getting started

This page takes you from nothing to your first app on macOS or Linux. You install the node, write a Lua script, then build a WASM plugin. [apps.md](apps.md) has the full reference.

There are no published releases yet, so you build from a checkout of this repository. You need rustup (not Homebrew's Rust) and `just`.

```sh
git clone https://github.com/Pierre-Mike/bricolo.git
cd bricolo
```

## Install the node on macOS

```sh
scripts/macos-dev-identity.sh   # once: a signing identity, so macOS permissions survive rebuilds
just install-macos              # builds the app, copies it to ~/Applications, starts the node
```

The node now runs as the LaunchAgent `com.pierre-mike.bricolo`, and starts again at each login. Put its `bricolo` on your `PATH`:

```sh
mkdir -p ~/.local/bin
ln -sf ~/Applications/bricolo.app/Contents/Helpers/bricolo.app/Contents/MacOS/bricolo ~/.local/bin/bricolo
bricolo status                       # "running": true
```

If `bricolo` is not found, add `~/.local/bin` to `PATH` in your shell profile.

Calls that touch other apps need permissions. Open System Settings, then Privacy & Security, and turn on "bricolo node" under Accessibility for windows and typing, and under Microphone for recording. [install.md](install.md#permissions) has the steps.

## Install the node on Linux

Build the tarball on the Linux machine, or on a Mac with Docker running:

```sh
just bundle-linux               # writes dist/bricolo-<version>-linux-<arch>.tar.gz
```

Copy it to the Linux machine, then:

```sh
tar xzf bricolo-0.1.0-linux-x86_64.tar.gz
cd bricolo-0.1.0-linux-x86_64
./install.sh                    # copies bricolo to ~/.local/bin and starts the systemd user service
bricolo status
```

Hotkeys, windows and typing work on X11. On Wayland those calls answer `unavailable`, and the clipboard, notifications, timers and the rest still work. [linux.md](linux.md) lists what works where.

## Your first Lua script

A script is one `.lua` file in `~/.config/bricolo/scripts/`. The node starts it when the file appears and reloads it each time you save. Save this as `~/.config/bricolo/scripts/front.lua`:

```lua
-- front.lua: on ctrl+alt+cmd+f, show which app is in front.
bricolo.hotkey.bind("ctrl+alt+cmd+f", function()
  local app, err = bricolo.app.frontmost()
  if not app then
    bricolo.log.warn("no frontmost app: " .. tostring(err))
    return
  end
  bricolo.notify.show("Frontmost app", app.name)
  bricolo.log.info("front: " .. app.name)
end)
```

Check it, then press ctrl+alt+cmd+f:

```sh
bricolo check ~/.config/bricolo/scripts/front.lua   # syntax only
bricolo status                                               # scripts[]: "front" is "running"
bricolo logs --plugin front.lua -n 20
```

On Linux, `cmd` is the Super key. For a commented starting point instead of a blank file, run `bricolo new-script <name> --template hotkey-tool`, and `bricolo new-script --list` for the other templates. [scripts.md](scripts.md) lists every `bricolo` call.

## Your first plugin

A plugin is a Rust crate built to WebAssembly. It runs in a sandbox and gets only the capabilities you approve. Work outside the repository:

```sh
bricolo new clip-tool --template hotkey-tool --dir ~/src
cd ~/src/clip-tool
cargo test                                      # the logic in src/domain.rs
cargo build --target wasm32-wasip2 --release    # rustup adds the target from rust-toolchain.toml
bricolo check .                                      # what it imports and asks for
bricolo install . -y                                 # approve its capabilities; the node loads it
bricolo fire clip-tool show-clipboard                # as if you pressed ctrl+alt+shift+c
bricolo logs --plugin clip-tool -n 20                # "shown: Clipboard: <n> characters"
```

Edit `src/domain.rs` for the logic and `src/lib.rs` for the capability calls. `bricolo dev .` rebuilds and reinstalls on every save. To share it, `bricolo pack . --out dist` writes one file that `bricolo install` takes on another machine ([apps.md](apps.md#sharing-through-github-releases)).

## Ask your AI agent

The `create-bricolo-app` skill teaches a coding agent (Claude Code, Codex CLI, Gemini CLI, OpenCode, Cursor and others) to build an app from your idea: Lua scripts, plugins, and sidecars for things like local models. Install it once:

```sh
bricolo skill install --agent all    # writes ~/.agents/skills and ~/.claude/skills
bricolo skill path                   # where it went
```

Start a new agent session, then describe the app. For example:

- "Make a bricolo script that shows a toast with the focused window's title when I press ctrl+alt+shift+w."
- "I want ctrl+alt+shift+o to OCR a part of the screen I select and put the text on my clipboard."
- "Build a bricolo sidecar in Python that rewrites text with a local LLM, and a hotkey that rewrites my selection."

The agent proposes a plan, asks only what changes the build, then writes, installs and checks the app with `bricolo status`, `bricolo logs` and `bricolo sidecar list`. It needs `bricolo` on `PATH`. [apps.md](apps.md#writing-apps-with-a-coding-agent) has the options.

## The hub

The hub, `bricolo-ui`, shows your plugins and scripts with on/off switches, the logs, and the panels apps show, such as toasts. It also adds a menu bar item.

- On macOS it is the app itself: `open ~/Applications/bricolo.app`. `just install-macos --ui` also opens it at login.
- On Linux, building it needs GTK 3 and WebKitGTK 4.1, so the tarball has it only when it was built on a machine with them. On a Linux desktop with those packages, `just ui` builds `target/release/bricolo-ui`.

Toasts and other panels show only while the hub runs. [ui.md](ui.md) covers it.
