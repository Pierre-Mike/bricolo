-- app-launcher.lua: one chord per app. ctrl+alt+shift+<key> launches the
-- app, or brings it to the front when it already runs.
--
-- Capabilities: hotkey, app. Shows: binding several chords from a table,
-- the Hammerspoon (mods, key, fn) form, checking every result.

local MODS = { "ctrl", "alt", "shift" }
local APPS = {
  b = "Safari",
  n = "Notes",
  t = "Terminal",
}

local bound = 0
for key, app in pairs(APPS) do
  local id, err = bricolo.hotkey.bind(MODS, key, function()
    local ok, lerr = bricolo.app.launch(app)
    if not ok then
      bricolo.log.warn("cannot launch " .. app .. ": " .. lerr)
    end
  end)
  if id then
    bound = bound + 1
  else
    bricolo.log.warn("cannot bind ctrl+alt+shift+" .. key .. ": " .. err)
  end
end
bricolo.log.info("app-launcher bound " .. bound .. " chords")
