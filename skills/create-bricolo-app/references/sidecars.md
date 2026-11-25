# Sidecars

A sidecar is a local HTTP server that the bricolo node supervises. Use one for what a WASM plugin or a Lua script can't do: Python or Node.js packages, ML models, the GPU, native libraries, threads, or a server that stays loaded between calls. Scripts and plugins call it over HTTP; the node only starts it, watches it and stops it.

## Contents

- [What the node does](#what-the-node-does)
- [The HTTP contract](#the-http-contract)
- [Scaffold and register](#scaffold-and-register)
- [node.toml](#nodetoml)
- [Ports](#ports)
- [Logs and debugging](#logs-and-debugging)
- [Calling a sidecar from a script](#calling-a-sidecar-from-a-script)
- [Calling a sidecar from a plugin](#calling-a-sidecar-from-a-plugin)
- [Speech and TTS sidecars](#speech-and-tts-sidecars)
- [Using a sidecar from another device](#using-a-sidecar-from-another-device)
- [Dependencies and models](#dependencies-and-models)

## What the node does

For each `[sidecars.<name>]` in `node.toml`, one supervisor:

1. Frees the port: it kills (SIGKILL) whatever listens on it, using `lsof`. Never put a sidecar on a port another program needs.
2. Starts the command in its own process group, with the node's environment plus `env`, in `cwd` if set. stdout and stderr go to `<data>/logs/sidecar-<name>.log`.
3. Polls the health URL every 250 ms until it is healthy, then every 5 s. Healthy means status 200 and, when the body is a JSON object with a `ready` key, `"ready": true`.
4. Restarts the process when it exits, after 1 s, doubling to 60 s. One that ran for 60 s restarts after 1 s.
5. On shutdown, after the plugins and scripts stop, sends SIGTERM to the group, then SIGKILL after 3 s.

States in `bricolo status` and `bricolo sidecar list`: `starting`, `ready`, `unhealthy` (was ready, now fails its check, still running), `restarting` and `stopped`. `startup_secs` (default 120) only decides when a slow start shows an error; the node never kills a sidecar that is still loading.

`bricolo sidecar add` and `bricolo sidecar remove` edit `node.toml` and ask a running node to apply the change at once (`bricolo sidecar reload` does the same after a hand edit). New sidecars start, removed ones stop, changed ones restart, the others keep running.

## The HTTP contract

These are the conventions the templates follow. Scripts, plugins and the hub depend only on `/health`; the rest is between your app and its sidecar.

| Rule | Why |
|---|---|
| Listen on `127.0.0.1:<port>`, never `0.0.0.0` | Nothing outside the machine can reach it |
| `GET /health` answers 200 `{"ready": true}`, or `{"ready": false}` while loading | The node shows `starting` until it is ready, and callers can wait for it |
| One endpoint per action: `POST /process`, `POST /ocr`, `POST /rewrite` | Easy to call and to test with `curl` |
| Plain text in and out where you can | Lua has no JSON library; `bricolo.http.request` returns the body as a string |
| JSON (`Content-Type: application/json`) for structured answers | Plugins parse it with `serde_json` |
| Errors: a 4xx or 5xx status, the reason as the body | Callers log it as is |
| Pass files by absolute path, not bytes, when both sides run here | A script or plugin reads and writes files through `disk`; the sidecar opens the path |
| Log to stdout or stderr | It lands in `bricolo sidecar logs <name>` |
| Load models once (`warm_up`), not per request | The first call is fast and `/health` tells the truth |
| Exit on SIGTERM | The node stops it cleanly |
| Read the port from `SIDECAR_PORT` with the scaffolded default | The same code runs on another port |

Long jobs: answer within the caller's timeout (30 s by default, at most 300 s with `timeout_ms`). For longer work, answer `202` with a job id at once and add `GET /jobs/<id>`; the script polls it with a timer.

## Scaffold and register

```bash
bricolo new-sidecar --list
bricolo new-sidecar ocr --template python --port 8810 --dir ~/src
cd ~/src/ocr
python3 -m unittest                       # node --test (node), cargo test (rust)
bricolo sidecar add ocr --command /opt/homebrew/bin/python3 --port 8810 -- ~/src/ocr/server.py
bricolo sidecar list                           # NAME PORT STATE COMMAND; wait for "ready"
curl -s 127.0.0.1:8810/health
curl -s --data 'hello' 127.0.0.1:8810/process
```

Each template has `GET /health`, one `POST /process` that upper-cases text, tests, and a README with its `bricolo sidecar add` line. Python uses `http.server`, Node.js uses `node:http`, Rust uses the standard library; none needs a package. The Rust one needs `cargo build --release` first, and its command is `target/release/<name>`.

`bricolo new-sidecar` prints the `bricolo sidecar add` line with absolute paths. Use absolute paths: the node runs the command from another folder, and as a login service it has a short `PATH` (`~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`, then the system folders).

Other commands:

```bash
bricolo sidecar add <name> --force --command ... --port ...   # replace: new command, port or env
bricolo sidecar add <name> ... --env MODEL=small --env THREADS=4 --cwd ~/src/ocr --startup-secs 600
bricolo sidecar restart <name>                 # after you change its code
bricolo sidecar remove <name>                  # refuses while [speech] or [tts] names it
bricolo sidecar reload                         # after you edit node.toml by hand
bricolo sidecar logs <name> -n 100
```

## node.toml

`bricolo sidecar add` writes this; you can also edit it by hand and run `bricolo sidecar reload`.

```toml
[sidecars.ocr]
command = "/opt/homebrew/bin/python3"     # ~/ expands; a bare name is looked up on the node's PATH
args = ["/Users/me/src/ocr/server.py"]    # optional
cwd = "/Users/me/src/ocr"                 # optional
port = 8810
health_url = "http://127.0.0.1:8810/health"   # optional; this is the default
startup_secs = 120                        # optional
env = { MODEL = "small" }                 # optional
```

## Ports

- Use 8800 to 8899 for your own sidecars. Before you pick one, check `bricolo sidecar list` and `lsof -nP -iTCP:<port> -sTCP:LISTEN`.
- Keep the port in three places in step: the sidecar (`--port` at scaffold time, or `SIDECAR_PORT` in `--env`), `node.toml`, and the caller (the URL in the script, or the plugin's `url` setting and `[http] allow`).
- `127.0.0.1` and `localhost` are different entries in a plugin's allow list. Use `127.0.0.1` everywhere.

## Logs and debugging

| Symptom | Look at |
|---|---|
| `starting` for a long time | `bricolo sidecar logs <name>`: still loading, or `/health` says `"ready": false` |
| `restarting` with an error like `exited (exit status: 1)` | `bricolo sidecar logs <name>`: a crash at start, often a missing package or a wrong path |
| `cannot start <command>: No such file or directory` | The command path; use an absolute path |
| The caller gets `unavailable` | The sidecar is not listening: check `bricolo sidecar list` and the port |
| A plugin gets `denied` | `127.0.0.1:<port>` is missing from `[http] allow`, or it says `localhost` |
| `not loaded` in `bricolo sidecar list` | The node runs an older `node.toml`; run `bricolo sidecar reload` |

Run the server by hand (`python3 server.py`) to see its errors directly; stop the node's copy first with `bricolo sidecar remove`, or use another port.

## Calling a sidecar from a script

Scripts may reach any host, with no allow list.

```lua
local BASE = "http://127.0.0.1:8810"

local function ocr(path)
  local response, err = bricolo.http.request({
    url = BASE .. "/process", method = "POST",
    headers = { ["content-type"] = "text/plain; charset=utf-8" },
    body = path, timeout_ms = 60000,
  })
  if not response then return nil, "cannot reach the sidecar: " .. err end
  if response.status >= 400 then return nil, "sidecar " .. response.status .. ": " .. response.body end
  return response.body
end
```

`bricolo new-script <name> --template sidecar-client --port <n>` writes a script that sends the clipboard on ctrl+alt+cmd+p, shows the answer in a toast, and on load waits for `/health` and logs a sample answer.

## Calling a sidecar from a plugin

```toml
# plugin.toml
imports = ["log", "hotkey", "clipboard", "http", "ui"]

[http]
allow = ["127.0.0.1:8810"]

[settings]
url = "http://127.0.0.1:8810/process"
```

```rust
let request = http::Request {
    method: "POST".into(),
    url: settings.url.clone(),
    headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
    body: text.into_bytes(),
    timeout_ms: Some(30_000),
};
match http::send(request).await {
    Ok(r) if r.status < 400 => { /* String::from_utf8_lossy(&r.body) */ }
    Ok(r) => { /* the sidecar's error: status and body */ }
    Err(e) => { /* Unavailable: the sidecar is down */ }
}
```

`bricolo new <name> --template sidecar-client --port <n>` writes this plugin, with the parsing in `src/domain.rs` and its tests. `bricolo fire <name> process-clipboard` makes one request; the log shows `sidecar answered: ...`. Redirects are not followed and bodies stop at 8 MiB.

## Speech and TTS sidecars

The `speech` and `tts` capabilities can use a sidecar as their engine. Apps keep calling `bricolo.speech.transcribe_file` or `bricolo.tts.speak`, and the mesh can route those calls between devices.

```toml
[sidecars.stt]
command = "/Users/me/stt/.venv/bin/python"
args = ["/Users/me/stt/server.py"]
port = 8765

[speech]
sidecar = "stt"            # or url = "http://127.0.0.1:8765"; not both

[tts]
sidecar = "voice"          # neither sidecar nor url means `say` (macOS) or espeak-ng (Linux)
voice = "alba"
```

- Speech contract: `POST /transcribe` with the absolute path of an audio file as the body. Answer 200 with the text. On failure answer an error status with a body that starts `__ERROR__`.
- TTS contract: `POST /speak` with the text as the body, headers `X-Format: path` and `X-Voice: <voice>` (when a voice is set). Answer 200 with the absolute path of a finished WAV file; the node plays it.
- `[speech]` and `[tts]` are read when the node starts. After changing them, restart the node (`bricolo service stop` then `bricolo service start`, or stop and start `bricolo run`). `bricolo sidecar reload` restarts only the processes.

## Using a sidecar from another device

A sidecar listens on `127.0.0.1`, so only its own machine reaches it.

- For speech and TTS, route the capability: on the device without the model, `rules.toml` says `speech = ["desktop", "local"]`, and the desktop's `peers.toml` allows `speech`. The audio travels over the mesh and the desktop's sidecar transcribes it. docs/mesh.md in the repository has the steps.
- `http` is not routable over the mesh. For other sidecars, run the script or plugin that calls it on the sidecar's machine. Don't open the sidecar to the network.

## Dependencies and models

- Python: keep packages in the sidecar's own environment and point `--command` at it: `uv venv && uv pip install <packages>`, then `--command <dir>/.venv/bin/python`. Or `python3 -m venv .venv`.
- Node.js: `npm install <package>` in the sidecar folder; `node_modules/` next to `server.mjs` is found.
- Rust: `cargo add <crate>`; rebuild with `cargo build --release`, then `bricolo sidecar restart <name>`.
- Big models: load them in `warm_up()` so `/health` reports `"ready": false` meanwhile, and raise `--startup-secs` if loading takes more than two minutes. Tell the user the download size before you fetch a model.
- A sidecar runs with the user's full access, outside any sandbox. Show the user its command and code before registering it, as you would a plugin's capabilities.
