-- __NAME__.lua: on ctrl+alt+cmd+t, transcribes the newest recording in
-- ~/Recordings (newest by name, so timestamped names work), logs the text
-- and says how many words it heard. ctrl+alt+cmd+x stops the speech.
--
-- `bricolo.speech` needs a [speech] server in node.toml. The node reloads this
-- file each time you save it. Full reference: docs/scripts.md.

local FOLDER = "~/Recordings"
local CHORD = "ctrl+alt+cmd+t"
local STOP_CHORD = "ctrl+alt+cmd+x"

local AUDIO = { wav = true, m4a = true, mp3 = true, flac = true, ogg = true, aiff = true }

-- The audio file that sorts last by name, or nil.
local function newest_audio(names)
  local newest
  for _, name in ipairs(names) do
    local ext = name:match("%.([^.]+)$")
    if ext and AUDIO[ext:lower()] and (newest == nil or name > newest) then
      newest = name
    end
  end
  return newest
end

-- What to say once a transcription is done.
local function summary(text)
  local words = 0
  for _ in text:gmatch("%S+") do
    words = words + 1
  end
  if words == 0 then
    return "I heard nothing"
  end
  return "Transcribed " .. words .. (words == 1 and " word" or " words")
end

local function transcribe_latest()
  local names, err = bricolo.disk.list(FOLDER)
  if not names then
    bricolo.log.warn("cannot list " .. FOLDER .. ": " .. err)
    return
  end
  local name = newest_audio(names)
  if not name then
    bricolo.log.warn("no recording in " .. FOLDER)
    return
  end
  local text, terr = bricolo.speech.transcribe(FOLDER .. "/" .. name)
  if not text then
    bricolo.log.warn("cannot transcribe " .. name .. ": " .. terr)
    return
  end
  bricolo.log.info("heard: " .. text)
  bricolo.tts.speak(summary(text))
end

for chord, fn in pairs({ [CHORD] = transcribe_latest, [STOP_CHORD] = bricolo.tts.stop }) do
  local id, err = bricolo.hotkey.bind(chord, function()
    fn()
  end)
  if not id then
    bricolo.log.warn("__NAME__: cannot bind " .. chord .. ": " .. err)
  end
end
bricolo.log.info("__NAME__: press " .. CHORD .. " to transcribe the newest recording")
