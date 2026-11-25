#!/usr/bin/env bash
# Renders the hub's documentation screenshots from the dev harness with
# headless Chrome: no node, no window on screen, the same picture each run.
#   crates/ui/dev/shots.sh [out-dir]     default docs/screenshots
# The panels' screenshots (panels.png, review.png) come from the real app:
# scripts/ui-smoke.sh --save.
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
out="${1:-$root/docs/screenshots}"
chrome="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
port=8779

python3 "$root/crates/ui/dev/serve.py" "$port" >/dev/null 2>&1 &
server=$!
trap 'kill $server 2>/dev/null' EXIT
for _ in $(seq 50); do curl -sf "http://127.0.0.1:$port/dev/hub.html" >/dev/null && break; sleep 0.1; done

shot() {
  local name="$1" query="$2"
  "$chrome" --headless=new --disable-gpu --hide-scrollbars --force-dark-mode \
    --force-device-scale-factor=1 --window-size=1040,700 --virtual-time-budget=4000 \
    --user-data-dir="$(mktemp -d)" \
    --screenshot="$out/$name.png" "http://127.0.0.1:$port/dev/hub.html?quiet=1&$query" 2>/dev/null
  echo "$out/$name.png"
}

shot hub-home "view=home"
shot hub-apps "view=apps"
shot hub-settings "view=apps&open=plugin:voice-agent&tab=settings&edit=1"
shot hub-logs "view=logs"
