-- __NAME__.lua: a bricolo script.
--
-- The node runs every .lua file directly in scripts/ and reloads this one
-- each time you save it. Files in subfolders (scripts/lib/...) are modules:
-- load them with require("lib.name").
--
-- The bricolo global is the standard library: bricolo.log, bricolo.timer, bricolo.kv, bricolo.disk,
-- bricolo.tts, bricolo.speech, bricolo.hotkey, bricolo.clipboard, bricolo.window, bricolo.app, bricolo.notify,
-- bricolo.shell and bricolo.http. Calls that wait, such as
-- bricolo.tts.speak, pause this script until they finish; no callbacks needed.
-- A call that fails returns nil and an error name: "unavailable",
-- "timeout", "denied" or "failed: <message>".
--
-- An uncaught error restarts the script once. A second one turns it off
-- until you save the file again. `bricolo status` shows its state.
-- Full reference: docs/scripts.md in the bricolo repository.

bricolo.log.info("__NAME__ loaded")

-- Speak the clipboard on cmd+alt+s.
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

-- Count runs in this script's private key/value store.
local runs = tonumber(bricolo.kv.get("runs") or "0") + 1
bricolo.kv.set("runs", tostring(runs))

-- Timers take milliseconds.
bricolo.timer.after(1000, function()
  bricolo.log.info("__NAME__ has started " .. runs .. " times")
end)

-- Optional hooks: init() runs after the file loads, dispose() before the
-- script stops or reloads. Timers and hotkeys are released for you.
function init() end

function dispose() end
