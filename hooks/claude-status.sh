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
#
# POSIX shell + jq only. No python dependency: this hook runs on minimal
# Nix/SSH/sandboxed profiles where python3 is often absent.

set -eu

state="${1:-idle}"

[ -n "${ZELLIJ_PANE_ID:-}" ] || exit 0
command -v zellij >/dev/null 2>&1 || exit 0
command -v jq >/dev/null 2>&1 || exit 0

payload="$(cat 2>/dev/null || true)"

case "$payload" in
  *'"agent_id"'*) exit 0 ;;
esac

# Invalid or empty JSON -> {}
data="$(printf '%s' "$payload" | jq -c 'if type == "object" then . else {} end' 2>/dev/null || true)"
if [ -z "$data" ]; then data='{}'; fi

out="$(printf '%s' "$data" | jq -r --arg state "$state" '
  def trimmed: gsub("^\\s+|\\s+$"; "");
  def collapse: trimmed | gsub("\\s+"; " ") | .[0:160];
  def lastline:
    split("\n")
    | map(select(trimmed != ""))
    | map(trimmed)
    | last // "";
  . as $d | $state as $s |
  (if $s == "done" then
    (($d.last_assistant_message // $d.message // "") as $raw |
     ($raw | lastline) as $q |
     if $q | endswith("?") then
       {status: "blocked", message: ($q | collapse)}
     else
       {status: "done", message: ($raw | collapse)}
     end)
  elif $s == "blocked" then
    (($d.message // "") | collapse) as $m |
    if $m == "" or $m == "Claude needs attention"
       or $m == "Claude Code needs your attention"
    then empty
    else {status: "blocked", message: $m}
    end
  elif $s == "working" then
    {status: "working", message: (($d.prompt // "") | collapse)}
  else
    {status: $s, message: ""}
  end) | "\(.status)\n\(.message)"
')"

[ -n "$out" ] || exit 0

status="$(printf '%s\n' "$out" | sed -n '1p')"
message="$(printf '%s\n' "$out" | sed -n '2p')"

body="$(jq -cn --arg pane "$ZELLIJ_PANE_ID" --arg status "$status" --arg message "$message" \
  '{pane_id: ($pane | tonumber), status: $status, agent: "claude", message: $message}')"

zellij pipe --name zj_agent_state.status.v1 -- "$body" >/dev/null 2>&1 || true
