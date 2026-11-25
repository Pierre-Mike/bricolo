# App design: six worked decompositions

Each example goes through steps 1 to 4 of the workflow: the goal in one sentence, features mapped to capabilities, the gaps, the parts, and how to check it. Copy the shape that fits, then change it. Capability names are the ones in [capabilities.md](capabilities.md).

## Contents

- [Screenshot to OCR to clipboard](#screenshot-to-ocr-to-clipboard)
- [Local LLM rewrite of the selected text](#local-llm-rewrite-of-the-selected-text)
- [Meeting recorder with a transcript](#meeting-recorder-with-a-transcript)
- [Window layouts per project](#window-layouts-per-project)
- [Home-server status tile](#home-server-status-tile)
- [Clipboard history with search](#clipboard-history-with-search)
- [Patterns across the examples](#patterns-across-the-examples)

## Screenshot to OCR to clipboard

**Goal:** on ctrl+alt+shift+o, let me drag a box on screen, read the text in it, and put the text on the clipboard.

| Feature | Capability |
|---|---|
| The chord | `hotkey` |
| Drag a box and capture it | gap: no screen capture capability. `shell` runs `/usr/sbin/screencapture -i <path>` on macOS (`maim -s <path>` on Linux X11) |
| Read the text | gap: computation. Sidecar |
| Put it on the clipboard, say so | `clipboard`, `ui` (toast) or `notify` |

**Parts:** a Lua script `screen-ocr` and a Python sidecar `ocr` on port 8810. The sidecar's `POST /process` takes an image path and returns the text; inside it, use macOS Vision through `ocrmac`, or `pytesseract` with Tesseract on Linux.

**Flow:** chord, then `bricolo.shell.run("/usr/sbin/screencapture", {"-i", "-x", path})`, then if the file exists post the path to the sidecar, then `bricolo.clipboard.set(text)` and `bricolo.ui.toast("Copied " .. #text .. " characters")`. If the user pressed Escape, screencapture writes no file: log it and stop.

**Ask only:** which OCR engine, when the user cares (Vision is macOS only).

**Needs:** on macOS, the Screen Recording permission for "bricolo node", because the node runs screencapture.

**Check:** `bricolo sidecar list` shows `ocr ready`; `curl --data /path/to/sample.png 127.0.0.1:8810/process` prints its text; the script logs its start-up line; press the chord once and read `bricolo logs --plugin screen-ocr.lua`.

## Local LLM rewrite of the selected text

**Goal:** on ctrl+alt+shift+r, rewrite the text selected in any app with a local LLM, and let me accept the result before it replaces the selection.

| Feature | Capability |
|---|---|
| The chord | `hotkey` |
| Read the selection | no "selection" call: press cmd+c with `keyboard.keystroke`, wait 150 ms, then `clipboard.get`. Keep the old clipboard and restore it after |
| Rewrite | gap: an LLM. Sidecar |
| Let me accept or edit | `ui` review panel (`on_review(accepted, text)`) |
| Replace the selection | `clipboard.set`, then `keyboard.keystroke` cmd+v |

**Parts:** a Lua script `rewrite` and a sidecar. Two ways to get the LLM:

- Register an existing server as the sidecar. Ollama serves on 127.0.0.1:11434 and answers `GET /` with 200: `bricolo sidecar add ollama --command /opt/homebrew/bin/ollama --port 11434 --health-url http://127.0.0.1:11434/ -- serve`. Its API speaks JSON, which a plugin parses with `serde_json` but a Lua script can't parse safely.
- So for a script, add a thin Python sidecar `rewrite` (port 8811) that holds the prompt, calls Ollama (or `llama-cpp-python`, or MLX), and answers plain text: `POST /process` with the text, 200 with the rewrite.

**Ask only:** which model, and the rewrite style (fix grammar, shorter, more formal) if the request doesn't say.

**Needs:** the Accessibility permission for "bricolo node" (keyboard), and the model download; tell the user its size first.

**Check:** curl the sidecar with a sentence; the script logs its start-up line; select text in a text editor, press the chord, accept, and see the text replaced.

## Meeting recorder with a transcript

**Goal:** a switch in the hub starts and stops recording a meeting; when it stops, the audio is transcribed and saved as a Markdown note in `~/Notes/meetings/`.

| Feature | Capability |
|---|---|
| Start and stop | `ui` tile with a switch (`on_switch`), and a chord as a second way (`hotkey`) |
| Record | `mic.record_start`, `record_stop`. One recording lasts at most 600 s |
| Transcribe | `speech.transcribe_file` with a speech sidecar behind `[speech]` |
| Save the note | `disk.write` under `~/Notes/meetings/` |
| Show progress | `ui` caption while recording, toast when saved |

**Gaps:** recordings longer than 10 minutes. Stop and start a new recording every 9 minutes with `bricolo.timer.every`, transcribe each part as it ends, and append the parts in order.

**Parts:** a Lua script `meeting-notes`, and a speech sidecar that follows the speech contract in [sidecars.md](sidecars.md#speech-and-tts-sidecars) (`POST /transcribe` with a file path), for example a Python server around `faster-whisper` or `parakeet-mlx`, registered with `bricolo sidecar add stt ...` and named in `[speech] sidecar = "stt"`. `[speech]` changes need a node restart.

**Ask only:** which speech engine, if none is configured yet. `bricolo call speech transcribe '[[], "wav"]'` answers `unavailable` when `[speech]` names no server.

**Needs:** ffmpeg, the Microphone permission, disk space for the model.

**Check:** `curl --data /path/to/sample.wav 127.0.0.1:8765/transcribe` prints the text; flip the switch on and off in the hub; the note appears and the log says where.

## Window layouts per project

**Goal:** save where my windows are for a project, and restore that layout later from a list.

| Feature | Capability |
|---|---|
| Save and restore chords | `hotkey` |
| Read and move windows | `window.list`, `window.set_frame`, `window.screens` |
| Open missing apps | `app.launch` |
| Remember layouts | `kv` (strings; store one key per project) |
| Pick a project | `ui` choice list (`on_choice`), at most 50 entries |

**Gaps:** none. No sidecar.

**Parts:** one app. A WASM plugin if the user wants it on several Macs, since the layout logic (matching windows to saved entries by app and title, scaling frames to a different screen) deserves unit tests in `src/domain.rs`. A script is fine for one machine.

**Needs:** the Accessibility permission (window). Linux: X11 only.

**Check:** `cargo test` for the matching and scaling; `bricolo fire <plugin> save-layout`, move a window, `bricolo fire <plugin> restore-layout`, and read the log for each window moved.

## Home-server status tile

**Goal:** a tile in the hub shows whether my home server is up, checked every minute, with a "Check now" button, and a notification when it changes.

| Feature | Capability |
|---|---|
| Check | `http` GET to the server's health URL |
| Every minute | `timer.every` |
| The tile and its button | `ui.set_tile` with `buttons`, the `ui` event (`on_button` in Lua) |
| Notify on change | `notify` |
| Remember the last state | `kv` |

**Gaps:** none, as long as the server answers HTTP. To ping a host instead, use `shell` with `/sbin/ping -c 1 -t 2 <host>` and allow that path.

**Parts:** a WASM plugin made from `bricolo new <name> --template http-service`, so it can be shared, with `[http] allow = ["home.lan:8080"]` set to the server's host and port. No sidecar.

**Check:** `bricolo fire <plugin> check-now` logs `up` or `down`; the tile shows in the hub; point the URL at a closed port to see the change notification.

## Clipboard history with search

**Goal:** remember the last 200 texts I copied, and on ctrl+alt+shift+h let me search them and put one back on the clipboard.

| Feature | Capability |
|---|---|
| Notice copies | no clipboard-change event: poll `clipboard.get` every second with `timer.every` and compare with the last text |
| Keep the history | `kv` for a short history; a sidecar with SQLite for a long, searchable one |
| Ask for a search term | `ui` review panel: the text field holds the query (`on_review(accepted, text)`) |
| Show matches, pick one | `ui` choice list, at most 50 entries of 200 bytes, so show previews |
| Restore it | `clipboard.set` |

**Gaps:** full-text search over hundreds of entries is easier in a sidecar. Python's standard library has `sqlite3` with FTS5: a sidecar `cliphist` (port 8812) with `POST /add` (text in), `POST /search` (query in, newline-separated ids and previews out) and `GET /item/<id>` (the full text).

**Parts:** a Lua script `clip-history` (the poll, the chord, the panels) and the Python sidecar. For a quick version, skip the sidecar: keep 50 entries in `kv` and filter with `string.find` in the script.

**Ask only:** whether a short in-memory history is enough. Mention that the history may hold passwords copied from a password manager, and offer to skip texts copied from it (`app.frontmost`).

**Check:** copy three texts, then `curl --data 'term' 127.0.0.1:8812/search`; press the chord, search, pick, and paste.

## Patterns across the examples

- The script or plugin owns the trigger, the UI and the capability calls; the sidecar owns computation and packages. Keep the sidecar free of UI and OS automation.
- Pass files by path between them when both run on the same machine.
- Plain text between a Lua script and its sidecar; JSON is fine for a plugin.
- A missing capability (screen capture, selection, clipboard events) usually has a small workaround with `shell`, `keyboard` or a timer. Say which one you chose and why.
- When the request needs something no workaround covers, such as mouse clicks, stop and tell the user.
