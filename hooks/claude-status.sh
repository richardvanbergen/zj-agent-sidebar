#!/bin/sh
# Claude Code -> watcher status producer.
#
# Wire it up in ~/.claude/settings.json, e.g.
#   "hooks": {
#     "UserPromptSubmit":  [{ "hooks": [{ "type": "command",
#        "command": "sh ~/Code/zj-agent-state/hooks/claude-status.sh working" }] }],
#     "Notification":      [{ "hooks": [{ "type": "command",
#        "command": "sh ~/Code/zj-agent-state/hooks/claude-status.sh blocked" }] }],
#     "Stop":              [{ "hooks": [{ "type": "command",
#        "command": "sh ~/Code/zj-agent-state/hooks/claude-status.sh done" }] }]
#   }

set -eu

state="${1:-idle}"

[ -n "${ZELLIJ_PANE_ID:-}" ] || exit 0
command -v zellij >/dev/null 2>&1 || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

payload="$(cat 2>/dev/null || true)"

case "$payload" in
  *'"agent_id"'*) exit 0 ;;
esac

printf '%s' "$payload" | python3 -c '
import json, os, subprocess, sys

state = sys.argv[1]
try:
    data = json.load(sys.stdin)
except Exception:
    data = {}

GENERIC = {"Claude needs attention", "Claude Code needs your attention"}

def trailing_question(text):
    lines = [l.strip() for l in text.strip().split("\n") if l.strip()]
    return lines[-1] if lines and lines[-1].endswith("?") else None

status = state
msg = ""

if state == "done":
    msg = (data.get("last_assistant_message") or data.get("message") or "").strip()
    q = trailing_question(msg)
    if q:
        status, msg = "blocked", q
elif state == "blocked":
    msg = (data.get("message") or "").strip()
    if not msg or msg in GENERIC:
        sys.exit(0)
elif state == "working":
    msg = (data.get("prompt") or "").strip()

msg = " ".join(msg.split())[:160]
pane_id = os.environ.get("ZELLIJ_PANE_ID", "")
body = json.dumps({"pane_id": int(pane_id), "status": status, "agent": "claude", "message": msg})
subprocess.run(
    ["zellij", "pipe", "--name", "zj_agent_state.status.v1", "--", body],
    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
)
' "$state" || true
