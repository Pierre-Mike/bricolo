# Writing scripts

Scripts are Lua 5.4 files for quick personal automations. They call the same standard library as WASM plugins, through the `bricolo` global, and need no build step. The node reloads a script each time you save it.

## Where scripts live

```
~/.config/bricolo/scripts/
├── speak.lua          runs
├── ticker.lua         runs
└── lib/
    ├── text.lua       module: require("lib.text")
    └── audio/
        └── init.lua   module: require("lib.audio")
```

- Every `.lua` file directly in `scripts/` runs as its own script, in its own Lua state.
- Files in subfolders never run on their own. They are modules that a script loads with `require`. `require("lib.text")` reads `scripts/lib/text.lua`, then `scripts/lib/text/init.lua`.
- A script's name is its file name without `.lua`. It may use ASCII letters, digits, `-` and `_`. The node skips other `.lua` files and logs a warning.
- `BRICOLO_CONFIG_DIR` moves the whole config folder, `scripts/` included.

`bricolo new-script <name>` writes a commented starter to `scripts/<name>.lua`. A running node starts it at once. `--template` picks another start (`bricolo new-script --list` shows them all), such as `hotkey-tool` or `window-tool`. `bricolo check <file>.lua` compiles a script without running it, to catch syntax errors.

`bricolo install <file>.lua`, `bricolo install <folder>` (every `.lua` file in it, modules in subfolders included) and `bricolo install github:owner/repo` copy scripts into `scripts/` and record them in `plugins.lock`. Because scripts run with full access, `bricolo install` lists the files and asks before it copies them. [apps.md](apps.md) covers sharing scripts through GitHub releases.

## Examples

Speak the clipboard on cmd+alt+s:

```lua
bricolo.hotkey.bind({ "cmd", "alt" }, "s", function()
  local text = bricolo.clipboard.get()
  if not text then
    return
  end
  local ok, err = bricolo.tts.speak(text)
  if not ok then
    bricolo.log.warn("cannot speak: " .. err)
  end
end)
```

Log a line every minute, and stop after ten:

```lua
local n = 0
local id
id = bricolo.timer.every(60 * 1000, function()
  n = n + 1
  bricolo.log.info("minute " .. n)
  if n == 10 then
    bricolo.timer.cancel(id)
  end
end)
```

Transcribe a recording and save the text:

```lua
local text, err = bricolo.speech.transcribe("~/Recordings/memo.wav")
if text then
  bricolo.disk.write("~/Notes/memo.md", text)
else
  bricolo.log.error("transcription failed: " .. err)
end
```

Give the script controls in its hub card, with a switch and a button, and show a caption:

```lua
bricolo.ui.set_tile{
  title = "Dictation", status = "idle",
  switches = { {id = "review", label = "Review before pasting", on = true} },
  buttons = { {id = "last", label = "Paste last take"} },
  on_switch = function(id, on) bricolo.kv.set(id, tostring(on)) end,
  on_button = function(id) bricolo.log.info("pressed " .. id) end,
}
bricolo.ui.caption("listening…")
```

Record five seconds, transcribe them and type the text at the cursor:

```lua
local id = assert(bricolo.mic.record_start({ max_secs = 5 }))
bricolo.timer.sleep(5000)
local path = assert(bricolo.mic.record_stop(id))
local text = assert(bricolo.speech.transcribe_file(path))
bricolo.keyboard.type_text(text)
```

