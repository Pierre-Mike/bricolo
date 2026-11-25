-- daily-note.lua: ctrl+alt+shift+n appends the clipboard text to
-- ~/Notes/<date>.md under a timestamp.
--
-- Capabilities: hotkey, clipboard, disk, notify. Shows: reading a file
-- that may not exist yet, writing it back, and telling the user.

local CHORD = "ctrl+alt+shift+n"
local FOLDER = "~/Notes"

-- The note with `text` appended as a new entry.
local function append(note, text, now)
  local entry = "## " .. os.date("%H:%M", now) .. "\n\n" .. text .. "\n"
  if note == nil or note == "" then
    return "# " .. os.date("%Y-%m-%d", now) .. "\n\n" .. entry
  end
  return note .. "\n" .. entry
end

local function save_clipboard()
  local text, err = bricolo.clipboard.get()
  if not text then
    bricolo.notify.show("Daily note", err and ("Cannot read the clipboard: " .. err) or "The clipboard holds no text")
    return
  end
  local now = os.time()
  local path = FOLDER .. "/" .. os.date("%Y-%m-%d", now) .. ".md"
  -- A missing file answers "failed: not found"; start a new note then.
  local note = bricolo.disk.read(path)
  local ok, werr = bricolo.disk.write(path, append(note, text, now))
  if ok then
    bricolo.notify.show("Daily note", "Saved to " .. path)
  else
    bricolo.log.warn("cannot write " .. path .. ": " .. werr)
  end
end

local id, err = bricolo.hotkey.bind(CHORD, save_clipboard)
if not id then
  bricolo.log.warn("cannot bind " .. CHORD .. ": " .. err)
end
