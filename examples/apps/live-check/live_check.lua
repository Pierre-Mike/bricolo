-- live_check.lua: checks the standard library on a real machine. Tests run
-- every capability against fakes; this script runs the real backends, so
-- run it on each OS after adding or porting a capability. A port is done
-- when no line says FAIL.
--
--   ctrl+alt+cmd+shift+l  run every automatic check (focus a normal window
--                         first: it is minimized, restored, hidden, shown)
--   ctrl+alt+cmd+shift+s  scroll the window under the pointer up 15 lines,
--                         then back after 2 s; check it with your eyes
--   ctrl+alt+cmd+shift+c  take one camera photo (the camera light turns on)
--
-- Each check logs "live_check: PASS|FAIL|UNAVAILABLE|DENIED <call> <detail>"
-- (read them with bricolo logs) and the run ends with a toast that counts them.
-- UNAVAILABLE means this OS has no backend for the call yet; DENIED means a
-- permission is missing (Accessibility, Screen Recording, Camera).

local HYPER = "ctrl+alt+cmd+shift+"

-- Pure: the kind of result for a call that answered `value, err`.
local function verdict(ok, err)
  if ok then
    return "PASS"
  end
  err = tostring(err or "")
  if err:find("^unavailable") then
    return "UNAVAILABLE"
  elseif err:find("^denied") then
    return "DENIED"
  end
  return "FAIL"
end

local counts, watchers = {}, {}

local function record(call, ok, detail)
  local kind = verdict(ok, detail)
  counts[kind] = (counts[kind] or 0) + 1
  bricolo.log.info(string.format("live_check: %s %s %s", kind, call, tostring(detail or "")))
end

local function check_mouse()
  local p, err = bricolo.mouse.position()
  record("mouse.position", p, p and string.format("%.0f,%.0f", p.x, p.y) or err)
  if not p then return end
  local moved, merr = bricolo.mouse.move_to(p.x + 40.6, p.y)
  local q = moved and bricolo.mouse.position()
  local landed = q and math.abs(q.x - (p.x + 41)) <= 1
  record("mouse.move_to", landed, q and string.format("asked %.1f, got %.0f", p.x + 40.6, q.x) or merr)
  bricolo.mouse.move_to(p.x, p.y)
end

local function check_window()
  local w, err = bricolo.window.focused()
  record("window.focused", w, w and (w.app .. ": " .. w.title) or err or "no focused window")
  if not w then return end
  local down, derr = bricolo.window.set_minimized(w.id, true)
  record("window.set_minimized(true)", down, derr)
  bricolo.timer.sleep(1200)
  local up, uerr = bricolo.window.set_minimized(w.id, false)
  bricolo.timer.sleep(800)
  local back = bricolo.window.focused()
  record("window.set_minimized(false)", up and back and back.id == w.id,
    uerr or (back and "focused again" or "restored but not focused"))
end

local function check_app()
  local a, err = bricolo.app.frontmost()
  record("app.frontmost", a, a and a.name or err)
  if not a then return end
  local key = a.bundle_id or a.name
  local hid, herr = bricolo.app.set_hidden(key, true)
  bricolo.timer.sleep(800)
  local front = bricolo.app.frontmost()
  record("app.set_hidden(true)", hid and front and front.name ~= a.name,
    herr or ("frontmost is now " .. tostring(front and front.name)))
  local shown, serr = bricolo.app.set_hidden(key, false)
  record("app.set_hidden(false)", shown, serr)
end

-- Watches the folder screen.capture writes to and captures again. The event
-- arrives after the calling callback returns, so this calls `done` from the
-- watch callback, or from a timer after 3 s.
local function check_screen_and_files(done)
  local path, err = bricolo.screen.capture({ x = 0, y = 0, w = 400, h = 300 })
  record("screen.capture", path, path or err)
  if not path then return done() end
  local dir = path:match("^(.*)[/\\][^/\\]+$")
  local id, settled
  local function settle(seen)
    if settled then return end
    settled = true
    record("files event", seen, seen and "created seen" or "no event within 3 s")
    bricolo.files.unwatch(id)
    done()
  end
  local werr
  id, werr = bricolo.files.watch(dir, function(e)
    if e.change == "created" then settle(true) end
  end)
  record("files.watch", id, werr or dir)
  if not id then return done() end
  bricolo.timer.after(3000, function() settle(false) end)
  bricolo.screen.capture({ x = 0, y = 0, w = 40, h = 30 })
end

