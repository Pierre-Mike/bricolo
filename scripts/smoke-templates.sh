#!/usr/bin/env bash
# Every `bricolo new`, `bricolo new-script` and `bricolo new-sidecar` template, end to
# end, outside this repository: generate it in a temp folder, `cargo test`
# its domain logic, build it for wasm32-wasip2, `bricolo check` it, install it
# into a temp Bricolo home, then start `bricolo run` and check that `bricolo status`
# shows every plugin and script running and every sidecar ready. `bricolo fire`
# then sends each plugin one binding.
#
# Sidecars: each template is scaffolded on a free port, tested, and
# registered with `bricolo sidecar add`; the Rust one after the node started,
# which exercises the live reload. The sidecar-client plugin calls the
# Python sidecar, and the sidecar-client script calls the Node.js one
# (the Python one when node is missing); both must log the answer.
#
#   scripts/smoke-templates.sh            all templates
#   KEEP=1 scripts/smoke-templates.sh     keep the temp folder
#
# Hotkeys are global: while the node runs (a few seconds) it binds the
# templates' chords. Builds use the network for crates the cache lacks.
# The node records through a fake ffmpeg, so the dictation template never
# opens the microphone.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

cargo build -q -p bricolo-node
bricolo="$PWD/target/debug/bricolo"
work="$(mktemp -d)"
[ -n "${KEEP:-}" ] || trap 'kill "${node:-}" 2>/dev/null || true; rm -rf "$work"' EXIT
export BRICOLO_CONFIG_DIR="$work/config" BRICOLO_DATA_DIR="$work/data"
mkdir -p "$BRICOLO_CONFIG_DIR" "$work/apps"
cat >"$work/ffmpeg" <<'SH'
#!/bin/sh
trap 'exit 255' INT
for a in "$@"; do out="$a"; done
{ printf 'RIFF\0\0\0\0WAVEdata\0\0\0\0'; head -c 3200 /dev/zero; } >"$out"
while :; do sleep 0.1; done
SH
chmod +x "$work/ffmpeg"
printf '[mesh]\nenabled = false\n[mic]\nffmpeg = "%s"\n' "$work/ffmpeg" >"$BRICOLO_CONFIG_DIR/node.toml"

plugins=$("$bricolo" new --list | awk '{print $1}')
scripts=$("$bricolo" new-script --list | awk '{print $1}')
sidecars=$("$bricolo" new-sidecar --list | awk '{print $1}')
command -v node >/dev/null || sidecars=$(echo "$sidecars" | grep -vx node)

free_port() { python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'; }
# macOS ships bash 3.2, without associative arrays: one file per sidecar.
mkdir -p "$work/sc"
port() { cat "$work/sc/$1.port" 2>/dev/null; }
register() { eval "\"\$bricolo\" $(cat "$work/sc/$1.add")"; }
for t in $sidecars; do
    free_port >"$work/sc/$t.port"
    echo "== sidecar template $t (port $(port "$t"))"
    "$bricolo" new-sidecar "sc-$t" --template "$t" --port "$(port "$t")" --dir "$work/sidecars" |
        sed -n 's/^register: bricolo //p' >"$work/sc/$t.add"
    (
        cd "$work/sidecars/sc-$t"
        case "$t" in
            python) python3 -m unittest 2>&1 | tail -1 | sed 's/^/   /' ;;
            node)
                node --test >"$work/sc/node-test.log" 2>&1 || { cat "$work/sc/node-test.log"; exit 1; }
                echo "   node --test: $(grep -E '(pass|fail) [0-9]+$' "$work/sc/node-test.log" | sed 's/^[^a-z]*//' | tr '\n' ' ')"
                ;;
            rust)
                cargo test -q 2>&1 | grep -E "^test result" | sed 's/^/   /'
                cargo build -q --release
                ;;
        esac
    )
    if [ "$t" != rust ]; then
        register "$t" | sed 's/^/   /'
    fi
done
script_port=$(port node || port python)

for t in $plugins; do
    name="t-$t"
    echo "== plugin template $t"
    "$bricolo" new "$name" --template "$t" --dir "$work/apps" --port "$(port python)" >/dev/null
    (
        cd "$work/apps/$name"
        cargo test -q 2>&1 | grep -E "^test result" | sed 's/^/   /'
        cargo build -q --target wasm32-wasip2 --release
        "$bricolo" check . >/dev/null
        "$bricolo" install . -y | tail -1 | sed 's/^/   /'
    )
done
for t in $scripts; do
    echo "== script template $t"
    "$bricolo" new-script "s-$t" --template "$t" --port "$script_port" | head -1 | sed 's/^/   /'
    "$bricolo" check "$BRICOLO_CONFIG_DIR/scripts/s-$t.lua" | sed 's/^/   /'
done

"$bricolo" run >"$work/run.log" 2>&1 &
node=$!
for _ in $(seq 100); do
    "$bricolo" status >/dev/null 2>&1 && break
    sleep 0.1
