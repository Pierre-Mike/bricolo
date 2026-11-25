#!/usr/bin/env bash
# The computer-use demo on Linux, for any machine with Docker: an agent
# answers a GTK dialog through the MCP gateway, using only the
# accessibility tree (AT-SPI).
#
# The container (scripts/linux-test/) runs a desktop on Xvfb, a bricolo node
# with its HTTP gateway, and `zenity --entry`. Claude Code runs here, on the
# host, with that gateway as its only MCP server and no other tools. It
# reads the dialog's tree, types the text and presses OK. zenity prints
# what was typed; the demo passes only if that is the agent's text.
#
#   scripts/demo-computer-use-linux.sh
#
# Costs one agent run, capped at DEMO_BUDGET_USD (default 1); DEMO_MODEL
# picks the model. The gateway is published on 127.0.0.1:8790 only, with a
# token. The node's log and the transcript stay in the folder it prints.
# The container keeps its files in /demo-out, read with docker exec and cp.
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.local/bin:$PATH"
image="${BRICOLO_LINUX_IMAGE:-bricolo-linux-test}"
MODEL="${DEMO_MODEL:-claude-sonnet-5-5}"
BUDGET="${DEMO_BUDGET_USD:-1}"
WANT="hello from the bricolo gateway"
command -v claude >/dev/null || { echo "the claude CLI is not on PATH"; exit 2; }

context="$(mktemp -d)"
cp "$root/scripts/linux-test/Dockerfile" "$root/rust-toolchain.toml" "$context/"
docker build -q -t "$image" "$context" >/dev/null || exit 1
rm -rf "$context"

# The container's /demo-out is read with docker exec, not a bind mount:
# Docker Desktop shares only the folders its settings list.
WORK="$(mktemp -d)"
name="bricolo-demo-$$"
trap 'docker rm -f "$name" >/dev/null 2>&1' EXIT
docker run -d --name "$name" -p 127.0.0.1:8790:8790 \
    -v "$root:/src" \
    -v "${BRICOLO_LINUX_TARGET:-bricolo-linux-target}:/src/target" \
    -v bricolo-linux-registry:/usr/local/cargo/registry \
    -e CARGO_INCREMENTAL=0 -e CARGO_TERM_COLOR=never -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" \
    "$image" bash /src/scripts/linux-test/inside.sh demo >/dev/null || exit 1

echo "work folder: $WORK"
echo "building bricolo in the container and starting the desktop ..."
for _ in $(seq 3600); do
    docker exec "$name" test -f /demo-out/ready 2>/dev/null && break
    docker inspect -f '{{.State.Running}}' "$name" 2>/dev/null | grep -q true || break
    sleep 1
done
docker exec "$name" test -f /demo-out/ready 2>/dev/null || { docker logs "$name" 2>&1 | tail -30; echo "the container did not get ready"; exit 1; }
TOKEN="$(docker exec "$name" cat /demo-out/data/mcp.token)"

cat >"$WORK/mcp.json" <<JSON
{"mcpServers": {"bricolo": {"type": "http", "url": "http://127.0.0.1:8790/mcp",
  "headers": {"Authorization": "Bearer $TOKEN"}}}}
JSON

PROMPT="You can act on the computer only through the bricolo MCP tools. On the node
'local', a dialog titled 'bricolo demo' (the app zenity) asks 'What should the node
write?'. Using the accessibility capability: find the app, read the dialog's
UI tree, put exactly this text in its entry: $WANT
then press the dialog's OK button. Check the verification each action
returns. When done, say in one line what you did."

echo "running claude ($MODEL) with only the bricolo tools ..."
START=$(date +%s)
printf '%s' "$PROMPT" | claude -p \
    --model "$MODEL" \
    --output-format json \
    --strict-mcp-config --mcp-config "$WORK/mcp.json" \
    --allowedTools "mcp__bricolo" \
    --disallowedTools "Bash Read Write Edit Glob Grep WebFetch WebSearch Task" \
    --setting-sources project \
    --no-session-persistence \
    --max-budget-usd "$BUDGET" \
    >"$WORK/agent.json" 2>"$WORK/agent.err"
echo "agent finished in $(( $(date +%s) - START )) s (exit $?)"

python3 - "$WORK/agent.json" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    print("no JSON from the agent"); sys.exit()
print(f"turns {d.get('num_turns')}, ${d.get('total_cost_usd', 0):.2f}, error={d.get('is_error')}")
print("agent: " + (d.get("result") or "").strip()[:1500])
PY

# The container stops once the dialog closes; docker cp reads it either way.
for _ in $(seq 20); do docker exec "$name" test -f /demo-out/done 2>/dev/null && break; sleep 0.5; done
if docker exec "$name" test -f /demo-out/done 2>/dev/null; then
    docker wait "$name" >/dev/null 2>&1
else
    docker stop -t 2 "$name" >/dev/null 2>&1
fi
docker cp "$name:/demo-out/node.log" "$WORK/node.log" 2>/dev/null
docker cp "$name:/demo-out/answer.txt" "$WORK/answer.txt" 2>/dev/null
echo "gateway calls (audit log):"
grep 'audit' "$WORK/node.log" | sed 's/^.*capability call /  /' | tail -40

GOT="$(cat "$WORK/answer.txt" 2>/dev/null)"
if [ "$GOT" = "$WANT" ]; then
    echo "PASS  zenity returned the agent's text: $GOT"
    exit 0
fi
echo "FAIL  zenity returned: ${GOT:-nothing}"
exit 1
