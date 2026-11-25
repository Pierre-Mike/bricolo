# __NAME__

A sidecar for bricolo in Node.js: a local HTTP server that the bricolo node starts, health-checks and restarts. It uses built-in modules only, so there is nothing to install. It needs Node.js 18 or later.

| Endpoint | In | Out |
|---|---|---|
| `GET /health` | | `{"ready": true}` once `warmUp()` is done, `{"ready": false}` before |
| `POST /process` | text (UTF-8) | text; the example upper-cases it |

Errors answer 4xx or 5xx with the reason as the body.

## Run and test it by hand

```sh
node --test                                           # the tests in server.test.mjs
node server.mjs                                       # listens on 127.0.0.1:__PORT__
curl -s 127.0.0.1:__PORT__/health
curl -s --data 'hello' 127.0.0.1:__PORT__/process     # HELLO
```

## Let the node run it

```sh
bricolo sidecar add __NAME__ --command __NODE__ --port __PORT__ -- __DIR__/server.mjs
bricolo sidecar list                                       # "ready" after a moment
bricolo sidecar logs __NAME__                              # its stdout and stderr
```

A running node starts it at once. After you edit `server.mjs`, run `bricolo sidecar restart __NAME__`.

## Call it

- From a Lua script: `bricolo.http.request{url = "http://127.0.0.1:__PORT__/process", method = "POST", body = text}`. `bricolo new-script <name> --template sidecar-client --port __PORT__` writes a script that does this on a chord.
- From a plugin: import `http` and allow `127.0.0.1:__PORT__` under `[http]` in plugin.toml. `bricolo new <name> --template sidecar-client --port __PORT__` writes one.

## Dependencies

For npm packages, run `npm install <package>` in this folder and import them in `server.mjs`. The node starts the sidecar with `--command` and the script path, so `node_modules/` next to `server.mjs` is found. Load models in `warmUp()`.
