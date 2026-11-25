#!/usr/bin/env bash
# The bricolo-ui smoke test on macOS (docs/ui.md). It builds bricolo, bricolo-ui and the
# test plugins, runs a node and bricolo-ui against a temporary config folder,
# takes screenshots, and checks that panels never take focus from the app
# in front.
#
#   scripts/ui-smoke.sh           screenshots stay in the temporary folder
#   scripts/ui-smoke.sh --save    and are copied to docs/screenshots/
#
# Needs an unlocked screen, and the Screen Recording permission for the
# terminal that runs it. It shows panels and the hub on the main display,
# then quits bricolo-ui and stops the node. Exits 1 if a check fails.

set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH:/usr/sbin"

save=false
[ "${1:-}" = --save ] && save=true
[ "$(uname)" = Darwin ] || {
    echo "macOS only" >&2
    exit 1
}

work=$(mktemp -d /tmp/bricolo-ui-smoke.XXXXXX)
export BRICOLO_CONFIG_DIR="$work/config" BRICOLO_DATA_DIR="$work/data"
shots="$work/shots"
mkdir -p "$BRICOLO_CONFIG_DIR/scripts" "$BRICOLO_DATA_DIR" "$shots" "$work/bin"
bricolo=target/debug/bricolo
ui=target/debug/bricolo-ui
failures=0

say() { printf '%s\n' "$*"; }
check() { # check <name> <expected> <actual>
    if [ "$2" = "$3" ]; then
        say "PASS $1: $3"
    else
        say "FAIL $1: expected $2, got $3"
        failures=$((failures + 1))
    fi
}

cleanup() {
    [ -n "${ui_pid:-}" ] && kill "$ui_pid" 2>/dev/null || true
    [ -n "${node_pid:-}" ] && kill -TERM "$node_pid" 2>/dev/null || true
    wait 2>/dev/null || true
}
trap cleanup EXIT

# A helper that reads the window list, the main display and the lock state.
cat >"$work/bin/probe.swift" <<'EOF'
import CoreGraphics
import Foundation
let args = CommandLine.arguments
switch args.count > 1 ? args[1] : "" {
case "locked":
    let d = CGSessionCopyCurrentDictionary() as? [String: Any] ?? [:]
    print((d["CGSSessionScreenIsLocked"] as? Bool) == true ? "yes" : "no")
case "display":
    let b = CGDisplayBounds(CGMainDisplayID())
    print("\(Int(b.width)) \(Int(b.height))")
default:
    // id, on screen, x, y, width, height, title of each window of argv[2].
    let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as! [[String: Any]]
    for w in list where (w[kCGWindowOwnerName as String] as? String) == args[2] {
        let b = w[kCGWindowBounds as String] as? [String: Double] ?? [:]
        let on = (w[kCGWindowIsOnscreen as String] as? Bool) == true ? 1 : 0
        let name = w[kCGWindowName as String] as? String ?? ""
        print("\(w[kCGWindowNumber as String]!)\t\(on)\t\(Int(b["X"] ?? 0))\t\(Int(b["Y"] ?? 0))\t\(Int(b["Width"] ?? 0))\t\(Int(b["Height"] ?? 0))\t\(name)")
    }
}
EOF
# A plain grey window behind the panels for their screenshot, so it shows
# none of what is on screen. Takes x y w h (points from the top-left of the
# main display) and how many seconds to stay.
cat >"$work/bin/backdrop.swift" <<'EOF'
import AppKit
let a = CommandLine.arguments.dropFirst().map { Double($0)! }
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let top = NSScreen.screens[0].frame.height
let window = NSWindow(
    contentRect: NSRect(x: a[0], y: top - a[1] - a[3], width: a[2], height: a[3]),
    styleMask: .borderless, backing: .buffered, defer: false)