local function check_selection()
  local text, err = bricolo.selection.get_text()
  record("selection.get_text", err == nil, err or (text and (#text .. " bytes") or "nothing selected"))
end

local function check_secrets()
  local name, value = "live-check", "v" .. os.time()
  local ok, err = bricolo.secrets.set(name, value)
  local got = ok and bricolo.secrets.get(name)
  local gone = ok and bricolo.secrets.delete(name)
  local after = ok and bricolo.secrets.get(name)
  record("secrets", ok and got == value and gone and after == nil,
    err or "set, read back, deleted")
end

local function check_system()
  local idle, err = bricolo.system.idle_seconds()
  record("system.idle_seconds", idle, idle and string.format("%.1f s", idle) or err)
  local power, perr = bricolo.system.power()
  record("system.power", power, power and string.format("battery %s, plugged in %s",
    tostring(power.battery), tostring(power.plugged_in)) or perr)
  local on, kerr = bricolo.system.keep_awake(true)
  record("system.keep_awake", on and bricolo.system.keep_awake(false), kerr)
end

-- Lists sessions only: starting one would run Claude Code. Without
-- `[claude-code] dirs` in node.toml this is UNAVAILABLE.
local function check_claude_code()
  local list, err = bricolo.claude_code.list()
  record("claude_code.list", list, list and (#list .. " sessions") or err)
end

-- Reads the focused app's tree, finds an element, and sets nothing: an
-- action could change the user's work.
local function check_accessibility()
  local apps, err = bricolo.accessibility.list_apps()
  record("accessibility.list_apps", apps, apps and (#apps .. " apps") or err)
  if not apps or #apps == 0 then return end
  local front = apps[1]
  local sel = "pid:" .. front.pid
  local s, serr = bricolo.accessibility.snapshot(sel)
  record("accessibility.snapshot", s, s and string.format("%s: %d elements", front.name, s.element_count) or serr)
  local found, ferr = bricolo.accessibility.find(sel, "button")
  record("accessibility.find", found, found and (#found .. " buttons") or ferr)
  local blocked, berr = bricolo.accessibility.press_key(sel, "cmd+q")
  record("accessibility.press_key blocks cmd+q", not blocked and tostring(berr):find("system_shortcut_blocked"), berr)
end

-- Reads only: setting the volume, Wi-Fi or sleep would surprise the user.
local function check_device_and_process()
  for _, name in ipairs({ "volume", "muted", "brightness", "wifi", "dark_mode", "focus_mode" }) do
    local v, err = bricolo.device[name]()
    record("device." .. name, v ~= nil, v ~= nil and tostring(v) or err)
  end
  local list, err = bricolo.process.list()
  record("process.list", list, list and (#list .. " processes, busiest " .. list[1].name) or err)
end

local function check_scripting_and_clipboard()
  local out, err = bricolo.scripting.run_applescript("return 6 * 7")
  record("scripting.run_applescript", out == "42", out or err)
  local ps, perr = bricolo.scripting.run_powershell("6 * 7")
  record("scripting.run_powershell", ps == "42", ps or perr)
  local formats, ferr = bricolo.clipboard.formats()
  record("clipboard.formats", formats, formats and table.concat(formats, ",") or ferr)
end

-- Starts a virtual screen, captures it and ends it.
local function check_virtual_screen()
  local v, err = bricolo.virtual_screen.create(800, 600)
  record("virtual_screen.create", v, v and v.name or err)
  if not v then return end
  local png, cerr = bricolo.virtual_screen.capture(v.id)
  record("virtual_screen.capture", png, png or cerr)
  local gone, derr = bricolo.virtual_screen.destroy(v.id)
  record("virtual_screen.destroy", gone, derr)
end

local function run_all()
  counts = {}
  for call, result in pairs(watchers) do
    record(call, result.ok, result.err)
  end
  check_mouse()
  check_selection()
  check_secrets()
  check_claude_code()
  check_accessibility()
  check_device_and_process()
  check_scripting_and_clipboard()
  check_virtual_screen()
  check_system()
  check_window()
  check_app()
  check_screen_and_files(function()
    local summary = string.format("live check: %d pass, %d fail, %d unavailable, %d denied",
      counts.PASS or 0, counts.FAIL or 0, counts.UNAVAILABLE or 0, counts.DENIED or 0)
    bricolo.log.info(summary)
    bricolo.ui.toast(summary, 8000)
  end)
end

local function scroll()
  local ok, err = bricolo.mouse.scroll(0, -15)
  record("mouse.scroll", ok, err or "sent; did the window move up 15 lines and back?")
  bricolo.timer.sleep(2000)
  bricolo.mouse.scroll(0, 15)
end

local function camera()
  local path, err = bricolo.camera.snapshot()
  record("camera.snapshot", path, path or err)
  bricolo.ui.toast("camera: " .. tostring(path or err), 8000)
end

function init()
  bricolo.hotkey.bind(HYPER .. "l", run_all)
  bricolo.hotkey.bind(HYPER .. "s", scroll)
  bricolo.hotkey.bind(HYPER .. "c", camera)
  -- Event sources start at load; their events show as toasts.
  local ok, err = bricolo.app.watch(function(e)
    if e.change ~= "activated" then bricolo.ui.toast("app " .. e.change .. ": " .. e.name) end
  end)
  watchers["app.watch"] = { ok = ok, err = err }
  ok, err = bricolo.system.watch(function(name) bricolo.ui.toast("system: " .. name) end)
  watchers["system.watch"] = { ok = ok, err = err }
  bricolo.log.info("live_check: press ctrl+alt+cmd+shift+l to run the checks")
end
