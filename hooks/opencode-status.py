#!/usr/bin/env python3
# Classifies opencode-bridge.js's per-event payload into our status.v1 wire
# shape and sends it. The bridge already knows which status class an event
# belongs to (running/pending/done/error/idle, zj-radar's vocabulary); this
# only maps that to our vocabulary and shapes the message per event.

import json
import os
import subprocess
import sys

STATUS_MAP = {
    "running": "working",
    "pending": "blocked",
    "done": "done",
    "error": "error",
    "idle": "idle",
}


def trailing_question(text):
    lines = [l.strip() for l in text.strip().split("\n") if l.strip()]
    return lines[-1] if lines and lines[-1].endswith("?") else None


def main():
    bridge_status = sys.argv[2] if len(sys.argv) > 2 and sys.argv[1] == "--status" else "idle"
    try:
        data = json.load(sys.stdin)
    except Exception:
        data = {}

    pane_id = os.environ.get("ZELLIJ_PANE_ID", "")
    if not pane_id:
        return

    event = data.get("event", "")
    status = STATUS_MAP.get(bridge_status, "unknown")
    msg = ""

    if event == "chat.message":
        msg = data.get("prompt", "")
    elif event == "tool.execute":
        msg = data.get("tool") or "working"
    elif event in ("permission.ask", "question.ask"):
        msg = data.get("message", "")
    elif event == "session.idle":
        msg = (data.get("message") or "").strip()
        q = trailing_question(msg)
        if q:
            status, msg = "blocked", q
    elif event == "session.error":
        msg = data.get("message", "")

    msg = " ".join(str(msg).split())[:160]
    body = json.dumps({"pane_id": int(pane_id), "status": status, "agent": "opencode", "message": msg})
    subprocess.run(
        ["zellij", "pipe", "--name", "zj_agent_state.status.v1", "--", body],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )


if __name__ == "__main__":
    try:
        main()
    except Exception:
        pass
