#!/bin/sh
# Codex -> watcher status producer. Codex fires the SAME command for every
# wired event (no per-event arg like Claude's settings.json hooks), so this
# reads hook_event_name from the JSON on stdin instead of $1.
#
# Wire it into $CODEX_HOME/hooks.json (default ~/.codex/hooks.json):
#   { "hooks": { "UserPromptSubmit": [{ "hooks": [{ "type": "command",
#       "command": "sh ~/Code/zj-agent-state/hooks/codex-status.sh",
#       "timeout": 10 }] }], ... same for PreToolUse, PermissionRequest,
#       PostToolUse, SubagentStart, SubagentStop, Stop } }
# Then run /hooks inside Codex once to trust it.

set -eu

[ -n "${ZELLIJ_PANE_ID:-}" ] || exit 0
command -v zellij >/dev/null 2>&1 || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

payload="$(cat 2>/dev/null || true)"

printf '%s' "$payload" | python3 -c '
import json, os, subprocess, sys

try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)

if data.get("transcript_path", "unset") is None:
    sys.exit(0)

event = data.get("hook_event_name")

def trailing_question(text):
    lines = [l.strip() for l in text.strip().split("\n") if l.strip()]
    return lines[-1] if lines and lines[-1].endswith("?") else None

status = None
msg = ""

if event == "UserPromptSubmit":
    status, msg = "working", (data.get("prompt") or "").strip()
elif event in ("PreToolUse", "PostToolUse"):
    status, msg = "working", data.get("tool_name") or "working"
elif event == "PermissionRequest":
    status = "blocked"
    msg = (data.get("tool_input") or {}).get("description") or data.get("message") or "approval requested"
elif event == "SubagentStart":
    status, msg = "working", "delegating"
elif event == "SubagentStop":
    status, msg = "working", data.get("last_assistant_message") or "delegating"
elif event == "Stop":
    msg = (data.get("last_assistant_message") or "").strip()
    q = trailing_question(msg)
    status = "blocked" if q else "done"
    msg = q or msg

if status is None:
    sys.exit(0)

msg = " ".join(str(msg).split())[:160]
pane_id = os.environ.get("ZELLIJ_PANE_ID", "")
body = json.dumps({"pane_id": int(pane_id), "status": status, "agent": "codex", "message": msg})
subprocess.run(
    ["zellij", "pipe", "--name", "zj_agent_state.status.v1", "--", body],
    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
)
' || true
