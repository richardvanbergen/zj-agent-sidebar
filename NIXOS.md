# NixOS setup

Flake builds everything `INSTALL.md` builds, as Nix packages:

- `zj-agent-sidebar` (default): `bin/zj-agent-viewer` + `lib/zellij/*.wasm` (watcher, sidebar)
- `zj-agent-sidebar-viewer`: viewer only
- `zj-agent-sidebar-wasm`: the two plugin wasms only
- `agents`: `claude-code`, `codex`, and `opencode` from nixpkgs-unstable
- `devShells.default`: Rust toolchain (wasm32-wasip1 target included), Zellij, python3, rust-analyzer

The Linux chime patch (`afplay` → `paplay` + freedesktop sound) is applied
automatically in the Nix builds, so nothing to edit there.

## Try it

```sh
nix build .            # or: nix build .#zj-agent-sidebar-viewer
nix shell .            # puts zj-agent-viewer on PATH
nix shell .#agents     # puts claude / codex / opencode on PATH
nix profile install .#agents   # or install the agents persistently
nix develop            # dev shell for hacking on the repo itself
```

## Home Manager

```nix
# flake.nix of your config
inputs.zj-agent-sidebar.url = "github:richardvanbergen/zj-agent-sidebar";

# home-manager config
imports = [ inputs.zj-agent-sidebar.homeManagerModules.default ];
programs.zj-agent-sidebar.enable = true;
```

That installs `zj-agent-viewer` on PATH and copies both wasms to
`~/.config/zellij/plugins/zj-agent-state-{watcher,sidebar}.wasm`.

## Still manual, on purpose (per INSTALL.md)

The flake never touches your configs. After enabling, paste into
`~/.config/zellij/config.kdl` — the plugin alias must be a real absolute
path, `~` is not expanded everywhere:

```kdl
plugins {
    zj-agents-sidebar location="file:/home/YOU/.config/zellij/plugins/zj-agent-state-sidebar.wasm"
}
```

And the keybinds from `INSTALL.md` (Alt a toggle, Alt g jump).

Layout: pin the sidebar pane into `default_tab_template` **and**
`new_tab_template` per `INSTALL.md` ("Sidebar in every tab").

Agent hooks: `INSTALL.md` "Wire up agent hooks" — the hook scripts live in
this repo, so either keep a clone on the remote (the usual
`~/Code/zj-agent-sidebar` path) or vendor `hooks/` with `home.file` and
point the hook commands at that path.
