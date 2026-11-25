# wake-word

Say "Codec" with no call open and the voice agent starts one. A port of `apps/wake_word` and `lib/wake_word` from the Hammerspoon setup, run by bricolo on `127.0.0.1:8803` and switched by the wake-word plugin (`../plugin`).

There is no wake-word model. An ffmpeg child tails the microphone into `/tmp/bricolo-wake.raw`; every 1.5 s the last 4 s, when loud enough to be speech, go to the parakeet sidecar's `/transcribe`. A transcript with the word starts a call on the voice agent's daemon (`:8792 POST /start`), and the speech in front of the word follows with `POST /heard`. It stays quiet during a call and for 5.5 s after one, and fires at most once every 8 s.

| Endpoint | Body | Answer |
|---|---|---|
| `GET /health` | | `{"ready": true}` |
| `GET /status` | | `{"enabled", "capturing", "state", "why", "device", "last_heard"}` |
| `POST /enable` | the microphone's avfoundation name or index | the status; opens the microphone |
| `POST /disable` | | the status; closes it |

It starts disabled, so the microphone is closed until the plugin enables it. The microphone is ffmpeg's, outside bricolo's `mic` capability, so `bricolo status` doesn't list it under `active_sensors`; macOS's microphone indicator does show it.

```bash
python3 -m unittest      # nothing opens the microphone: capture, HTTP and the clock are fakes
bricolo sidecar add wake-word --command /opt/homebrew/bin/python3 --port 8803 -- /path/to/bricolo/apps/wake-word/sidecar/server.py
curl -s 127.0.0.1:8803/status
```
