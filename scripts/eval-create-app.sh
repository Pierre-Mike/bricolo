#!/usr/bin/env bash
# End-to-end eval for the create-bricolo-app skill. Starts a bricolo node in a
# throwaway Bricolo home, runs a headless coding agent with the skill on a
# one-line request in an empty folder, then checks deterministically that
# exactly one new app (script or plugin) is running in that node, and,
# when the request asks for a sidecar, that a sidecar is ready and the app
# calls its port.
#
#   scripts/eval-create-app.sh                                  # claude, the default request
#   scripts/eval-create-app.sh "a plugin that ..."              # any request
#   scripts/eval-create-app.sh --agent gemini "a Lua script ..."
#
# --agent picks the CLI: claude (Claude Code, the default), gemini (Gemini
# CLI) or codex (Codex CLI). It must be on PATH and logged in. The skill is
# copied where that agent reads project skills (.claude/skills or
# .agents/skills), and the prompt also names its path, so an agent that
# skips skill discovery can still read it. Costs one agent run. Claude
# stops at EVAL_BUDGET_USD (default 3); EVAL_MODEL picks the model. The
# node binds the app's chords while it runs: hotkeys are global.
set -uo pipefail

AGENT=claude
if [ "${1:-}" = "--agent" ]; then
    AGENT="${2:-}"
    shift 2
fi
case "$AGENT" in
    claude) MODEL="${EVAL_MODEL:-claude-sonnet-5-5}" SKILLS=".claude/skills" ;;
    gemini) MODEL="${EVAL_MODEL:-}" SKILLS=".agents/skills" ;;
    codex) MODEL="${EVAL_MODEL:-}" SKILLS=".agents/skills" ;;
    *) echo "--agent must be claude, gemini or codex"; exit 2 ;;
esac
REQUEST="${*:-a Lua script that shows a notification with the clipboard text when I press ctrl+alt+shift+v}"
BUDGET="${EVAL_BUDGET_USD:-3}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
command -v "$AGENT" >/dev/null || { echo "the $AGENT CLI is not on PATH"; exit 2; }

(cd "$REPO" && cargo build -q -p bricolo-node) || exit 1
WORK="$(mktemp -d)"
mkdir -p "$WORK/bin" "$WORK/ws/$SKILLS"
ln -s "$REPO/target/debug/bricolo" "$WORK/bin/bricolo"
cp -R "$REPO/skills/create-bricolo-app" "$WORK/ws/$SKILLS/create-bricolo-app"
rm -rf "$WORK/ws/$SKILLS/create-bricolo-app/evals"
export PATH="$WORK/bin:$PATH"
export BRICOLO_CONFIG_DIR="$WORK/home/config" BRICOLO_DATA_DIR="$WORK/home/data"
mkdir -p "$BRICOLO_CONFIG_DIR/scripts"
printf '[mesh]\nenabled = false\n' >"$BRICOLO_CONFIG_DIR/node.toml"

FAILED=0
pass() { echo "PASS  $1"; }
fail() { echo "FAIL  $1"; FAILED=1; }
NODE=""
trap '[ -n "$NODE" ] && kill "$NODE" 2>/dev/null' EXIT

bricolo run >"$WORK/node.log" 2>&1 &
NODE=$!
for _ in $(seq 100); do bricolo status >/dev/null 2>&1 && break; sleep 0.1; done
bricolo status >/dev/null 2>&1 || { echo "the node did not start; see $WORK/node.log"; exit 1; }

