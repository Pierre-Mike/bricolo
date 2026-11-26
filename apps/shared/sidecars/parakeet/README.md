# parakeet

The warm speech-to-text server on `127.0.0.1:8765`, bricolo's `[speech]` sidecar. the dictation plugin (through `speech`), the wake-word sidecar and the pipecat voice agent all post to it. Moved here from the Hammerspoon setup's `parakeet_server.py`; `server.py` is that file with a script block added on top, and its header lists every endpoint and engine.

`dictate/` is the live-dictation commit rule the server imports. It has to stay beside `server.py`, because running the file as a script puts this folder on `sys.path`. Without it, `/health` reports `dictate: false` and live dictation falls back to transcribing on release.

`server.py` names its package (parakeet-mlx) in an inline script block, so node.toml runs it with `uv run --script server.py`: uv builds the venv on the first start and reuses it after. The model and engine come from node.toml's `env`; the mlx-audio engine adds `--with mlx-audio` before `--script`.

```bash
uv run --no-project --with pytest --with numpy python -m pytest -q -p no:cacheprovider dictate/tests
curl -s 127.0.0.1:8765/health
bricolo sidecar logs parakeet
```