[examples/apps/dictation/dictation.lua](../examples/apps/dictation/dictation.lua) is a complete dictation app: a chord starts and stops the recording, and the text is pasted at the cursor. [apps.md](apps.md#dictation) walks through it.

## Calls that wait

`bricolo.speech.transcribe`, `bricolo.tts.speak`, `bricolo.disk.read` and the other calls marked "waits" below pause the calling code until the answer comes back. The rest of the node keeps running, and so do the script's other callbacks. You write straight-line code with no callbacks for the result.

The node runs the script body, `init`, `dispose` and every timer or hotkey callback in a coroutine of its own. Waiting calls also work inside coroutines you create with `coroutine.create` or `coroutine.wrap`.

A module's top-level code runs inside `require`, and it can't call waiting functions while it loads. Lua stops it with "attempt to yield across a C-call boundary". The module's functions may wait once a script calls them.

## Errors

A call that fails returns `nil` and an error name:

| `err` | Meaning |
|---|---|
| `"unavailable"` | No node offering this capability is online. |
| `"timeout"` | The deadline passed. |
| `"denied"` | A peer's allow list, or the operating system, refused the call. |
| `"failed: <message>"` | The provider ran and reported an error, such as `"failed: not found"`. |

Calls that succeed with nothing to return give `true`, so `assert(bricolo.tts.speak("hi"))` works.

An error that nothing catches stops the script. The node logs it with a traceback, releases the script's timers and hotkeys, and starts the script again from the top. A second uncaught error turns the script off. It stays off until you save its file again or ask the node to reload it. `bricolo status` shows the state, the restart count and the error. Other scripts and plugins keep running either way.

## The `bricolo` library

| Call | Returns | Notes |
|---|---|---|
| `bricolo.log.trace/debug/info/warn/error(...)` | nothing | Joins its arguments with spaces. `print(...)` logs at info level. |
| `bricolo.timer.after(ms, fn)` | timer id | Calls `fn(id)` once after `ms` milliseconds. |
| `bricolo.timer.every(ms, fn)` | timer id | Calls `fn(id)` every `ms` milliseconds (at least 10) until cancelled. |
| `bricolo.timer.cancel(id)` | `true` or `false` | `false` when the id is unknown or already fired. |
| `bricolo.timer.sleep(ms)` | `true` | Waits. |
| `bricolo.kv.get(key)` | string or `nil` | Each script has its own store, in `data/<name>.lua/kv.json`. Values are strings. |
| `bricolo.kv.set(key, value)` | `true` | Keys are 1 to 256 bytes, values at most 1 MiB. |
| `bricolo.kv.delete(key)` / `bricolo.kv.keys()` | `true` / list | |
| `bricolo.disk.read(path)` | file contents | Waits. Paths are absolute or start with `~/`. |
| `bricolo.disk.write(path, data)` | `true` | Waits. Creates or replaces the file. |
| `bricolo.disk.list(path)` | sorted list of names | Waits. |
| `bricolo.tts.speak(text [, voice])` | `true` | Waits until the text is queued. |
| `bricolo.tts.stop()` | `true` | Waits. Stops speech and clears the queue. |
| `bricolo.tts.status()` | `{speaking, queued, voice}` | Waits. `voice` is the default voice, or `nil`. |
| `bricolo.speech.transcribe(path [, format])` | text | Waits. Reads the audio file on this node, even when `speech` runs on a peer. `format` defaults to the file extension. |
| `bricolo.speech.transcribe_audio(bytes, format)` | text | Waits. Transcribes audio already in a string. |
| `bricolo.speech.transcribe_file(path)` | text | Waits. The speech server reads the file itself. When `speech` routes to a peer, the node reads the file and sends the bytes. |
| `bricolo.hotkey.bind(chord, fn[, on_release])` | binding id | Waits. `chord` looks like `"cmd+alt+h"`. Each press calls `fn(id)`; on macOS each release calls `on_release(id)`, for hold-to-talk. `"fn"` binds the Fn (Globe) key alone, on macOS only, and needs Input Monitoring. |
| `bricolo.hotkey.bind(mods, key, fn[, on_release])` | binding id | The Hammerspoon form: `bind({"cmd", "alt"}, "h", fn)`. |
| `bricolo.hotkey.unbind(id)` | `true` | Waits. |
| `bricolo.clipboard.get()` | text or `nil` | Waits. `nil` with no error means the clipboard holds no text. |
| `bricolo.clipboard.set(text)` | `true` | Waits. |
| `bricolo.clipboard.formats()` | list of `"text"`, `"image"`, `"files"` | Waits. |
| `bricolo.clipboard.get_image()` | path or `nil` | Waits. Saves the image as a PNG in the script's data folder. |
| `bricolo.clipboard.set_image(path)` | `true` | Waits. A PNG or JPEG file. |
| `bricolo.clipboard.get_files()` | list of paths | Waits. The files copied in the file manager. |
| `bricolo.clipboard.set_files(paths)` | `true` | Waits. Copies files so a paste drops them. |
| `bricolo.window.list()` | list of windows | Waits. A window is `{id, app, title, frame}`, and a frame is `{x, y, w, h}` in points from the top-left corner of the main display (pixels on Linux and Windows). |
| `bricolo.window.focused()` | window or `nil` | Waits. |
| `bricolo.window.focus(id)` | `true` | Waits. |
| `bricolo.window.set_frame(id, frame)` | `true` | Waits. `frame` takes `w` and `h`, or `width` and `height`. |
| `bricolo.window.close(id)` | `true` | Waits. Presses the window's close button; the app may ask to save first. |
| `bricolo.window.set_minimized(id, minimized)` | `true` | Waits. Minimizes the window to the Dock, or restores it when `minimized` is `false`. |
| `bricolo.window.screens()` | list of screens | Waits. A screen is `{id, frame, visible_frame, main}`. `visible_frame` leaves out the menu bar and the Dock. |
| `bricolo.app.launch(app)` | `true` | Waits. `app` is a bundle id like `"com.apple.Notes"` or an app name like `"Notes"`, not a path. On Linux, a desktop file id like `"firefox"` or an app name like `"Files"`. On Windows, a program name like `"notepad"`, found through the Start Menu, App Paths or `PATH`. |
| `bricolo.app.open_url(url)` | `true` | Waits. `file:` URLs answer `"denied"`. |
| `bricolo.app.running()` | list of apps | Waits. An app is `{name, bundle_id, pid}`. Only apps that show in the Dock; on Linux and Windows, apps with a window (`bundle_id` is `nil`). |
| `bricolo.app.frontmost()` | app or `nil` | Waits. |
| `bricolo.app.quit(app)` | `true` | Waits. Asks the app to quit, as Cmd+Q does. `app` is a bundle id or a name; `nil, "failed: no such app"` when none runs. On Linux (X11 only) and Windows it closes each of the app's windows. |
| `bricolo.app.set_hidden(app, hidden)` | `true` | Waits. Hides the app's windows, as Cmd+H does, or shows them when `hidden` is `false`. `nil, "failed: no such app"` when none runs. On Linux (X11 only) and Windows it minimizes or restores the app's windows. |
| `bricolo.notify.show(title [, body])` | `true` | Waits. Shows a notification: macOS's, the desktop's over D-Bus on Linux, or a toast on Windows. |
| `bricolo.shell.run(program [, args [, env]])` | `{code, stdout, stderr}` | Waits. `program` is an absolute path, run directly with no shell, in `/`. `args` is a list of strings and `env` a table of names to values. `code` is `nil` when a signal ended the program. |
| `bricolo.http.request(url)` | `{status, headers, body}` | Waits. A GET. Any status is an answer; only a transport problem is an error. |
| `bricolo.http.request{url=, method=, headers=, body=, timeout_ms=}` | `{status, headers, body}` | Waits. `method` defaults to `"GET"`, `headers` is a table of names to values. In the answer, header names are lowercase and repeated headers are joined with `", "`. |
| `bricolo.ui.set_tile{title=, status=, switches=, buttons=, on_switch=, on_button=}` | `true` | Sets or replaces this script's tile in the hub. A switch is `{id, label, on}`, a button `{id, label}`. `on_switch(id, on)` runs when the user toggles a switch, `on_button(id)` when they press a button. |
| `bricolo.ui.clear_tile()` | `true` | Removes the tile. |
| `bricolo.ui.set_widget{title=, rows={{label=, value=}}}` | `true` | Sets or replaces this script's widget in the hub's Widgets view: display only, up to 16 rows, each a label and a value. Only for values worth a glance; controls go in the tile. |
| `bricolo.ui.clear_widget()` | `true` | Removes the widget. |
| `bricolo.ui.show_panel{kind=, title=, text=, choices=, timeout_ms=, on_choice=, on_review=}` | panel id | Shows a floating panel. `kind` is `"caption"`, `"toast"`, `"choice_list"` or `"review"`. `on_choice(index, label)` gets a 1-based index, or `nil, nil` when the user dismisses the list. `on_review(accepted, text)` gets the edited text. |
| `bricolo.ui.caption(text [, timeout_ms])` / `bricolo.ui.toast(text [, timeout_ms])` | panel id | Shortcuts for `show_panel`. A toast hides after 4 seconds unless `timeout_ms` says otherwise. |
| `bricolo.ui.hide_panel(id)` | `true` | Hides one of this script's panels. Unknown ids are ignored. |
| `bricolo.keyboard.keystroke(mods, key)` | `true` | Waits. Presses `key` with `mods` held, like `keystroke({"cmd"}, "v")`. Names follow hotkey chords. Keys are positions on a US layout. |
| `bricolo.keyboard.keystroke(chord)` | `true` | The chord form: `keystroke("cmd+v")`. |
| `bricolo.keyboard.type_text(text)` | `true` | Waits. Types `text` into the focused app one key press per character, with no modifiers held, about 80 characters a second on macOS. Line breaks press Enter and tabs press Tab. At most 20000 characters. |
| `bricolo.mouse.position()` | `{x, y}` | Waits. Where the pointer is, in `window`'s coordinates. |
| `bricolo.mouse.move_to(x, y)` | `true` | Waits. Moves the pointer without clicking. Any number works: each is rounded to the nearest whole number (half away from zero). NaN or a value outside the 32-bit range answers `"failed: coordinate out of range"`. |
| `bricolo.mouse.click([button [, count]])` | `true` | Waits. Clicks at the pointer. `button` is `"left"` (the default), `"right"` or `"middle"`; any other name raises a Lua error. `count` is 1 to 3 (default 1): 2 is a double click. Any other count answers `"failed: click count must be 1 to 3"`. |
| `bricolo.mouse.scroll(dx, dy)` | `true` | Waits. Scrolls by whole lines: positive `dy` scrolls down, positive `dx` scrolls right. Fractions round like `move_to`, with the same out-of-range answer. |
| `bricolo.mic.record_start([{max_secs=, format=}])` | recording id | Waits until audio arrives. Records the microphone through ffmpeg. `max_secs` (at most 600, the default) stops the recording by itself. `format` can only be `"wav16k-mono"`. One recording at a time per script. |
| `bricolo.mic.record_stop(id)` | file path | Waits. A 16 kHz mono WAV in `data/<name>.lua/recordings/`. The newest 10 recordings are kept. |
| `bricolo.mic.record_cancel(id)` | `true` | Waits. Stops the recording and deletes its file. |
| `bricolo.audio.play(path[, {volume=, looping=}])` | sound id | Waits until the sound starts. Plays a WAV, AIFF, MP3, AAC or M4A file on the default output: AVAudioPlayer on macOS, ffmpeg on Linux, `"unavailable"` on Windows. `volume` is 0 to 100 (default 100). `looping = true` starts again at the end, with no gap, until `stop`. `~/` expands. Up to 16 sounds at once per script. |
| `bricolo.audio.set_volume(id, volume)` | `true` | Waits. Changes the volume while the sound plays. On Linux the sound restarts from the beginning. |
| `bricolo.audio.stop(id)` | `true` | Waits. Stopping a sound that has ended does nothing. |
| `bricolo.audio.sounds()` | list of `{id, path, volume, looping}` | Waits. This script's sounds that still play. |
| `bricolo.app.watch(fn)` | `true` | Waits. Starts app events and calls `fn(event)` for each one. An event is `{change, name, bundle_id, pid}`, where `change` is `"launched"`, `"terminated"` or `"activated"` (it came to the front). `bricolo.app.watch(nil)` stops them. macOS and Windows; answers `"unavailable"` elsewhere. |
| `bricolo.system.watch(fn)` | `true` | Waits. Calls `fn(name)` with `"will_sleep"`, `"did_wake"`, `"screen_locked"`, `"screen_unlocked"` or `"screens_changed"`. `bricolo.system.watch(nil)` stops it. |
| `bricolo.system.idle_seconds()` | number | Waits. Seconds since the last key press, click or pointer move. |
| `bricolo.system.power()` | `{battery, plugged_in}` | Waits. `battery` is 0 to 100, or `nil` without a battery. |
| `bricolo.system.keep_awake(on)` | `true` | Waits. Keeps the device from idle sleep until `keep_awake(false)` or the script stops or reloads. |
| `bricolo.files.watch(path, fn)` | watch id | Waits. Watches `path` and everything under it, which must lie under a granted path (scripts can reach any). Calls `fn(event)` with `{watch, path, change}`, where `change` is `"created"`, `"modified"` or `"removed"`. A missing path answers `"failed"`. |
| `bricolo.files.unwatch(id)` | `true` | Waits. Stopping a watch that has stopped does nothing. |
| `bricolo.screen.capture([area])` | file path | Waits. Saves a PNG of the main screen (every monitor on Linux and Windows), or of `area` (`{x, y, w, h}`, where `width` and `height` also work), in `data/<name>.lua/screenshots/<ms>.png`. A zero width or height answers `"failed: the area is empty"`. |
| `bricolo.selection.get_text()` | text or `nil` | Waits. The text selected in the frontmost app, or `nil` when nothing is selected or the app does not say. |
| `bricolo.camera.snapshot()` | file path | Waits. Takes one photo with the default camera through ffmpeg: `avfoundation` on macOS (Camera permission), V4L2 on Linux, DirectShow on Windows. A JPEG in `data/<name>.lua/camera/`. The newest 20 photos are kept. ffmpeg is stopped after 10 seconds (`timeout`). `[camera] device` and `ffmpeg` in node.toml choose the device and program. |
| `bricolo.secrets.get(name)` | string or `nil` | Waits. The secret, or `nil` when it is not set. Kept in the macOS Keychain, in the login keychain under the service `bricolo`, on Linux in the Secret Service under the same service, or on Windows in the Credential Manager as `bricolo/<script>.lua/<name>`; each script sees only its own. A name is 1 to 128 letters, digits, `.`, `_` or `-`; any other name answers `"failed"`. |
| `bricolo.secrets.set(name, value)` | `true` | Waits. Stores or replaces the secret. |
| `bricolo.secrets.delete(name)` | `true` | Waits. Deleting a secret that is not set does nothing. |
| `bricolo.claude_code.start(dir, prompt)` | session | Waits until Claude Code names the session (at most 25 seconds), not for the work. Runs the `claude` CLI headless in `dir`, which must be under `[claude-code] dirs` in node.toml (else `"denied"`; without the section `"unavailable"`). A session is `{id, dir, state, last_result, title, started, updated}`; `state` is `"working"`, `"finished"`, `"waiting"` (permissions were refused), `"failed"` or `"idle"`. |
| `bricolo.claude_code.send(id, prompt)` | session | Waits. Resumes a session that is not working with another prompt; a working one answers `"failed: session is working"`. |
| `bricolo.claude_code.list()` | list of sessions | Waits. The sessions started since the node started. |
| `bricolo.claude_code.status(id)` | session | Waits. `last_result` holds the text of the last finished turn. |
| `bricolo.claude_code.stop(id)` | `true` | Waits. Kills the running turn, if any; the session becomes `"idle"`. |
| `bricolo.accessibility.list_apps()` | list of `{name, bundle_id, pid, frontmost}` | Waits. Apps with a window. docs/computer-use.md has the rules agents follow. |
| `bricolo.accessibility.list_windows(app)` | list of `{index, title, frame, focused, minimized}` | Waits. `app` is a bundle id, a name or `"pid:123"`. |
| `bricolo.accessibility.snapshot(app, window, query, element, screenshot)` | `{app, pid, window, tree, element_count, truncated, focused, screenshot}` | Waits. Every argument after `app` may be `nil`. `tree` has one line per element, starting with its index. |
| `bricolo.accessibility.find(app, text)` | list of `{index, role, name, value, frame, enabled, focused, actions}` | Waits. |
| `bricolo.accessibility.click(app, index)` | `{verification, detail, state}` | Waits. `verification` is `"verified"`, `"unverified"`, `"suspected-noop"` or `"mismatch"`; `state` is the fresh snapshot. |
| `bricolo.accessibility.set_value(app, index, value)` | result as `click` | Waits. Reads the value back. |
| `bricolo.accessibility.type_text(app, index, text)` | result as `click` | Waits. `index` may be `nil` for the focused field. |
| `bricolo.accessibility.press_key(app, key)` | result as `click` | Waits. `"return"`, `"cmd+a"`; quit, close and lock chords answer `"failed: system_shortcut_blocked…"`. |
| `bricolo.accessibility.perform(app, index, action)` | result as `click` | Waits. One of the element's `actions`. |
| `bricolo.accessibility.menu(app, path)` | result as `click` | Waits. `{"File", "Export…"}`. |
| `bricolo.accessibility.wait(app, text, timeout_ms)` | list of elements | Waits until an element shows `text`; `nil, "timeout"` after `timeout_ms` (10 s when `nil`). |
| `bricolo.process.list()` | list of `{pid, parent, name, command, user, cpu, memory, started}` | Waits about a quarter second for CPU use. |
| `bricolo.process.get(pid)` | process or `nil` | Waits. |
| `bricolo.process.stop(pid)` | `true` | Waits. SIGTERM; closes the windows on Windows. Refuses the node and its parent. |
| `bricolo.process.kill(pid)` | `true` | Waits. SIGKILL. |
| `bricolo.device.volume()`, `muted()`, `brightness()`, `wifi()`, `dark_mode()`, `focus_mode()` | number 0 to 100, or boolean | Waits. |
| `bricolo.device.set_volume(level)`, `set_muted(on)`, `set_brightness(level)`, `set_wifi(on)`, `set_dark_mode(on)`, `set_focus_mode(on)` | `true` | Waits. `"unavailable"` where the system has no way to do it. |
| `bricolo.device.lock()`, `bricolo.device.sleep()` | `true` | Waits. |
| `bricolo.scripting.run_applescript(source)`, `run_jxa(source)` | text | Waits, at most a minute. macOS. |
| `bricolo.scripting.list_shortcuts()` | list of names | Waits. macOS. |
| `bricolo.scripting.run_shortcut(name, input)` | text | Waits. `input` may be `nil`. |
| `bricolo.scripting.run_powershell(source)` | text | Waits. `powershell.exe` on Windows, `pwsh` elsewhere. |
| `bricolo.virtual_screen.create(width, height)` | `{id, name, width, height, pids}` | Waits. Linux (Xvfb) and Windows (hidden desktop). |
| `bricolo.virtual_screen.list()` | list of screens | Waits. Only this script's screens. |
| `bricolo.virtual_screen.launch(id, program, args)` | pid | Waits. `program` is an absolute path. |
| `bricolo.virtual_screen.capture(id)` | path | Waits. A PNG in the script's data folder. |
| `bricolo.virtual_screen.click(id, x, y)`, `type_text(id, text)`, `press_key(id, key)` | `true` | Waits. |
| `bricolo.virtual_screen.destroy(id)` | `true` | Waits. Ends the screen and its programs. |
| `bricolo.os()` | string | `"macos"`, `"linux"` or `"windows"`. |
| `bricolo.script` | string | This script's name. |

Scripts get every capability without a permission prompt, because you wrote them. `bricolo.disk` can reach any path. `bricolo.shell.run` can run any program given by its absolute path, and `bricolo.http.request` can reach any host: scripts have no `[shell]` or `[http]` allow list, unlike plugins. The node still sets `HOME` and `PATH` for every program (on Windows `PATH` and the system variables listed in [windows.md](windows.md#shell)), so `env` can't override them. `http` follows no redirects, keeps at most 8 MiB of body and waits 30 seconds by default (at most 300). A program that runs past 60 seconds is killed and the call answers `"timeout"`.

Calls still go through the router, so `rules.toml` decides where `tts`, `speech`, `disk`, `clipboard`, `notify` and `claude-code` run, and a peer's allow list still applies. `hotkey`, `window`, `app`, `keyboard`, `mouse`, `mic`, `audio`, `system`, `files`, `screen`, `selection`, `camera`, `secrets`, `shell` and `http` always run on this node.

`ui` always runs on this node, and the hub (`bricolo-ui`) draws it; without a running hub, tiles and panels wait in the node until one connects. A script has one tile, one widget, and one panel of each kind: showing a caption again replaces the text of the one on screen and keeps its id. Panels never take focus from the app in front. See [ui.md](ui.md).

`hotkey`, `clipboard`, `window`, `app`, `notify`, `keyboard` and `mouse` work on macOS, on Linux ([linux.md](linux.md) lists what needs X11) and on Windows ([windows.md](windows.md)); elsewhere they answer `"unavailable"`. `hotkey` and `window.screens` need `bricolo run`'s main-thread loop on macOS, and so do `screen` and `selection`, which answer `"unavailable"` without it. `selection` needs the Accessibility permission and `screen` the Screen Recording permission on macOS (without it, calls answer `"denied"`). On Linux `selection` reads the PRIMARY selection, and `screen` works on X11, and on Wayland with `grim`. `window`, `keyboard` and `mouse` need the Accessibility permission there (without it, calls answer `"denied"`), the `"fn"` chord needs Input Monitoring (`bricolo permissions` and the hub's Library view show what the node holds), `camera` needs the Camera permission, `secrets` uses the Keychain on macOS and the Secret Service on Linux (`"unavailable"` without a keyring), `system` reads power, idle time and sleep events through `/sys`, X11 and logind on Linux, and `mic` needs the Microphone permission for the app that runs `bricolo run`. `mic` needs ffmpeg, and so does `audio` on Linux. `tts` speaks with `say` on macOS unless `node.toml` names a TTS server, and `speech` needs a `[speech]` server. On Windows, `selection` and `audio` answer `"unavailable"`, `screen` captures every monitor, `secrets` uses the Credential Manager, `mic` and `camera` use ffmpeg with DirectShow, `tts` speaks with SAPI, `keyboard` and `mouse` answer `"denied"` when the focused app runs as administrator, and frames are physical pixels. Nothing there asks for a permission.

## Lifecycle

1. The node reads the file and compiles it. A syntax error turns the script off, with the error in `bricolo status`.
2. It runs the file from the top, then calls the global `init()` if the script defines one.
3. Timer, hotkey, `bricolo.ui`, `bricolo.app.watch`, `bricolo.system.watch` and `bricolo.files.watch` callbacks run as their events arrive.
4. Before the script stops or reloads, the node cancels callbacks still waiting, then calls the global `dispose()` if the script defines one. It releases the script's timers, hotkeys, recordings, sounds, watches, keep-awake, tile, widget and panels afterwards, so `dispose` only needs to clean up what the node doesn't know about.

Saving a script reloads that script only, usually within 200 ms. Saving a module reloads every script that loaded it. If the new file doesn't compile, the old version keeps running and `bricolo status` shows `reload refused`. If the new version fails while it starts, the node starts the old one again and shows `reload failed`. Nothing in the Lua state survives a reload. Keep values that should last in `bricolo.kv`.

Each run of Lua code has a CPU budget: `init_budget_ms` (default 2000) for the file body and `init` together, `event_budget_ms` (default 500) for each callback and for `dispose`. Both come from `[runtime]` in `node.toml`, the same settings plugins use. Time spent waiting on a call doesn't count. A loop that runs past its budget fails with `ran past its N ms budget` and counts as an uncaught error, even if the script catches it with `pcall`. Each script may allocate up to 256 MiB.

`bricolo status` lists the scripts with their state (`running`, `off`, `stopped` or `disabled`), last error, restart count and live timers. A script turned off in the hub is `disabled`: it does not start, saving it does not start it, and it stays off after the node restarts, until you turn it on again. `bricolo logs --plugin <name>.lua` shows one script's log lines. The control socket also accepts `{"cmd":"reload-script","script":"<name>"}`, which turns an `off` script back on.

## The sandbox

Each script gets Lua 5.4's `string`, `table`, `math`, `utf8` and `coroutine` libraries, and `os` without `execute`, `exit`, `remove`, `rename`, `tmpname` and `setlocale`. There is no `io` and no `debug`.

- `require` loads Lua source from `scripts/` only. It refuses names that aren't dotted paths, such as `"../x"`, and files that resolve outside `scripts/` through a symlink. There are no C modules and no `package.loadlib`, `package.path` or `package.searchers`.
- `dofile` and `loadfile` take a path relative to `scripts/` and refuse anything outside it.
- `load` accepts source text only. It refuses precompiled chunks from `string.dump`.

Reach files, the network and the rest of the machine through `bricolo`.

## Coming from Hammerspoon

| Hammerspoon | bricolo |
|---|---|
| `hs.hotkey.bind({"cmd"}, "x", fn)` | `bricolo.hotkey.bind({"cmd"}, "x", fn)` |
| `hs.timer.doAfter(1.5, fn)` | `bricolo.timer.after(1500, fn)`. Times are in milliseconds. |
| `hs.timer.doEvery(60, fn)` | `bricolo.timer.every(60000, fn)` |
| `t:stop()` | `bricolo.timer.cancel(id)` |
| `hs.pasteboard.getContents()` | `bricolo.clipboard.get()` |
| `hs.window.focusedWindow():frame()` | `bricolo.window.focused().frame`, the same `x`, `y`, `w` and `h` |
| `win:setFrame(f)` | `bricolo.window.set_frame(win.id, f)` |
| `hs.screen.mainScreen():frame()` | the `visible_frame` of the screen with `main == true` in `bricolo.window.screens()` |
| `hs.application.launchOrFocus("Notes")` | `bricolo.app.launch("Notes")` |
| `hs.application.frontmostApplication():name()` | `bricolo.app.frontmost().name` |
| `hs.urlevent.openURL(url)` | `bricolo.app.open_url(url)` |
| `hs.notify.new({title=t, informativeText=b}):send()` | `bricolo.notify.show(t, b)` |
| `hs.eventtap.keyStroke({"cmd"}, "v")` | `bricolo.keyboard.keystroke({"cmd"}, "v")` |
| `hs.eventtap.keyStrokes(text)` | `bricolo.keyboard.type_text(text)` |
| `hs.task.new(ffmpeg, …)` to record the mic | `bricolo.mic.record_start()`, then `bricolo.mic.record_stop(id)` for the file |
| `hs.sound.getByFile(path):play()` | `bricolo.audio.play(path)`; `{looping = true}` for `sound:loopSound(true)` |
| `hs.execute(cmd)` | `bricolo.shell.run("/bin/sh", {"-c", cmd}).stdout` |
| `hs.http.get(url)` | `bricolo.http.request(url)`, which returns a table |
| `hs.settings.get/set` | `bricolo.kv.get/set`, strings only |
| `hs.logger.new("x").i(...)` | `bricolo.log.info(...)` |
| `hs.alert.show(text)` | `bricolo.ui.toast(text)` |
| `hs.chooser.new(fn):choices(list):show()` | `bricolo.ui.show_panel{kind = "choice_list", choices = labels, on_choice = fn}` |
| `io.open(path)` | `bricolo.disk.read(path)` |
| `require("lib.util")` | `require("lib.util")`, from `scripts/lib/util.lua` |

Pure Lua helpers in `lib/`, the ones that never touch `hs.*`, carry over unchanged, along with their `busted` tests. Code that calls `hs.*` needs rewriting to call `bricolo.*`.