PROMPT="Use the create-bricolo-app skill ($SKILLS/create-bricolo-app/SKILL.md in this folder). Build this: ${REQUEST}.
I approve the capabilities, chords, sidecars and ports it needs in advance; do not wait for me.
A bricolo node is already running for this task: BRICOLO_CONFIG_DIR and BRICOLO_DATA_DIR in
the environment point at its home, and \`bricolo\` is on PATH. Do not start or stop
a node. Write any sidecar inside this folder. Leave the app installed and running."

echo "request: $REQUEST"
echo "running $AGENT in $WORK/ws (model ${MODEL:-default}) ..."
START=$(date +%s)
case "$AGENT" in
    claude)
        (cd "$WORK/ws" && printf '%s' "$PROMPT" | claude -p \
            --model "$MODEL" \
            --output-format json \
            --permission-mode bypassPermissions \
            --allowedTools "Bash Read Write Edit Glob Grep Skill" \
            --setting-sources project \
            --no-session-persistence \
            --max-budget-usd "$BUDGET" \
            >"$WORK/agent.json" 2>"$WORK/agent.err")
        ;;
    gemini)
        (cd "$WORK/ws" && gemini --prompt "$PROMPT" --approval-mode yolo \
            --output-format json ${MODEL:+--model "$MODEL"} \
            </dev/null >"$WORK/agent.json" 2>"$WORK/agent.err")
        ;;
    codex)
        (cd "$WORK/ws" && codex exec --skip-git-repo-check \
            --dangerously-bypass-approvals-and-sandbox --json \
            ${MODEL:+--model "$MODEL"} "$PROMPT" \
            </dev/null >"$WORK/agent.json" 2>"$WORK/agent.err")
        ;;
esac
AGENT_EXIT=$?
SECONDS_TAKEN=$(( $(date +%s) - START ))

python3 - "$AGENT" "$WORK/agent.json" "$SECONDS_TAKEN" <<'PY'
import json, sys
agent, path, secs = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(path, errors="replace").read()
try:
    d = json.loads(text)
except Exception:
    # codex --json and partial output: JSON lines; keep the last object.
    d = {}
    for line in text.splitlines():
        try:
            d = json.loads(line)
        except Exception:
            pass
if agent == "claude":
    print(f"agent: {secs} s, {d.get('num_turns')} turns, "
          f"${d.get('total_cost_usd', 0):.2f}, error={d.get('is_error')}")
    result = d.get("result") or ""
elif agent == "gemini":
    models = (d.get("stats") or {}).get("models") or {}
    tokens = sum(((m.get("tokens") or {}).get("total") or 0) for m in models.values())
    print(f"agent: {secs} s, models {', '.join(models) or '?'}, {tokens} tokens, "
          f"error={d.get('error')}")
    result = d.get("response") or ""
else:
    print(f"agent: {secs} s")
    result = json.dumps(d)[:3000]
print("agent's last message:")
print("  " + result.strip().replace("\n", "\n  ")[:3000])
PY
[ "$AGENT_EXIT" = 0 ] && pass "agent exited 0" || fail "agent exited $AGENT_EXIT (see $WORK/agent.err)"

bricolo status >"$WORK/status.json" 2>/dev/null || fail "bricolo status after the agent"
CHECK=$(python3 - "$WORK/status.json" "$REQUEST" "$BRICOLO_CONFIG_DIR" "$WORK/ws" <<'PY'
import json, re, sys, pathlib
status = json.load(open(sys.argv[1]))
request, config, ws = sys.argv[2], pathlib.Path(sys.argv[3]), pathlib.Path(sys.argv[4])
apps = [("plugin", p) for p in status.get("plugins", [])] + \
       [("script", s) for s in status.get("scripts", [])]
sidecars = status.get("sidecars", [])
out = []
def say(ok, what):
    out.append(("PASS  " if ok else "FAIL  ") + what)
say(len(apps) == 1, f"exactly one app installed ({', '.join(k + ':' + a['name'] for k, a in apps) or 'none'})")
sources = {}
for kind, app in apps:
    say(app["state"] == "running", f"{kind} {app['name']} is running ({app['state']}, error={app.get('error')})")
    if kind == "script":
        sources[app["name"]] = (config / "scripts" / f"{app['name']}.lua").read_text()
    else:
        manifest = config / "plugins" / app["name"] / "plugin.toml"
        sources[app["name"]] = manifest.read_text() if manifest.exists() else ""
    chord = re.search(r"\b((?:cmd|ctrl|alt|shift|option)(?:\+[a-z0-9]+)+)", request)
    if chord:
        chord = chord.group(1)
        if kind == "plugin":
            bound = list((app.get("hotkeys") or {}).values())
            say(chord in bound, f"{app['name']} binds {chord} (has {bound})")
        else:
            text = sources[app["name"]]
            mods = chord.split("+")[:-1]
            spread = r",\s*".join(f'"{m}"' for m in mods)
            found = chord in text or re.search(spread, text) is not None
            say(found, f"{app['name']}.lua binds {chord}")
    if kind == "plugin":
        for binding in (app.get("hotkeys") or {}):
            out.append(f"FIRE  {app['name']} {binding}")
    out.append(f"NAME  {app['name']}{'.lua' if kind == 'script' else ''}")
if "sidecar" in request.lower():
    say(len(sidecars) >= 1, f"a sidecar is registered ({', '.join(s['name'] for s in sidecars) or 'none'})")
    for s in sidecars:
        say(s["state"] == "ready", f"sidecar {s['name']} is ready on port {s['port']} ({s['state']}, error={s.get('error')})")
        calls = any(f":{s['port']}" in text for text in sources.values())
        say(calls, f"the app calls 127.0.0.1:{s['port']}")
        out.append(f"SIDECAR  {s['name']}")
print("\n".join(out))
PY
)
echo "$CHECK" | grep -E '^(PASS|FAIL)'
echo "$CHECK" | grep -q '^FAIL' && FAILED=1
while read -r _ plugin binding; do
    bricolo fire "$plugin" "$binding" >/dev/null 2>&1 && pass "bricolo fire $plugin $binding" || fail "bricolo fire $plugin $binding"
done < <(echo "$CHECK" | grep '^FIRE')
sleep 1
for name in $(echo "$CHECK" | awk '/^NAME/ {print $2}'); do
    echo "log lines for $name:"
    bricolo logs --plugin "$name" -n 10 | sed 's/^/  /'
    if bricolo logs --plugin "$name" -n 200 | grep -q " ERROR "; then
        fail "$name logged an error"
    else
        pass "$name logged no error"
    fi
done
for name in $(echo "$CHECK" | awk '/^SIDECAR/ {print $2}'); do
    echo "sidecar $name output:"
    bricolo sidecar logs "$name" -n 10 2>&1 | sed 's/^/  /'
done

echo "workspace kept at $WORK (ws/, home/, agent.json, node.log)"
exit $FAILED
