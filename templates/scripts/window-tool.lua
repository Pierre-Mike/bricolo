-- __NAME__.lua: centers the focused window on its screen at two thirds of
-- the screen's size when you press ctrl+alt+cmd+m.
--
-- On macOS, window calls need the Accessibility permission for the app
-- that runs `bricolo run`. The node reloads this file each time you save it.
-- Full reference: docs/scripts.md.

local CHORD = "ctrl+alt+cmd+m"
local PERCENT = 66

-- A frame `percent` of `area`'s size, centered in it. Frames are
-- {x, y, w, h}.
local function centered(area, percent)
  local w = area.w * percent // 100
  local h = area.h * percent // 100
  return { x = area.x + (area.w - w) // 2, y = area.y + (area.h - h) // 2, w = w, h = h }
end

-- The screen holding the window's center; else the main screen.
local function screen_for(frame, screens)
  local cx, cy = frame.x + frame.w // 2, frame.y + frame.h // 2
  local main
  for _, s in ipairs(screens) do
    local f = s.frame
    if cx >= f.x and cx < f.x + f.w and cy >= f.y and cy < f.y + f.h then
      return s
    end
    if s.main then
      main = s
    end
  end
  return main or screens[1]
end

local function center()
  local win, err = bricolo.window.focused()
  if not win then
    bricolo.log.warn("no focused window" .. (err and (": " .. err) or ""))
    return
  end
  local screens, serr = bricolo.window.screens()
  if not screens or #screens == 0 then
    bricolo.log.warn("no screens" .. (serr and (": " .. serr) or ""))
    return
  end
  local target = centered(screen_for(win.frame, screens).visible_frame, PERCENT)
  local ok, ferr = bricolo.window.set_frame(win.id, target)
  if ok then
    bricolo.log.info("centered " .. win.app)
  else
    bricolo.log.warn("cannot move " .. win.app .. ": " .. ferr)
  end
end

local id, err = bricolo.hotkey.bind(CHORD, center)
if id then
  bricolo.log.info("__NAME__: press " .. CHORD .. " to center the focused window")
else
  bricolo.log.warn("__NAME__: cannot bind " .. CHORD .. ": " .. err)
end
