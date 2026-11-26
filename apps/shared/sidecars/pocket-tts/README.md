# pocket-tts

The warm pocket-tts synthesis server behind bricolo's `tts` capability. node.toml runs it twice from this one file: `pocket-en` on `127.0.0.1:8791` (voice alba, English) and `pocket-fr` on `127.0.0.1:8793` (voice estelle, French). Moved here from the Hammerspoon setup's `pocket_tts_server.py`; `server.py` is that file unchanged, and its header lists the endpoints.

It runs under the `pocket-tts` uv tool (`uv tool install pocket-tts`), whose Python at `~/.local/share/uv/tools/pocket-tts/bin/python` holds pocket-tts and torch. `POCKET_TTS_PORT`, `POCKET_TTS_VOICE`, `POCKET_TTS_LANGUAGE` and `POCKET_TTS_OUT` come from node.toml's `env`.

```bash
curl -s 127.0.0.1:8791/health
curl -s 127.0.0.1:8791/voices
bricolo sidecar logs pocket-en
```
