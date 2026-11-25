# __NAME__

A sidecar for bricolo in Rust: a local HTTP server that the bricolo node starts, health-checks and restarts. It uses the standard library only.

| Endpoint | In | Out |
|---|---|---|
| `GET /health` | | `{"ready": true}` once `warm_up` is done, `{"ready": false}` before |
| `POST /process` | text (UTF-8) | text; the example upper-cases it |

Errors answer 4xx or 5xx with the reason as the body.

## Build, run and test it by hand

```sh
cargo test
cargo build --release
./target/release/__NAME__                             # listens on 127.0.0.1:__PORT__
curl -s 127.0.0.1:__PORT__/health
curl -s --data 'hello' 127.0.0.1:__PORT__/process     # HELLO
```

## Let the node run it

```sh
bricolo sidecar add __NAME__ --command __DIR__/target/release/__NAME__ --port __PORT__
bricolo sidecar list                                       # "ready" after a moment
bricolo sidecar logs __NAME__                              # its stdout and stderr
```

A running node starts it at once. After a rebuild, run `bricolo sidecar restart __NAME__`.

## Call it

- From a Lua script: `bricolo.http.request{url = "http://127.0.0.1:__PORT__/process", method = "POST", body = text}`. `bricolo new-script <name> --template sidecar-client --port __PORT__` writes a script that does this on a chord.
- From a plugin: import `http` and allow `127.0.0.1:__PORT__` under `[http]` in plugin.toml. `bricolo new <name> --template sidecar-client --port __PORT__` writes one.

## Dependencies

The template has none. This is the place for what a WASM plugin cannot do: native libraries, the GPU, threads, long-lived state. Add crates with `cargo add`, load models in `warm_up`, and keep the HTTP contract above so scripts and plugins can call it.
