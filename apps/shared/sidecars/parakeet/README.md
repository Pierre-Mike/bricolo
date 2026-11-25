# parakeet

The warm speech-to-text server on `127.0.0.1:8765`, bricolo's `[speech]` sidecar. the dictation plugin (through `speech`), the wake-word sidecar and the pipecat voice agent all post to it. Moved here from the Hammerspoon setup's `parakeet_server.py`; `server.py` is that file unchanged, and its header lists every endpoint and engine.

`dictate/` is the live-dictation commit rule the server imports. It has to stay beside `server.py`, because running the file as a script puts this folder on `sys.path`. Without it, `/health` reports `dictate: false` and live dictation falls back to transcribing on release.

It runs under parakeet-mlx's own python (`~/.local/share/uv/tools/parakeet-mlx/bin/python`), with the model and engine set in node.toml's `env`.

```bash
uv run --no-project --with pytest --with numpy python -m pytest -q -p no:cacheprovider dictate/tests
curl -s 127.0.0.1:8765/health
bricolo sidecar logs parakeet
```
