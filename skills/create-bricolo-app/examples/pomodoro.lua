-- pomodoro.lua: ctrl+alt+shift+p starts a 25 minute focus timer and
-- notifies when it ends; pressing again while it runs cancels it. Counts
-- finished sessions per day in bricolo.kv.
--
-- Capabilities: hotkey, timer, notify, kv. Shows: timer ids, cancel,
-- kv as a small store, a pure helper above the bricolo calls.

local CHORD = "ctrl+alt+shift+p"
local MINUTES = 25

-- The kv key for today's count, like "done:2026-11-16".
local function day_key(now)
  return "done:" .. os.date("%Y-%m-%d", now)
end

local running -- the timer id while a session runs

local function finish()
  running = nil
  local key = day_key(os.time())
  local done = tonumber(bricolo.kv.get(key) or "0") + 1
  bricolo.kv.set(key, tostring(done))
  bricolo.notify.show("Pomodoro", "Time for a break. Sessions today: " .. done)
end

local function toggle()
  if running then
    bricolo.timer.cancel(running)
    running = nil
    bricolo.notify.show("Pomodoro", "Cancelled")
    return
  end
  running = bricolo.timer.after(MINUTES * 60 * 1000, finish)
  bricolo.notify.show("Pomodoro", MINUTES .. " minutes, starting now")
end

local id, err = bricolo.hotkey.bind(CHORD, toggle)
if not id then
  bricolo.log.warn("cannot bind " .. CHORD .. ": " .. err)
end
bricolo.log.info("pomodoro ready on " .. CHORD)
