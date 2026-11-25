# touch-id

The sidecar behind the password-typer plugin (`../plugin`), on `127.0.0.1:8804`. A WASM plugin can't call LocalAuthentication, so this server opens the macOS Touch ID prompt and answers whether you confirmed. It never sees a password; the plugin keeps those in the Keychain through `secrets`.

| Endpoint | Body | Answer |
|---|---|---|
| `GET /health` | | `{"ready": true}` |
| `POST /authenticate` | the reason, as in `type the password "work"` | `200 ok` once you confirm. `401 cancelled` or `401 not recognized`, `408` when nobody answers in time, `409` while another prompt is open, `503` when Touch ID can't run |

macOS shows the reason after "touch-id is trying to". One prompt is open at a time.

| Variable | Default | Meaning |
|---|---|---|
| `SIDECAR_PORT` | `8804` | The port |
| `TOUCH_ID_POLICY` | `owner` | `owner` accepts Touch ID or your Mac password, so it also works with the lid closed. `biometrics` accepts Touch ID only |
| `TOUCH_ID_TIMEOUT_SECS` | `50` | The sidecar closes the prompt after this many seconds |

## Build and test

```sh
cargo test                      # no prompt: the tests pass a fake in place of Touch ID
cargo build --release
```

## Let the node run it

```sh
bricolo sidecar add touch-id --command /path/to/bricolo/apps/password-typer/sidecar/target/release/touch-id --port 8804
bricolo sidecar list                 # "ready"
curl -s --data 'test the prompt' 127.0.0.1:8804/authenticate   # touch the sensor: "ok"
bricolo sidecar logs touch-id
```

After a rebuild, run `bricolo sidecar restart touch-id`. Add `--force --env TOUCH_ID_POLICY=biometrics` to the `add` line for Touch ID only.
