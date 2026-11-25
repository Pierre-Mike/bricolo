-- __NAME__.lua: sends the clipboard text to a sidecar when you press
-- ctrl+alt+cmd+p, and shows the answer in a toast.
--
-- A sidecar is a local HTTP server the node starts and restarts: make one
-- with `bricolo new-sidecar`, register it with `bricolo sidecar add`. The sidecar
-- answers GET /health with {"ready": true} and POST /process with text.
-- On load, this script waits for the sidecar and sends it a sample, so
-- its log shows at once whether the two talk.
--
-- Scripts may reach any host; plugins need an [http] allow list. The node
-- reloads this file each time you save it. Full reference: docs/scripts.md.

local BASE = "http://127.0.0.1:__PORT__"
local CHORD = "ctrl+alt+cmd+p"

-- `text` on one line, cut to `max` characters with "…".
local function preview(text, max)
  local flat = (text:gsub("%s+", " ")):match("^ ?(.-) ?$")
  local count = utf8.len(flat)
  if count and count > max then
    flat = flat:sub(1, utf8.offset(flat, max + 1) - 1) .. "…"
  end
  return flat
end

-- Posts `text` to the sidecar. Returns its answer, or nil and why not.
local function process(text)
  local response, err = bricolo.http.request({
    url = BASE .. "/process",
    method = "POST",
    headers = { ["content-type"] = "text/plain; charset=utf-8" },
    body = text,
    timeout_ms = 30000,
  })
  if not response then
    -- "unavailable" usually means the sidecar is not running: check
    -- `bricolo sidecar list`.
    return nil, "cannot reach " .. BASE .. ": " .. err
  end
  if response.status >= 400 then
    return nil, "the sidecar answered " .. response.status .. ": " .. preview(response.body, 200)
  end
  return response.body
end

local function process_clipboard()
  local text = bricolo.clipboard.get()
  if text == nil or text:match("^%s*$") then
    text = "hello from __NAME__"
  end
  local answer, err = process(text)
  if answer then
    bricolo.log.info("sidecar answered: " .. preview(answer, 80))
    bricolo.ui.toast(preview(answer, 500))
  else
    bricolo.log.warn(err)
    bricolo.ui.toast(err)
  end
end

-- Polls /health every 2 s until the sidecar is ready, then sends a sample.
local attempts = 0
local poll
poll = bricolo.timer.every(2000, function()
  attempts = attempts + 1
  local health = bricolo.http.request({ url = BASE .. "/health", timeout_ms = 2000 })
  if health and health.status == 200 and not health.body:find('"ready"%s*:%s*false') then
    bricolo.timer.cancel(poll)
    local answer, err = process("hello from __NAME__")
    if answer then
      bricolo.log.info("sidecar ready; sample answer: " .. preview(answer, 80))
    else
      bricolo.log.warn(err)
    end
  elseif attempts == 30 then
    bricolo.timer.cancel(poll)
    bricolo.log.warn("no sidecar ready at " .. BASE .. " after 60 s; check `bricolo sidecar list`")
  end
end)

-- A chord that fails to bind (another app holds it, or no hotkeys on this
-- system) is logged; the script keeps running.
local id, err = bricolo.hotkey.bind(CHORD, process_clipboard)
if id then
  bricolo.log.info("__NAME__: press " .. CHORD .. " to send the clipboard to " .. BASE)
else
  bricolo.log.warn("__NAME__: cannot bind " .. CHORD .. ": " .. err)
end
