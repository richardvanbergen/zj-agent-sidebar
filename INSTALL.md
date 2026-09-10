Manual, on purpose — no script touches your `config.kdl` or any agent's
config on your behalf. You're a technical user; you already know how to
paste a block into a file you own. Automating that is how a tool ends up
silently clobbering something you wrote yourself.

## Prerequisites

- Rust + `rustup target add wasm32-wasip1`
- Zellij ≥ 0.45 (older than 0.44.3 won't load the wasm at all)
- python3 (all three hook scripts use it)
- macOS: the chime uses `afplay` (built in). On Linux, edit
  `CHIME_COMMAND` in `watcher/src/main.rs` to `paplay`/`aplay` and rebuild.

## Build

```sh
git clone <this repo> ~/Code/zj-agent-state   # or wherever you like
cd ~/Code/zj-agent-state
cargo build --release --target wasm32-wasip1 -p watcher
cargo build --release -p viewer
```

That's it — two binaries, `target/wasm32-wasip1/release/watcher.wasm` and
`target/release/viewer`. Nothing gets copied anywhere automatically.

## Install the plugin

```sh
mkdir -p ~/.config/zellij/plugins
cp target/wasm32-wasip1/release/watcher.wasm \
   ~/.config/zellij/plugins/zj-agent-state-watcher.wasm
```

Re-run that `cp` after every rebuild. Zellij caches compiled plugins by file
path, not content, but `viewer` always spawns it with `--skip-plugin-cache`,
so a stale copy is never actually a problem once the file itself is updated.

`viewer` itself is *not* installed anywhere — point the keybind below
straight at `target/release/viewer` in your clone. Rebuilding overwrites it
in place; there's no separate "install" step for it.

## Nothing to add to the layout

No `viewer` pane declared anywhere in your layout — one keybind, described
below, opens and closes it.

## Keybind

`viewer` is its own toggle, in-process — no wrapper script. On launch it
asks Zellij (`zellij action list-panes --all --json`) whether a pane titled
`zj-agents` already exists anywhere in the session. If one does, it closes
that pane and exits immediately instead of opening a second one. If not, it
renames its own pane to `zj-agents` (`zellij action rename-pane`, called
from inside itself — an OSC-2 title escape looked like the more obvious way
to do this and didn't actually get picked up by Zellij's `list-panes`, so
this is the one that's actually verified working) and starts normally.

If you don't already have a `keybinds` block, you'll need one (see Zellij's
own docs). Inside a `shared_except "locked"` block:

```kdl
bind "Alt a" {
    Run "/home/you/Code/zj-agent-state/target/release/viewer" {
        close_on_exit true
        name "agents"
    }
}
```

Use the real absolute path to your clone — a keybind's `Run` command is
**not** `~`-expanded (verified against Zellij's own KDL parser: only a
layout pane's `command=` attribute gets `shellexpand`, keybind actions
don't).

## Wire up agent hooks

Only wire the agents you actually use.

**Claude Code** — add to `~/.claude/settings.json` (merge with whatever's
already there, don't replace the file):

```json
{
  "hooks": {
    "UserPromptSubmit": [{ "hooks": [{ "type": "command",
      "command": "sh ~/Code/zj-agent-state/hooks/claude-status.sh working" }] }],
    "Notification": [{ "hooks": [{ "type": "command",
      "command": "sh ~/Code/zj-agent-state/hooks/claude-status.sh blocked" }] }],
    "Stop": [{ "hooks": [{ "type": "command",
      "command": "sh ~/Code/zj-agent-state/hooks/claude-status.sh done" }] }]
  }
}
```

(`~` is fine here — Claude Code runs hook commands through a shell.)

**Codex** — add a hook group per event to `$CODEX_HOME/hooks.json` (default
`~/.codex/hooks.json`), additive alongside anything already there:

```json
{
  "hooks": {
    "UserPromptSubmit": [{ "hooks": [{ "type": "command",
      "command": "sh ~/Code/zj-agent-state/hooks/codex-status.sh", "timeout": 10 }] }],
    "PreToolUse":       [{ "hooks": [{ "type": "command",
      "command": "sh ~/Code/zj-agent-state/hooks/codex-status.sh", "timeout": 10 }] }],
    "PermissionRequest":[{ "hooks": [{ "type": "command",
      "command": "sh ~/Code/zj-agent-state/hooks/codex-status.sh", "timeout": 10 }] }],
    "PostToolUse":      [{ "hooks": [{ "type": "command",
      "command": "sh ~/Code/zj-agent-state/hooks/codex-status.sh", "timeout": 10 }] }],
    "SubagentStart":    [{ "hooks": [{ "type": "command",
      "command": "sh ~/Code/zj-agent-state/hooks/codex-status.sh", "timeout": 10 }] }],
    "SubagentStop":     [{ "hooks": [{ "type": "command",
      "command": "sh ~/Code/zj-agent-state/hooks/codex-status.sh", "timeout": 10 }] }],
    "Stop":             [{ "hooks": [{ "type": "command",
      "command": "sh ~/Code/zj-agent-state/hooks/codex-status.sh", "timeout": 10 }] }]
  }
}
```

Then run `/hooks` once inside Codex to trust the new entries.

**Opencode**:

```sh
mkdir -p ~/.config/opencode/plugins
cp hooks/opencode-bridge.js ~/.config/opencode/plugins/zj-agent-state.js
```

Restart opencode — plugins load once at startup. `opencode-bridge.js` calls
`hooks/opencode-status.py` at `$HOME/Code/zj-agent-state/hooks/...` — if you
cloned somewhere other than `~/Code/zj-agent-state`, edit that path in the
bridge before copying it into place.

## Test

```sh
zellij   # start a brand-new session — an already-running one won't pick any of this up
```

You shouldn't see anything different at first (no wide sidebar, nothing
auto-opened). Press `Alt a` — a pane running `viewer` opens. Press `Alt a`
again — it closes. That's the whole toggle.

## What's still manual, on purpose

- The keybind and the three agent hook configs are copy-paste, not scripted
  — no uninstaller, no `--check`, no automatic JSON merge. Re-applying them
  by hand after a change is safe (they're additive) but not automated.
- `viewer`'s path in the keybind and `opencode-status.py`'s path in the
  bridge both assume your clone location — one string to edit in each if
  yours differs.
- The chime command is hardcoded to macOS's `afplay`.

If this is worth using on more than one machine, the next real step (see
zj-radar's own README for the shape of it) is a proper CLI with `setup`
subcommands that *ask* before writing anything — not a script that writes
on your behalf.
