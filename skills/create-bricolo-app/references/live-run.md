# Running and checking an app live

## Is a node running?

`bricolo status` prints the running node's state as JSON and exits 0. It exits 2 when no node answers. Then either ask the user to start `bricolo run`, or test in a temporary home that never touches theirs:

```bash
export BRICOLO_CONFIG_DIR="$(mktemp -d)/config" BRICOLO_DATA_DIR="$(dirname "$BRICOLO_CONFIG_DIR")/data"
mkdir -p "$BRICOLO_CONFIG_DIR/scripts"
printf '[mesh]\nenabled = false\n' > "$BRICOLO_CONFIG_DIR/node.toml"   # stay off the network
bricolo run > "$BRICOLO_DATA_DIR.run.log" 2>&1 &
until bricolo status >/dev/null 2>&1; do sleep 0.1; done
```

Every `bricolo` command in that shell then uses the temporary home, `bricolo sidecar add` included: it edits the temporary `node.toml`. Stop the node with `kill %1` when you are done. Hotkeys are global, so a test node's chords are live while it runs.

## Reading `bricolo status`

```json
{
  "plugins": [
    {"name": "clip-tool", "version": "0.1.0", "state": "running", "error": null,
     "restarts": 0, "timers": 0, "hotkeys": {"show-clipboard": "ctrl+alt+shift+c"}}
  ],
  "scripts": [
    {"name": "front-app", "state": "running", "error": null, "restarts": 0, "timers": 0}
  ],
  "sidecars": [
    {"name": "ocr", "state": "ready", "port": 8810, "pid": 4242, "restarts": 0, "error": null}
  ],
  "mesh": {"enabled": false}
}
```

| `state` | Meaning | What to do |
|---|---|---|
| `running` | Loaded and started | Check the logs and fire its bindings |
| `refused` (plugin) | Failed its checks: not in plugins.lock, wrong hash, or a capability not granted | Read `error`; usually `bricolo install . -y` again |
| `off` | Failed twice, or failed in `init`; a script that doesn't compile | Read `error` and the log, fix, then reinstall (plugin) or save (script) |
| `stopped` | Shut down on purpose | |

Sidecar states are `starting`, `ready`, `unhealthy`, `restarting` and `stopped`; `bricolo sidecar list` prints the same as a table. [sidecars.md](sidecars.md#what-the-node-does) explains them.

`error` starting with `reload refused` means the new version failed its checks and the old one still runs. `reload failed` means the new version failed while starting and the old one was started again.

## Logs

```bash
bricolo logs -n 50                          # the end of today's log file
bricolo logs --plugin clip-tool -n 20       # one plugin
bricolo logs --plugin front-app.lua -n 20   # one script: its name plus .lua
bricolo sidecar logs ocr -n 20              # one sidecar's stdout and stderr
```

## Firing a plugin's bindings

`bricolo fire <plugin> <binding-id>` delivers `hotkey(<binding-id>)` to the plugin, as if its chord was pressed. It works even when the chord failed to bind or Accessibility is missing. The answer is `{"fired": ...}`; then read the plugin's log for the line its handler writes.

## Exercising a script

`bricolo fire` reaches plugins only. To run a script's callback once while testing, add a temporary line at the end of the script and save it:

```lua
bricolo.timer.after(500, show)   -- TEST: remove before you finish
```

Read the log for the result, then remove the line and save again.

## Permissions and providers

- `window` on macOS answers `denied` until the app that runs `bricolo run` (the terminal) has the Accessibility permission in System Settings > Privacy & Security.
- `hotkey`, `window`, `app`, `clipboard` and `notify` answer `unavailable` on a system without a desktop session.
- `speech` answers `unavailable` until node.toml names a `[speech]` server. `tts` uses `say` on macOS without one.
- A chord another program holds answers `failed` from `hotkey.bind`; pick another chord.