window.backgroundColor = NSColor(calibratedRed: 0.23, green: 0.26, blue: 0.31, alpha: 1)
window.level = .floating
window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
window.ignoresMouseEvents = true
window.orderFrontRegardless()
DispatchQueue.main.asyncAfter(deadline: .now() + a[4]) { app.terminate(nil) }
app.run()
EOF
swiftc -O "$work/bin/probe.swift" -o "$work/bin/probe" 2>/dev/null
swiftc -O "$work/bin/backdrop.swift" -o "$work/bin/backdrop" 2>/dev/null
probe() { "$work/bin/probe" "$@"; }

if [ "$(probe locked)" = yes ]; then
    say "the screen is locked; unlock it and run this again" >&2
    exit 1
fi

front() { osascript -e 'tell application "System Events" to name of first process whose frontmost is true'; }

# Sends one request to the node and prints the answer.
request() {
    python3 -c 'import json, socket, sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall((sys.argv[2] + "\n").encode())
print(s.makefile().readline().strip())' "$BRICOLO_DATA_DIR/bricolo.sock" "$1"
}

# Window lines of bricolo-ui: "<on> <x> <y> <w> <h>" for the window whose
# title is $1, largest first, so the panels' windows sort by size.
window() { probe windows bricolo-ui | sort -u | awk -F'\t' -v t="$1" '$7 == t { print $2, $3, $4, $5, $6 }'; }

# Crops the full screenshot $1 to x y w h (points) plus a margin, into $2.
crop() {
    local src=$1 out=$2 x=$3 y=$4 w=$5 h=$6 m=${7:-0}
    read -r dw _ < <(probe display)
    local pw scale
    pw=$(sips -g pixelWidth "$src" | awk '/pixelWidth/ { print $2 }')
    scale=$((pw / dw))
    x=$(((x - m) * scale)) y=$(((y - m) * scale)) w=$(((w + 2 * m) * scale)) h=$(((h + 2 * m) * scale))
    ((x < 0)) && x=0
    ((y < 0)) && y=0
    sips -c "$h" "$w" --cropOffset "$y" "$x" "$src" --out "$out" >/dev/null
    # Keep files small: one point per pixel.
    [ "$scale" -gt 1 ] && sips --resampleWidth $((w / scale)) "$out" >/dev/null
    say "wrote $out"
}

say "== building"
cargo build -q -p bricolo-node -p bricolo-ui
cargo build -q --target wasm32-wasip2 --release -p window-tiler -p caps

say "== setting up $work"
cat >"$BRICOLO_CONFIG_DIR/node.toml" <<'EOF'
name = "smoke"

[mesh]
relays = false

[sidecars.files]
command = "/usr/bin/python3"
args = ["-m", "http.server", "8799", "--bind", "127.0.0.1"]
cwd = "/tmp"
port = 8799
health_url = "http://127.0.0.1:8799/"
EOF
cat >"$BRICOLO_CONFIG_DIR/scripts/demo.lua" <<'EOF'
-- A tile with a switch and buttons, a caption and a toast.
local count = 0
local function tile(status)
  bricolo.ui.set_tile{
    title = "Demo", status = status,
    switches = { {id = "loud", label = "Speak results", on = false} },
    buttons = { {id = "count", label = "Count"}, {id = "pick", label = "Pick…"} },
    on_switch = function(id, on) bricolo.log.info("switch " .. id .. " -> " .. tostring(on)) end,
    on_button = function(id)
      count = count + 1
      tile("pressed " .. count .. " times")
    end,
  }
end
tile("idle")
bricolo.ui.caption("The quick brown fox jumps over the lazy dog")
bricolo.ui.toast("Demo script loaded", 600000)
bricolo.log.info("demo ready")
EOF
$bricolo install plugins/window-tiler -y >/dev/null
$bricolo install fixtures/caps -y >/dev/null

say "== starting the node"
$bricolo run >"$work/run.log" 2>&1 &
node_pid=$!
for _ in $(seq 1 50); do $bricolo status >/dev/null 2>&1 && break || sleep 0.2; done
$bricolo fire caps "tile:Caps fixture" >/dev/null
request '{"cmd":"sensor","plugin":"caps","sensor":"mic","active":true}' >/dev/null

