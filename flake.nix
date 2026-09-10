{
  description = "zj-agent-sidebar: Zellij agent-state watcher/sidebar plugins, viewer binary, and agent hooks";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self
    , nixpkgs
    , flake-utils
    , rust-overlay
    }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
          config.allowUnfree = true;
        };

        # pinned toolchain with the wasm32-wasip1 std, per INSTALL.md
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          targets = [ "wasm32-wasip1" ];
        };
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        # The chime is hardcoded to macOS `afplay` (INSTALL.md, "What's still
        # manual"). Patch to PulseAudio's player for Linux builds.
        chimePatch = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
          substituteInPlace watcher/src/main.rs \
            --replace-fail '["afplay", "/System/Library/Sounds/Glass.aiff"]' \
                           '["paplay", "/usr/share/sounds/freedesktop/stereo/complete.oga"]'
        '';

        # Native helper binary: target/release/viewer
        viewer = pkgs.rustPlatform.buildRustPackage {
          pname = "zj-agent-sidebar-viewer";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          doCheck = false;

          preBuild = chimePatch;
          # watcher/sidebar don't build natively (zellij-tile is wasm-only);
          # build just the viewer.
          buildAndTestSubdir = "viewer";

          postInstall = ''
            mv $out/bin/viewer $out/bin/zj-agent-viewer
          '';
        };

        # Zellij plugins: target/wasm32-wasip1/release/{watcher,sidebar}.wasm
        # Uses the pinned toolchain above — stock nixpkgs rustc has no
        # wasm32-wasip1 std.
        wasmPlugins = rustPlatform.buildRustPackage {
          pname = "zj-agent-sidebar-wasm";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          doCheck = false;

          preBuild = chimePatch;
          env.CARGO_BUILD_TARGET = "wasm32-wasip1";

          buildPhase = ''
            runHook preBuild
            cargo build --release --offline --target wasm32-wasip1 -p watcher -p sidebar
            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            mkdir -p $out/lib/zellij
            install -Dm644 target/wasm32-wasip1/release/watcher.wasm \
              $out/lib/zellij/zj-agent-state-watcher.wasm
            install -Dm644 target/wasm32-wasip1/release/sidebar.wasm \
              $out/lib/zellij/zj-agent-state-sidebar.wasm
            runHook postInstall
          '';
        };

        # AI coding agents (Claude Code, Codex, opencode) — all present in
        # nixpkgs-unstable; single install target for a dev box.
        agents = pkgs.symlinkJoin {
          name = "llm-agents";
          paths = with pkgs; [
            claude-code
            codex
            opencode
          ];
        };

        default = pkgs.symlinkJoin {
          name = "zj-agent-sidebar";
          paths = [ viewer wasmPlugins ];
        };
      in
      {
        packages = { inherit viewer wasmPlugins agents default; };

        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.zellij
            pkgs.python3
            pkgs.rust-analyzer
          ];
        };
      })
    // {
      # System-level wiring for NixOS rebuild switch. Adds the viewer + both
      # plugin wasms (and optionally the AI agents) to
      # environment.systemPackages. Still never touches config.kdl — see
      # NIXOS.md for the manual paste.
      nixosModules.default =
        { config
        , lib
        , pkgs
        , ...
        }: {
          options.programs.zj-agent-sidebar = {
            enable = lib.mkEnableOption "zj-agent-sidebar Zellij plugins and viewer";
            package = lib.mkPackageOption self.packages.${pkgs.stdenv.hostPlatform.system} "default" { };

            agents = {
              enable = lib.mkEnableOption "claude-code, codex and opencode system-wide";
              package = lib.mkPackageOption self.packages.${pkgs.stdenv.hostPlatform.system} "agents" { };
            };
          };

          config = lib.mkIf config.programs.zj-agent-sidebar.enable {
            environment.systemPackages = [
              config.programs.zj-agent-sidebar.package
            ] ++ lib.optional config.programs.zj-agent-sidebar.agents.enable
              config.programs.zj-agent-sidebar.agents.package;
          };
        };

      # Optional home-manager wiring. Follows INSTALL.md's "manual on
      # purpose" rule: installs the plugin wasms and puts `viewer` on PATH,
      # but never touches your config.kdl or agent hook configs.
      #
      # After enabling, paste into ~/.config/zellij/config.kdl (adjust the
      # absolute path — plugin locations are not ~-expanded everywhere):
      #
      #   plugins {
      #     zj-agents-sidebar location="file:/home/YOU/.config/zellij/plugins/zj-agent-state-sidebar.wasm"
      #   }
      #
      # and the Alt a / Alt g keybinds from INSTALL.md. Hook wiring for
      # Claude/Codex/opencode is also manual (INSTALL.md, "Wire up agent
      # hooks") — the hooks live in this repo's hooks/ dir, so keep a clone
      # on the remote or vendor it with home.file.
      homeManagerModules.default =
        { config
        , lib
        , pkgs
        , ...
        }: {
          options.programs.zj-agent-sidebar = {
            enable = lib.mkEnableOption "zj-agent-sidebar Zellij plugins and viewer";
            package = lib.mkPackageOption self.packages.${pkgs.stdenv.hostPlatform.system} "default" { };
          };

          config = lib.mkIf config.programs.zj-agent-sidebar.enable {
            home.packages = [ config.programs.zj-agent-sidebar.package ];

            xdg.configFile."zellij/plugins/zj-agent-state-watcher.wasm".source =
              "${config.programs.zj-agent-sidebar.package}/lib/zellij/zj-agent-state-watcher.wasm";
            xdg.configFile."zellij/plugins/zj-agent-state-sidebar.wasm".source =
              "${config.programs.zj-agent-sidebar.package}/lib/zellij/zj-agent-state-sidebar.wasm";
          };
        };
    };
}
