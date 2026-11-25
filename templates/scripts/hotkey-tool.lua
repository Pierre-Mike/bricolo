-- __NAME__.lua: shows the clipboard text in a notification when you press
-- ctrl+alt+cmd+c.
--
-- The node reloads this file each time you save it. Pure helpers come
-- first and make no bricolo calls, so they can move to scripts/lib/ and be
-- tested with busted. Full reference: docs/scripts.md.

local CHORD = "ctrl+alt+cmd+c"
local MAX_CHARS = 200

-- The notification body for `text`: whitespace runs become one space, and
-- text longer than `max` characters is cut with "…". nil for no text.
local function preview(text, max)
  if text == nil then
    return nil
  end
  local flat = (text:gsub("%s+", " ")):match("^ ?(.-) ?$")
  if flat == "" then
    return nil
  end
  local count = utf8.len(flat)
  if count and count > max then
    flat = flat:sub(1, utf8.offset(flat, max + 1) - 1) .. "…"
  end
  return flat
end

local function show_clipboard()
  local text, err = bricolo.clipboard.get()
  if err then
    bricolo.log.warn("cannot read the clipboard: " .. err)
    return
  end
  local body = preview(text, MAX_CHARS) or "The clipboard holds no text"
  local ok, nerr = bricolo.notify.show("Clipboard", body)
  if not ok then
    bricolo.log.warn("cannot notify: " .. nerr)
  end
end

-- A chord that fails to bind (another app holds it, or no hotkeys on this
-- system) is logged; the script keeps running.
local id, err = bricolo.hotkey.bind(CHORD, show_clipboard)
if id then
  bricolo.log.info("__NAME__: press " .. CHORD .. " to show the clipboard")
else
  bricolo.log.warn("__NAME__: cannot bind " .. CHORD .. ": " .. err)
end
