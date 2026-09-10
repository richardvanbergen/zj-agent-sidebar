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
cargo build --release --target wasm32-wasip1 -p watcher -p sidebar
cargo build --release -p viewer
```

Three binaries: `target/wasm32-wasip1/release/watcher.wasm`,
`target/wasm32-wasip1/release/sidebar.wasm`, and `target/release/viewer`.
Nothing gets copied anywhere automatically.

## Install the plugins

```sh
mkdir -p ~/.config/zellij/plugins
cp target/wasm32-wasip1/release/watcher.wasm \
   ~/.config/zellij/plugins/zj-agent-state-watcher.wasm
cp target/wasm32-wasip1/release/sidebar.wasm \
   ~/.config/zellij/plugins/zj-agent-state-sidebar.wasm
```

Re-run those `cp`s after every rebuild. Zellij caches compiled plugins by
file path, not content, but `viewer` always spawns the watcher with
`--skip-plugin-cache`, so a stale copy is never actually a problem once the
file itself is updated. The sidebar is loaded fresh on every session start.

`viewer` itself is *not* installed anywhere — point the keybind below
straight at `target/release/viewer` in your clone. Rebuilding overwrites it
in place; there's no separate "install" step for it.

## Sidebar in every tab (the zj-radar mechanism)

The sidebar is a plugin pane pinned into your layout's tab templates —
the same mechanism Zellij uses for its own tab-bar and status-bar, and the
same one zj-radar uses. One pane in the template = one always-visible
sidebar in every tab, including new ones created with `Ctrl+t n`.

Both templates are required: Zellij derives `new_tab_template` from
`default_tab_template` when omitted and drops the `children` call, leaving
runtime tabs with no focusable pane.

If you don't have a layout file, create `~/.config/zellij/layouts/default.kdl`
with:

```kdl
layout {
    default_tab_template {
        pane split_direction="vertical" {
            pane size=32 borderless=true {
                plugin location="zj-agents-sidebar"
            }
            children
        }
    }
    new_tab_template {
        pane split_direction="vertical" {
            pane size=32 borderless=true {
                plugin location="zj-agents-sidebar"
            }
            pane focus=true
        }
    }
}
```

If you already have a layout, add the sidebar pane (the `size=26`
`split_direction` wrapper) around your existing `children` in
`default_tab_template`, and give `new_tab_template` the same split with
`pane focus=true` in place of `children`. Move the sidebar pane after
`children`/`pane focus=true` to put it on the right side instead. Restart
Zellij (or start a new session) to load it.

The sidebar needs a plugin alias. Add to `~/.config/zellij/config.kdl`:

```kdl
plugins {
    zj-agents-sidebar location="file:~/.config/zellij/plugins/zj-agent-state-sidebar.wasm"
}
```

It needs a one-time permission grant (reads pane/tab state + receives the
status pipe); Zellij prompts on first load. No `RunCommands`, no
`ChangeApplicationState` — the sidebar only renders.

## Keybind

Both keybinds talk to the sidebar plugin over a pipe (`MessagePlugin`) —
no wrapper scripts, no extra panes:

```kdl
bind "Alt a" {
    MessagePlugin "zj-agents-sidebar" {
        name "zj_agent_state.sidebar.v1"
        payload "toggle"
    }
}
bind "Alt g" {
    MessagePlugin "zj-agents-sidebar" {
        name "zj_agent_state.sidebar.v1"
        payload "jump"
    }
}
```

- **Alt a** — hide/show the sidebar. A hidden sidebar stays loaded (its
  state keeps updating), so showing it again is instant. Works from any
  tab.
- **Alt g** — jump to the sidebar itself: focuses the sidebar pane in the
  current tab. Then Up/Down (or j/k) move the selection and Enter jumps to
  that agent's pane (switching tabs if needed).

The `plugins` alias in `config.kdl` must use the real absolute `file:` path
to the wasm — plugin locations are **not** `~`-expanded everywhere.

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
