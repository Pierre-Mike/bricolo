# pocket-tts

The warm pocket-tts synthesis server behind bricolo's `tts` capability. node.toml runs it twice from this one file: `pocket-en` on `127.0.0.1:8791` (voice alba, English) and `pocket-fr` on `127.0.0.1:8793` (voice estelle, French). Moved here from the Hammerspoon setup's `pocket_tts_server.py`; `server.py` is that file with a script block added on top, and its header lists the endpoints.

`server.py` names its packages in an inline script block (pocket-tts, which brings torch), so node.toml runs it with `uv run --script server.py`: uv builds the venv on the first start and reuses it after. The first start downloads torch, so give the sidecar a long `startup_secs` on a new machine. `POCKET_TTS_PORT`, `POCKET_TTS_VOICE`, `POCKET_TTS_LANGUAGE` and `POCKET_TTS_OUT` come from node.toml's `env`.

```bash
curl -s 127.0.0.1:8791/health
curl -s 127.0.0.1:8791/voices
bricolo sidecar logs pocket-en
```
