-- __NAME__.lua: checks a URL every minute and logs when it goes up or
-- down. The last state is kept in bricolo.kv, so a reload doesn't log the same
-- state again.
--
-- Scripts may reach any host; plugins need an [http] allow list. The node
-- reloads this file each time you save it. Full reference: docs/scripts.md.

local URL = "http://127.0.0.1:8080/health"
local EVERY_MS = 60 * 1000

-- "up" or "down", and a line describing it.
local function classify(response, err)
  if response and response.status < 400 then
    return "up", URL .. " is up (" .. response.status .. ")"
  elseif response then
    return "down", URL .. " is down: status " .. response.status
  end
  return "down", URL .. " is down: " .. err
end

local function check()
  local state, line = classify(bricolo.http.request({ url = URL, timeout_ms = 5000 }))
  if bricolo.kv.get("health") ~= state then
    if state == "up" then
      bricolo.log.info(line)
    else
      bricolo.log.warn(line)
    end
    bricolo.kv.set("health", state)
  end
end

bricolo.timer.after(1000, check)
bricolo.timer.every(EVERY_MS, check)
bricolo.log.info("__NAME__: checking " .. URL .. " every " .. EVERY_MS // 1000 .. " s")