done
sleep 2

echo "== bricolo sidecar add sc-rust while the node runs"
register rust | sed 's/^/   /'
ready=""
for _ in $(seq 120); do
    ready=$("$bricolo" status | python3 -c 'import json,sys; s=json.load(sys.stdin)["sidecars"]; print(" ".join(sorted(x["name"] for x in s if x["state"]=="ready")))')
    [ "$(echo "$ready" | wc -w)" -eq "$(echo $sidecars | wc -w)" ] && break
    sleep 0.5
done
echo
"$bricolo" sidecar list
for t in $sidecars; do
    answer=$(curl -s --max-time 5 --data hello "127.0.0.1:$(port "$t")/process" || true)
    echo "   curl sc-$t /process: $answer"
done

"$bricolo" status >"$work/status.json"
fail=0
python3 - "$work/status.json" $plugins -- $scripts -- $sidecars <<'PY' || fail=1
import json, sys
status = json.load(open(sys.argv[1]))
args = sys.argv[2:]
split = args.index("--")
split2 = args.index("--", split + 1)
plugins = {p["name"]: p for p in status["plugins"]}
scripts = {s["name"]: s for s in status["scripts"]}
sidecars = {s["name"]: s for s in status["sidecars"]}
bad = 0
print()
print(f"{'kind':<7} {'name':<16} {'state':<8} detail")
for t in args[:split]:
    p = plugins.get(f"t-{t}", {})
    state = p.get("state", "missing")
    detail = ", ".join(f"{k}={v}" for k, v in (p.get("hotkeys") or {}).items()) or (p.get("error") or "")
    print(f"{'plugin':<7} {'t-' + t:<16} {state:<8} {detail}")
    bad += state != "running"
for t in args[split + 1:split2]:
    s = scripts.get(f"s-{t}", {})
    state = s.get("state", "missing")
    print(f"{'script':<7} {'s-' + t:<16} {state:<8} {s.get('error') or ''}")
    bad += state != "running"
for t in args[split2 + 1:]:
    s = sidecars.get(f"sc-{t}", {})
    state = s.get("state", "missing")
    print(f"{'sidecar':<7} {'sc-' + t:<16} {state:<8} port {s.get('port')} {s.get('error') or ''}")
    bad += state != "ready"
sys.exit(1 if bad else 0)
PY

echo
"$bricolo" fire t-hotkey-tool show-clipboard
"$bricolo" fire t-window-tool center
"$bricolo" fire t-voice transcribe-latest
"$bricolo" fire t-http-service check-now
"$bricolo" fire t-dictation toggle
"$bricolo" fire t-sidecar-client process-clipboard
# The fake ffmpeg can take a few seconds to start the first time macOS sees it.
sensors() { "$bricolo" status | python3 -c 'import json,sys; print(json.load(sys.stdin)["active_sensors"])'; }
for _ in $(seq 50); do [ "$(sensors)" != "[]" ] && break; sleep 0.2; done
echo "active_sensors while recording: $(sensors)"
"$bricolo" fire t-dictation toggle
for _ in $(seq 50); do [ "$(sensors)" = "[]" ] && break; sleep 0.2; done
sleep 1
echo "active_sensors after the second toggle: $(sensors)"
echo
echo "\$ bricolo logs -n 40 (template lines)"
"$bricolo" logs -n 300 | grep -E "plugin=(t-|s-)|t-[a-z-]+|s-[a-z-]+\.lua|sidecar" | tail -40 || true

# Both sidecar clients reached their sidecar. The script waits for /health
# on a 2 s timer, so give it a moment.
for _ in $(seq 30); do
    "$bricolo" logs --plugin s-sidecar-client.lua -n 50 | grep -q "sample answer: HELLO FROM S-SIDECAR-CLIENT" && break
    sleep 0.5
done
echo
if "$bricolo" logs --plugin t-sidecar-client -n 50 | grep -q "sidecar answered: "; then
    echo "sidecar-client plugin: $("$bricolo" logs --plugin t-sidecar-client -n 50 | grep -o 'sidecar answered: .*' | tail -1)"
else
    echo "FAIL: the sidecar-client plugin logged no answer"; fail=1
fi
if "$bricolo" logs --plugin s-sidecar-client.lua -n 50 | grep -q "sample answer: HELLO FROM S-SIDECAR-CLIENT"; then
    echo "sidecar-client script: $("$bricolo" logs --plugin s-sidecar-client.lua -n 50 | grep -o 'sidecar ready.*' | tail -1)"
else
    echo "FAIL: the sidecar-client script logged no answer"; fail=1
fi

kill "$node"
wait "$node" 2>/dev/null || true
if [ "$fail" = 0 ]; then
    echo
    echo "PASS: every template is running, every sidecar is ready and both clients reached theirs"
else
    echo "FAIL: see $work/run.log"
    exit 1
fi
