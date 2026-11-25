# noseguard

The daemon keeper behind the noseguard plugin (`../plugin`), on `127.0.0.1:8802`. It runs the NoseGuard camera daemon (`daemon/noseguard.py`, moved here from the Hammerspoon setup's `apps/noseguard`) between `/start` and `/stop`. On each touch it flashes the screen red through `daemon/overlay/overlay` (a window hidden from screen sharing), plays Sosumi twice and counts. The plugin owns the widget and the settings; this server starts with watching off.

| Endpoint | Body | Answer |
|---|---|---|
| `GET /health` | | `{"ready": true}` |
| `GET /status` | | JSON: `watching`, `count`, `camera_id`, `camera_name`, `sensitivity`, `hold`, `debug`, `last_event`, `last_event_at`, `last_error` |
| `POST /start` | JSON `{"camera_id", "camera_name", "sensitivity", "hold", "debug"}`, each optional | the status; restarts the daemon when it runs |
| `POST /stop` | | the status; the camera light goes out |
| `GET /cameras` | | `noseguard.py list`: `[{"id", "name", "builtin", "connected", "index"}]` |
| `POST /reset` | | the status, count 0 |
| `POST /flash` | | the status, after one flash and alarm that don't count |

Sensitivity is 0 to 100 (the tile uses 35, 55 and 75), hold is seconds of contact before an alert. `camera_id` is an AVFoundation uniqueID; empty means the built-in camera.

`server.py` starts `run_daemon.py` with the NG_* settings in its environment. `run_daemon.py` imports `noseguard.py` unchanged and replaces its `fire()`, which ran `open -g hammerspoon://noseguard?event=...`, with a function that writes the event to a pipe the server reads. The daemon's output lands in this sidecar's log. If the server dies, `run_daemon.py` notices it was orphaned and exits within a second, so the camera doesn't stay on.

`NOSEGUARD_DIR` moves the daemon's folder (default `daemon/`), `NOSEGUARD_PYTHON` its python (default `daemon/.venv/bin/python`), `SIDECAR_PORT` the port. node.toml sets `NOSEGUARD_PYTHON` to the venv still in `~/.hammerspoon/apps/noseguard/.venv`, which has the pyobjc Vision and AVFoundation bindings the daemon needs. The server itself needs only the standard library. `daemon/overlay/overlay` is built from `overlay.swift` with `swiftc -O overlay.swift -o overlay`.

```bash
/opt/homebrew/bin/python3 -m unittest   # no camera, flash or sound: every process is a fake
bricolo sidecar add noseguard --command /opt/homebrew/bin/python3 --port 8802 -- /path/to/bricolo/apps/noseguard/sidecar/server.py
curl -s 127.0.0.1:8802/status
bricolo sidecar logs noseguard               # the daemon's lines, and its detail log when debug is on
```

macOS asks for camera access the first time watching starts. Grant it to the app it names, which is the bricolo node, not Hammerspoon.
