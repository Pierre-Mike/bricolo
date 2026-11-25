#!/usr/bin/env bash
# The Linux smoke test, on the desktop inside.sh starts: install the window
# tiler and the caps fixture, add a Lua script that uses the clipboard,
# windows and notifications, run `bricolo run`, drive it with `bricolo fire` and
# xdotool, then stop it with SIGTERM. Fails on the first check that fails.
set -euo pipefail
cd "$(dirname "$0")/../.."

bricolo="$PWD/target/debug/bricolo"
work="$(mktemp -d)"
export BRICOLO_CONFIG_DIR="$work/config" BRICOLO_DATA_DIR="$work/data"
mkdir -p "$BRICOLO_CONFIG_DIR/scripts"
printf '[mesh]\nenabled = false\n' >"$BRICOLO_CONFIG_DIR/node.toml"

step() {
    echo
    echo "\$ $*"
    "$@"
}

# Waits up to 10 s for `pattern` in the node's log file.
expect_log() {
    for _ in $(seq 100); do
        if grep -h -- "$1" "$BRICOLO_DATA_DIR"/logs/bricolo.log.* 2>/dev/null | tail -1 | grep .; then
            return 0
        fi
        sleep 0.1
    done
    echo "FAIL: no log line matching: $1" >&2
    exit 1
}

step "$bricolo" install -y plugins/window-tiler
step "$bricolo" install -y fixtures/caps

cat >"$BRICOLO_CONFIG_DIR/scripts/desk.lua" <<'LUA'
-- The Linux desktop from Lua: clipboard, windows, notifications, a hotkey.
local ok, err = bricolo.clipboard.set("from lua")
bricolo.log.info("desk clipboard: set " .. tostring(ok or err) .. ", read " .. tostring(bricolo.clipboard.get()))
local w = bricolo.window.focused()
if w then
  bricolo.log.info(string.format("desk focused: %s %q at %d,%d %dx%d",
    w.app, w.title, w.frame.x, w.frame.y, w.frame.w, w.frame.h))
end
local screens = bricolo.window.screens()
bricolo.log.info(string.format("desk screens: %d, main %dx%d",
  #screens, screens[1].frame.w, screens[1].frame.h))
bricolo.log.info("desk notify: " .. tostring(bricolo.notify.show("bricolo smoke", "hello from Lua")))
bricolo.hotkey.bind({ "ctrl", "alt", "shift" }, "j", function()
  bricolo.log.info("desk hotkey pressed")
end)
LUA

xterm +sb -title "smoke xterm" -geometry 60x15+100+100 -e sleep 600 &
xterm_pid=$!
for _ in $(seq 100); do
    xdotool search --name "smoke xterm" >/dev/null 2>&1 && break
    sleep 0.1
done

echo
echo "\$ bricolo run &"
"$bricolo" run >"$work/run.log" 2>&1 &
node=$!
for _ in $(seq 100); do
    "$bricolo" status 2>/dev/null | grep -q '"name": "desk"' && break
    sleep 0.1
done
expect_log "desk hotkey\|desk notify" >/dev/null
step "$bricolo" status

step "$bricolo" fire caps clip-roundtrip
expect_log "clip-roundtrip: ok"
for cmd in windows front screens notify:hello-from-caps; do
    step "$bricolo" fire caps "$cmd"
done
expect_log "windows: [1-9]"
expect_log "front: Some(\"XTerm\")"
expect_log "screens: 1"
expect_log "notify: Ok(())"
expect_log "desk clipboard: set true, read from lua"
expect_log "desk focused: XTerm"
expect_log "desk screens: 1, main 1280x800"
expect_log "desk notify: true"

echo
echo "\$ xdotool key super+alt+Left    # window-tiler"
xdotool key super+alt+Left
expect_log "left-half: XTerm window"
echo
echo "\$ xdotool key ctrl+alt+shift+j  # desk.lua"
xdotool key ctrl+alt+shift+j
expect_log "desk hotkey pressed"

echo
echo "\$ kill -TERM bricolo run"
kill -TERM "$node"
code=0
wait "$node" || code=$?
echo "bricolo run exited with $code"
kill "$xterm_pid" 2>/dev/null || true
echo
echo "== bricolo run output (last 15 lines)"
sed 's/\x1b\[[0-9;]*m//g' "$work/run.log" | tail -15
[ "$code" -eq 0 ] || { echo "FAIL: bricolo run exited with $code" >&2; exit 1; }
echo
echo "smoke: ok"
