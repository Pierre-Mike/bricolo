# Lua script patterns

A script is one `.lua` file directly in the scripts folder (`~/.config/bricolo/scripts/`, or `$BRICOLO_CONFIG_DIR/scripts`). Each runs in its own sandboxed Lua 5.4 state. Files in subfolders are modules, loaded with `require("lib.name")`. The full reference is docs/scripts.md in the bricolo repository; every call is in [capabilities.md](capabilities.md).

## Shape of a script

```lua
-- front-app.lua: shows the frontmost app's name when I press ctrl+alt+shift+f.

local CHORD = "ctrl+alt+shift+f"

-- Pure helpers first: no bricolo calls, so they can move to scripts/lib/ later.
local function describe(app)
  if app == nil then
    return "No app is in front"
  end
  return app.name .. (app.bundle_id and (" (" .. app.bundle_id .. ")") or "")
end

local function show()
  local app, err = bricolo.app.frontmost()
  if err then
    bricolo.log.warn("cannot read the frontmost app: " .. err)
    return
  end
  local ok, nerr = bricolo.notify.show("Frontmost app", describe(app))
  if not ok then
    bricolo.log.warn("cannot notify: " .. nerr)
  end
end

-- A chord that fails to bind is logged; the script keeps running.
local id, err = bricolo.hotkey.bind(CHORD, show)
if not id then
  bricolo.log.warn("cannot bind " .. CHORD .. ": " .. err)
end
bricolo.log.info("front-app ready on " .. CHORD)
```

## Rules that bite

- Calls that wait (`bricolo.clipboard.get`, `bricolo.tts.speak`, `bricolo.hotkey.bind`, `bricolo.http.request`...) return their value, or `nil, err`. Calls with nothing to return give `true`. Always look at `err`.
- `err` is `"unavailable"`, `"timeout"`, `"denied"` or `"failed: <message>"`.
- `bricolo.clipboard.get()` returns `nil` with no error when the clipboard holds no text.
- Chords are `"cmd+alt+h"` strings, or the Hammerspoon form `bricolo.hotkey.bind({"cmd", "alt"}, "h", fn)`. Modifiers: `cmd`, `ctrl`, `alt` (`option`), `shift`. Keys: letters, digits, `f1`-`f20`, arrows, `space`, `enter`, `tab`, `escape`, `delete`, `home`, `end`, `pageup`, `pagedown`, punctuation.
- Timers take milliseconds: `bricolo.timer.after(1500, fn)`, `bricolo.timer.every(60 * 1000, fn)`. Keep the id to `bricolo.timer.cancel(id)`.
- `bricolo.kv` keeps strings per script and survives reloads. Nothing else survives a reload.
- Frames are `{x, y, w, h}` from the top-left of the main display. `visible_frame` leaves out the menu bar and the Dock.
- There is no JSON library. Talk to your own sidecars in plain text; [sidecars.md](sidecars.md#calling-a-sidecar-from-a-script) has the call.
- No `io`, no `os.execute`, no `debug`. Reach files with `bricolo.disk`, programs with `bricolo.shell.run("/abs/path", {args})`, the network with `bricolo.http.request`.
- A module's top-level code runs inside `require` and can't call waiting functions. Its functions can.
- Each callback has a CPU budget (500 ms by default; time spent waiting doesn't count). An endless loop fails the callback.
- Two uncaught errors in a row turn the script off until you save it again. `bricolo status` shows the error.
- Optional globals: `init()` runs after the file body, `dispose()` before the script stops or reloads. Timers and hotkeys are released for you.

## Checking a script

```bash
bricolo check scripts/<name>.lua           # syntax only; prints "<file> compiles"
bricolo status                             # scripts[]: {"name": "<name>", "state": "running", "error": null, ...}
bricolo logs --plugin <name>.lua -n 20     # its log lines; errors come with a traceback
```

`bricolo check` doesn't catch a misspelled `bricolo` call. That shows up when the line runs, as an error in `bricolo status` and the log.
