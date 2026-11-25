# Troubleshooting

Find the symptom, run the check, apply the fix. `bricolo status` and `bricolo logs -n 50` are the first two commands in every case.

## The node

| Symptom | Check | Fix |
|---|---|---|
| `bricolo status` prints `{"running": false}` and exits 2 | Is a node running for this `BRICOLO_DATA_DIR`? | Start one: `bricolo service start` (installed app), `bricolo run`, or a test node from [live-run.md](live-run.md) |
| `bricolo: command not found` | `ls ~/Applications/bricolo.app/Contents/Helpers/bricolo.app/Contents/MacOS/bricolo` | Link it into `~/.local/bin`, or in the repository use `cargo build -p bricolo-node` and `./target/debug/bricolo` |
| Changes land in the wrong node | `echo $BRICOLO_CONFIG_DIR $BRICOLO_DATA_DIR` | Every `bricolo` command uses those folders; unset them for the user's own node |
| `bricolo run` refuses node.toml | The error names the section | Fix that section; `[tts]` and `[speech]` name a sidecar or a URL, not both, and the sidecar must exist |

## Scripts

| Symptom | Check | Fix |
|---|---|---|
| `state` is `off` with a syntax error | `bricolo check <file>.lua` | Fix the line it names and save |
| `state` is `off` after errors at run time | `bricolo logs --plugin <name>.lua -n 30` shows the traceback | Two uncaught errors in a row turn a script off; fix and save to start it again |
| `attempt to yield across a C-call boundary`, or a waiting call fails in a module | The call runs at a module's top level | Move it into a function or a callback |
| `attempt to index a nil value (field ...)` on an `bricolo.` call | The call name | Compare with [capabilities.md](capabilities.md); `bricolo check` doesn't catch misspelled calls |
| Nothing happens on the chord | The start-up log line says `cannot bind` | Another app holds the chord, or hotkeys are `unavailable` (no desktop session, Wayland); pick another chord |
| A callback stops with a budget error | It runs more than 500 ms of CPU | Move the work to a sidecar, or split it with `bricolo.timer.after` |

## Plugins

| Symptom | Check | Fix |
|---|---|---|
| `bricolo check .` says an import is not listed | `imports` in plugin.toml | Add the interface, or stop calling it in `src/lib.rs` |
| `bricolo check .` says `[http]` or `[shell]` is missing or empty | plugin.toml | Add the section with a non-empty `allow` |
| `state` is `refused` | `error` in `bricolo status` | Usually a capability not approved: `bricolo install . -y` again |
| `state` is `off` | `bricolo logs --plugin <name>` | `init` failed or the plugin trapped twice; fix, rebuild, `bricolo install . -y` |
| `reload refused` in `error` | The new build failed its checks | The old version still runs; fix what `bricolo check .` reports |
| `bricolo fire` answers "not running" | `bricolo status` | The plugin is `off`, `refused` or `disabled` in the hub |
| `http.send` answers `denied` | `[http] allow` | Add the exact lowercase `host:port`; `127.0.0.1` and `localhost` differ; 80 and 443 are filled in for URLs without a port |
| `shell.run` answers `denied` | `[shell] allow` | List the program's absolute path |
| Build fails with `can't find crate for core` | `rustup target list --installed` | Use rustup's cargo (`export PATH="$HOME/.cargo/bin:$PATH"`); the crate's `rust-toolchain.toml` adds `wasm32-wasip2` |
| `wit_bindgen` cannot find the world | The `path` in `generate!` | A standalone crate uses `wit`; refresh it with `bricolo wit all --out wit` |

## Sidecars

| Symptom | Check | Fix |
|---|---|---|
| `starting` for minutes | `bricolo sidecar logs <name>` | Still loading a model: wait, or raise `--startup-secs`. `/health` must answer 200 and `"ready": true` |
| `restarting`, `error` says `exited (...)` | `bricolo sidecar logs <name>` | It crashed at start: a missing package, a syntax error, or the port in use by something the node couldn't kill |
| `cannot start <command>` | The command path | Use an absolute path (`which python3`); the login service has a short `PATH` |
| `unhealthy` | `curl -s 127.0.0.1:<port>/health` | The server hangs or answers an error; read its log |
| `not loaded` in `bricolo sidecar list` | The node runs an older node.toml | `bricolo sidecar reload` |
| The caller gets `unavailable` | `bricolo sidecar list` | Not running, or a different port in the caller's URL |
| The caller gets `timeout` | How long one request takes | Raise `timeout_ms` (at most 300000), or answer 202 with a job id and poll |
| Another program on the same port was killed | The port | The node frees a sidecar's port before starting it; move the sidecar to a free port with `bricolo sidecar add <name> --force ... --port <n>` |
| Code changes don't show | | `bricolo sidecar restart <name>` (and `cargo build --release` for Rust) |

## Permissions and the platform

| Symptom | Fix |
|---|---|
| `window` or `keyboard` answers `denied` on macOS | System Settings, Privacy & Security, Accessibility: turn on "bricolo node" (or the terminal that runs `bricolo run`) |
| `mic` fails or records silence | Privacy & Security, Microphone, for the same app; check `[mic] device` and that ffmpeg is installed |
| Screen capture through `shell` gives a blank image | Privacy & Security, Screen Recording |
| `hotkey`, `window`, `keyboard` answer `unavailable` on Linux | They need X11; under Wayland they are `unavailable` |
| `speech` answers `unavailable` | `[speech]` in node.toml names no server, or its sidecar is down |
| A toast or caption doesn't show | Panels show only while the hub (`bricolo-ui`) runs; the log still has the line |

## When to stop

After five rounds of fix and check, stop and report: what works, what fails, the last error from `bricolo status`, `bricolo logs` or `bricolo sidecar logs`, and what you would try next.
