#!/usr/bin/env bash
# The computer-use demo: an agent edits a document in a real app through
# the MCP gateway, using only the accessibility tree.
#
# It starts a bricolo node in a throwaway home, opens a text file in TextEdit in
# the background, and runs Claude Code headless with the gateway as its only
# MCP server and no other tools. The agent reads TextEdit's tree, replaces
# the text and saves through the File menu. The script then reads the file
# on disk: the demo passes only if the agent's edit is there.
#
#   scripts/demo-computer-use.sh
#
# macOS only, in an unlocked session; the program running it needs the
# Accessibility permission (bricolo permissions). Costs one agent run, capped at
# DEMO_BUDGET_USD (default 1); DEMO_MODEL picks the model; DEMO_ACTIVATE=1
# brings TextEdit to the front (see below). The transcript
# and the node's log stay in the folder it prints.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
MODEL="${DEMO_MODEL:-claude-sonnet-5-5}"
BUDGET="${DEMO_BUDGET_USD:-1}"
WANT="Hello from the bricolo MCP gateway"
[ "$(uname)" = Darwin ] || { echo "the demo drives TextEdit: macOS only"; exit 2; }
command -v claude >/dev/null || { echo "the claude CLI is not on PATH"; exit 2; }

(cd "$REPO" && cargo build -q -p bricolo-node) || exit 1
WORK="$(mktemp -d)"
mkdir -p "$WORK/bin" "$WORK/doc"
ln -s "$REPO/target/debug/bricolo" "$WORK/bin/bricolo"
export PATH="$WORK/bin:$PATH"
export BRICOLO_CONFIG_DIR="$WORK/home/config" BRICOLO_DATA_DIR="$WORK/home/data"
mkdir -p "$BRICOLO_CONFIG_DIR"
printf '[mesh]\nenabled = false\n' >"$BRICOLO_CONFIG_DIR/node.toml"
DOC="$WORK/doc/bricolo-demo.txt"
printf 'Replace me.\n' >"$DOC"

NODE=""
trap '[ -n "$NODE" ] && kill "$NODE" 2>/dev/null' EXIT
bricolo run >"$WORK/node.log" 2>&1 &
NODE=$!
for _ in $(seq 100); do bricolo status >/dev/null 2>&1 && break; sleep 0.1; done
bricolo status >/dev/null 2>&1 || { echo "the node did not start; see $WORK/node.log"; exit 1; }

# -g: TextEdit opens the file without coming to the front. macOS shows
# accessibility only the windows on the current Space, so when TextEdit's
# windows live on another Space (behind a full-screen app, say), run with
# DEMO_ACTIVATE=1: TextEdit comes to the front, on this Space.
if [ "${DEMO_ACTIVATE:-0}" = 1 ]; then
    open -a TextEdit "$DOC"
else
    open -g -a TextEdit "$DOC"
fi

cat >"$WORK/mcp.json" <<JSON
{"mcpServers": {"bricolo": {"command": "$WORK/bin/bricolo", "args": ["mcp"],
  "env": {"BRICOLO_CONFIG_DIR": "$BRICOLO_CONFIG_DIR", "BRICOLO_DATA_DIR": "$BRICOLO_DATA_DIR"}}}}
JSON

PROMPT="You can act on this computer only through the bricolo MCP tools.
TextEdit has a document named bricolo-demo.txt open. Using the accessibility
capability on the node 'local': find that window, read its UI tree, replace
the whole text of the document with exactly: $WANT
then save the document with TextEdit's File > Save menu item. Check the
verification each action returns, and use accessibility.wait if the app is
slow. Don't use any other app, and don't close or quit TextEdit. When done,
say in one line what you did and what the tree showed after saving."

echo "work folder: $WORK"
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
AGENT_EXIT=$?
echo "agent finished in $(( $(date +%s) - START )) s (exit $AGENT_EXIT)"

python3 - "$WORK/agent.json" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    print("no JSON from the agent"); sys.exit()
print(f"turns {d.get('num_turns')}, ${d.get('total_cost_usd', 0):.2f}, error={d.get('is_error')}")
print("agent: " + (d.get("result") or "").strip()[:1500])
PY

echo "gateway calls (audit log):"
bricolo logs 2>/dev/null | grep 'audit' | sed 's/^/  /' | tail -40

GOT="$(cat "$DOC")"
if [ "${GOT%$'\n'}" = "$WANT" ]; then
    echo "PASS  the file on disk holds the agent's text"
    exit 0
fi
echo "FAIL  the file on disk holds: $GOT"
exit 1
