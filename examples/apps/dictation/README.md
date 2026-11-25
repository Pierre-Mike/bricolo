# Dictation

Press ctrl+alt+shift+d to start recording and press it again to stop. The node sends the recording to the speech server, copies the text, and pastes it at the cursor with cmd+v (ctrl+v on Linux). A recording stops by itself after 60 seconds.

```bash
bricolo install examples/apps/dictation/dictation.lua
```

It needs:

- a `[speech]` server in `node.toml`, such as `parakeet_server.py` as a sidecar, or `url = "http://127.0.0.1:8765"` when one already runs;
- ffmpeg (`brew install ffmpeg`, `apt install ffmpeg`);
- on macOS, the Microphone and Accessibility permissions for the app that runs `bricolo run`.

The settings are at the top of `dictation.lua`. [docs/apps.md](../../../docs/apps.md#dictation) walks through the app and the `node.toml` setup. `bricolo new <name> --template dictation` gives the same app as a WASM plugin.