say "== the hub"
$ui >"$work/ui.log" 2>&1 &
ui_pid=$!
sleep 5
read -r on x y w h < <(window bricolo | head -1)
check "hub on screen" 1 "$on"
screencapture -x "$work/full-home.png"
crop "$work/full-home.png" "$shots/hub-home.png" "$x" "$y" "$w" "$h"
# The panels, the toast above the caption, over a plain backdrop.
read -r _ cx cy cw ch < <(window "bricolo panel" | awk '$1 == 1' | sort -k4 -n -r | head -1)
read -r _ _ ty _ _ < <(window "bricolo panel" | awk '$1 == 1' | sort -k4 -n | head -1)
"$work/bin/backdrop" $((cx - 40)) $((ty - 40)) $((cw + 80)) $((cy + ch - ty + 80)) 3 &
sleep 1
screencapture -x "$work/full-panels.png"
wait $!
crop "$work/full-panels.png" "$shots/panels.png" "$cx" "$ty" "$cw" $((cy + ch - ty)) 24

for view in apps logs; do
    kill "$ui_pid"
    sleep 1
    $ui --view "$view" >>"$work/ui.log" 2>&1 &
    ui_pid=$!
    sleep 4
    read -r _ x y w h < <(window bricolo | head -1)
    screencapture -x "$work/full-$view.png"
    crop "$work/full-$view.png" "$shots/hub-$view.png" "$x" "$y" "$w" "$h"
done
request '{"cmd":"sensor","plugin":"caps","sensor":"mic","active":false}' >/dev/null

say "== focus"
# The hub may have taken the front; give it back to another app first.
if [ "$(front)" = bricolo-ui ]; then
    osascript -e 'tell application "Finder" to activate'
    sleep 1
fi
before=$(front)
say "in front: $before"
$bricolo fire caps "caption:Caption from the caps plugin" >/dev/null
sleep 1
check "front app after a caption" "$before" "$(front)"
$bricolo fire caps "review:Send the draft before noon, and copy Sam." >/dev/null
sleep 1
check "front app after a review panel" "$before" "$(front)"
read -r on rx ry rw bricolo_ < <(window "bricolo panel" | awk '$4 == 760 && $5 == 200 { print }')
check "review panel on screen" 1 "$on"
# Over a plain backdrop too, so whatever is behind stays out of the shot.
"$work/bin/backdrop" $((rx - 40)) $((ry - 40)) $((rw + 80)) $((bricolo_ + 80)) 3 &
sleep 1
screencapture -x "$work/full-review.png"
wait $!
crop "$work/full-review.png" "$shots/review.png" "$rx" "$ry" "$rw" "$bricolo_" 24
panel=$($bricolo logs --plugin caps -n 20 | grep -o 'review: Ok("p[0-9]*")' | tail -1 | grep -o 'p[0-9]*')
answer=$(request "{\"cmd\":\"ui-action\",\"action\":{\"kind\":\"review\",\"panel\":\"$panel\",\"accepted\":true,\"text\":\"edited\"}}")
check "review answer" '{"ok":true}' "$answer"
sleep 1
read -r on _ < <(window "bricolo panel" | awk '$4 == 760 && $5 == 200 { print }')
check "review panel hidden after the answer" 0 "$on"
check "front app after the answer" "$before" "$(front)"
$bricolo logs --plugin caps -n 5 | grep -q 'ReviewDone { panel: "'"$panel"'", accepted: true, text: "edited" }' &&
    say "PASS the plugin got the review" || {
    say "FAIL the plugin never got the review"
    failures=$((failures + 1))
}

if $save; then
    mkdir -p docs/screenshots
    cp "$shots"/*.png docs/screenshots/
    say "copied the screenshots to docs/screenshots/"
fi
say "screenshots: $shots"
[ "$failures" -eq 0 ] || exit 1
