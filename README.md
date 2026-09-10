# zj-agent-state (spike)

One question: can the thing that watches Zellij state and the thing that
displays it be two separate processes, joined only by a file — not one wasm
plugin that does both? This is not a product, it's the smallest program that
can answer that.

Not a fork of anything. Built with two projects as reference, neither of
which is depended on or copied wholesale:

- **zj-herd** (this machine, `../zj-herd`) — an already-working, live-tested
  floating panel that does both halves itself (renders, takes keys, jumps to
  panes). Its `PaneUpdate`/`TabUpdate` join logic and its `go-to-tab`-then-
  `focus-pane-id` jump sequence are reused here near-verbatim, credited
  inline — no point re-discovering what it already proved.
- **zj-radar** (github.com/marktoda/zj-radar, MIT) — precedent for the
  general shape (push-only, no polling) and for persisting state as plain
  JSON on a WASI-mounted path rather than shelling out to write a file.

## The split

```
Claude Code hook ─zellij pipe─▶ watcher (wasm plugin)  ─┐
                                                          │ writes JSON
Zellij PaneUpdate/TabUpdate ───▶ watcher ────────────────┘
                                     │
                                     ▼
                      <tmp>/zellij-<uid>/zj-agent-state/state.json
                                     │
                                     ▼
                          viewer (ordinary terminal program)
                                     │
                                     ▼
                     `zellij action go-to-tab` / `focus-pane-id`
```

- **`watcher/`** — wasm Zellij plugin. Requests only `ReadApplicationState` +
  `ReadCliPipes` (no `ChangeApplicationState`, no `RunCommands` — it never
  navigates and never runs a command). Joins pushed agent status to Zellij's
  own pane/tab geometry, same as zj-herd, and on every change writes the
  joined rows to disk with a plain `std::fs::write` into its WASI-mounted
  `/tmp` — no shell-out needed for that either.
- **`viewer/`** — a native binary. Not a Zellij plugin, not wasm, requests no
  Zellij permissions of any kind. Polls the file, renders a list, and on
  Enter shells out to `zellij action`.
- **`shared/`** — the JSON shape both agree on. The only contract between
  the two processes.

## The one non-obvious bit: the `/tmp` path

Zellij mounts a plugin's WASI `/tmp` onto its **own** scratch dir
(`zellij-utils/src/consts.rs`: `std::env::temp_dir().join("zellij-<uid>")`),
not the literal host `/tmp`. `watcher` writes to guest path
`/tmp/zj-agent-state/state.json`; `viewer` has no such mount, so it
recomputes that same host path itself (`viewer/src/main.rs::state_path`).
Verified against this machine's real Zellij scratch dir
(`$TMPDIR/zellij-503`, already present from a prior session) before writing
any code against it — if Zellij ever changes that formula, this is the one
place it breaks.

## Try it

```sh
cargo build --release --target wasm32-wasip1 -p watcher
cargo build --release -p viewer
mkdir -p ~/.config/zellij/plugins
cp target/wasm32-wasip1/release/watcher.wasm ~/.config/zellij/plugins/zj-agent-state-watcher.wasm
```

Load the watcher into any running session (any pane, tiled is fine — it's
not meant to be looked at):

```sh
zellij action new-pane -- true   # or just pick any pane
zellij plugin -- file:$HOME/.config/zellij/plugins/zj-agent-state-watcher.wasm
```

Grant the permission prompt once. Feed it a fake status by hand:

```sh
zellij pipe --name zj_agent_state.status.v1 -- \
  '{"pane_id":'"${ZELLIJ_PANE_ID}"',"status":"working","agent":"demo","message":"hello"}' \
  < /dev/null
```

Then, in any pane (does not need to be inside Zellij at all):

```sh
./target/release/viewer
```

It should show one row for the pane you fed. Up/Down move, Enter calls
`zellij action go-to-tab` + `focus-pane-id` (best-effort — zj-herd's README
already documents that `focus-pane-id` alone doesn't switch tabs in 0.45.1,
hence the go-to-tab-first order), `q`/Esc quits.

For real Claude Code status instead of hand-fed JSON, wire up
`hooks/claude-status.sh` the same way zj-herd's own hook is wired (see that
script's header) — it's the same script pointed at this project's pipe name
(`zj_agent_state.status.v1`).

## What this deliberately does not do (it's a spike)

- No session-scoping (`ZELLIJ_SESSION_NAME` isn't in the path) — one running
  watcher's state clobbers another's. Fine for "does the split work at all",
  wrong for two sessions at once.
- No debounce/atomic-rename on write (zj-radar's own snapshot code does
  temp-file-plus-rename for exactly this reason) — a viewer read mid-write
  could occasionally see a half-written file.
- `viewer` polls on a 300ms timer rather than watching the file — fine for a
  spike, a real version would use a file-watch crate or have `watcher` also
  write to a fifo/socket.
- Single global path, not per-plugin-instance — only makes sense with one
  watcher loaded at a time.

If the split holds up under real use, the next real question is whether
`watcher` should also persist enough to let a *fully* external program (no
Zellij plugin anywhere) resolve pane→tab mapping without ever being loaded —
right now `viewer` still depends on `watcher` being the one Zellij plugin
that has the manifest.
