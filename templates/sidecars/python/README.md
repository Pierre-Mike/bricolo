# __NAME__

A sidecar for bricolo in Python: a local HTTP server that the bricolo node starts, health-checks and restarts. It uses the standard library only.

| Endpoint | In | Out |
|---|---|---|
| `GET /health` | | `{"ready": true}` once `warm_up()` is done, `{"ready": false}` before |
| `POST /process` | text (UTF-8) | text; the example upper-cases it |

Errors answer 4xx or 5xx with the reason as the body.

## Run and test it by hand

```sh
python3 -m unittest                                   # the tests in test_server.py
python3 server.py                                     # listens on 127.0.0.1:__PORT__
curl -s 127.0.0.1:__PORT__/health
curl -s --data 'hello' 127.0.0.1:__PORT__/process     # HELLO
```

## Let the node run it

```sh
bricolo sidecar add __NAME__ --command __PYTHON__ --port __PORT__ -- __DIR__/server.py
bricolo sidecar list                                       # "ready" after a moment
bricolo sidecar logs __NAME__                              # its stdout and stderr
```

A running node starts it at once. After you edit `server.py`, run `bricolo sidecar restart __NAME__`.

## Call it

- From a Lua script: `bricolo.http.request{url = "http://127.0.0.1:__PORT__/process", method = "POST", body = text}`. `bricolo new-script <name> --template sidecar-client --port __PORT__` writes a script that does this on a chord.
- From a plugin: import `http` and allow `127.0.0.1:__PORT__` under `[http]` in plugin.toml. `bricolo new <name> --template sidecar-client --port __PORT__` writes one.

## Dependencies

The template needs none. For packages (a model, an OCR library), give the sidecar its own environment and point `--command` at its Python:

```sh
uv venv && uv pip install -r requirements.txt         # or: python3 -m venv .venv && .venv/bin/pip install ...
bricolo sidecar add __NAME__ --force --command __DIR__/.venv/bin/python --port __PORT__ -- __DIR__/server.py
```

Load the model in `warm_up()`, so `/health` says `"ready": false` while it loads and the node waits. If loading takes more than two minutes, add `--startup-secs 600`.
