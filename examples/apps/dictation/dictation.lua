-- dictation.lua: press ctrl+alt+shift+d to start recording, press it again
-- to stop. The recording goes to the speech server, the text goes to the
-- clipboard, and a paste keystroke puts it at the cursor (cmd+v on macOS,
-- ctrl+v on Linux). A recording stops by itself after MAX_SECS.
--
-- A port of the core of apps/dictation from the Hammerspoon setup: record
-- with ffmpeg, transcribe on the warm parakeet server, paste. Live preview,
-- the review panel and the rewrites are left out. While it records and
-- transcribes, a caption from bricolo.ui says so; the hub and the menu bar also
-- show the recording dot.
--
-- Needs, in node.toml, a [speech] server (see docs/apps.md, "Dictation"),
-- and ffmpeg on the machine. On macOS the app that runs `bricolo run` (your
-- terminal) needs the Microphone and Accessibility permissions. Install:
--
--   bricolo install examples/apps/dictation/dictation.lua

-- ┌─ Configure ──────────────────────────────────────────────────────────┐
local CHORD = "ctrl+alt+shift+d" -- starts and stops a recording
local MAX_SECS = 60              -- the safety stop
local PASTE = true               -- false: leave the text on the clipboard
-- └──────────────────────────────────────────────────────────────────────┘

local state = "idle" -- idle, starting, recording or transcribing
local recording -- the mic recording id
local safety -- the safety stop's timer id
local caption -- the caption's panel id while recording or transcribing

-- Shows what dictation is doing in a caption, which stays up until done().
local function status(text)
  caption = bricolo.ui.caption(text) or caption
end

-- Hides the caption, then shows `text`, if any, in a toast.
local function done(text)
  if caption then
    bricolo.ui.hide_panel(caption)
    caption = nil
  end
  if text then
    bricolo.ui.toast(text)
  end
end

local function fail(what, err)
  bricolo.log.warn(what .. ": " .. tostring(err))
  done(what .. ": " .. tostring(err))
  state = "idle"
end

-- The paste chord for this system.
local function paste_keys()
  if bricolo.os() == "macos" then
    return { "cmd" }, "v"
  end
  return { "ctrl" }, "v"
end

local function trim(text)
  return (text:gsub("^%s+", ""):gsub("%s+$", ""))
end

local function stop()
  if state ~= "recording" then
    return
  end
  state = "transcribing"
  status("transcribing…")
  if safety then
    bricolo.timer.cancel(safety)
    safety = nil
  end
  local path, err = bricolo.mic.record_stop(recording)
  recording = nil
  if not path then
    return fail("recording failed", err)
  end
  local text, terr = bricolo.speech.transcribe_file(path)
  if not text then
    return fail("transcription failed", terr)
  end
  text = trim(text)
  if text == "" then
    done("heard nothing")
    state = "idle"
    return
  end
  assert(bricolo.clipboard.set(text))
  bricolo.log.info("heard: " .. text)
  done()
  if PASTE then
    local mods, key = paste_keys()
    local ok, kerr = bricolo.keyboard.keystroke(mods, key)
    if not ok then
      done("copied; paste it yourself (" .. kerr .. ")")
    end
  end
  state = "idle"
end

local function start()
  state = "starting"
  -- The mic's own limit is a backstop, should the timer below not fire.
  local id, err = bricolo.mic.record_start({ max_secs = MAX_SECS + 5 })
  if not id then
    return fail("cannot record", err)
  end
  recording = id
  state = "recording"
  safety = bricolo.timer.after(MAX_SECS * 1000, function()
    safety = nil
    bricolo.log.info("stopped after " .. MAX_SECS .. " s")
    stop()
  end)
  status("recording; " .. CHORD .. " to stop")
end

local function toggle()
  if state == "idle" then
    start()
  elseif state == "recording" then
    stop()
  end
  -- Presses while starting or transcribing are dropped.
end

local id, err = bricolo.hotkey.bind(CHORD, toggle)
if not id then
  bricolo.log.warn("dictation: cannot bind " .. CHORD .. ": " .. err)
end
bricolo.log.info("dictation: press " .. CHORD .. " to start and stop recording")

function dispose()
  if recording then
    bricolo.mic.record_cancel(recording)
  end
end
