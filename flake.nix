{
  inputs = {
    codex = {
      url = "github:openai/codex/rust-v0.105.0";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    jail.url = "sourcehut:~alexdavid/jail.nix";
    llm-agents = {
      url = "github:numtide/llm-agents.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixpkgs-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = {
    self,
    nixpkgs,
    ...
  } @ inputs: let
    inherit (nixpkgs) lib;
    systems = lib.systems.flakeExposed;
    forEachSystem = lib.genAttrs systems;
    pkgsFor = forEachSystem (system:
      import nixpkgs {
        inherit system;
        overlays = [
          inputs.rust-overlay.overlays.default
          self.overlays.dev
        ];
      });
  in {
    devShells = forEachSystem (system: let
      pkgs = pkgsFor.${system};
    in {
      default = pkgs.mkShell {
        name = "lt";
        inputsFrom = [self.packages.${system}.lt];
        packages = with pkgs; [
          beads-primer
          beads-rust
          cargo-nextest
          ccusage
          ccusage-pi
          claude-code-wrapped
          pi-wrapped
        ];
      };
    });
    overlays = {
      default = final: prev: let
        inherit (prev.stdenv.hostPlatform) system;
      in {
        inherit (self.packages.${system}) lt;
      };

      dev = final: prev: let
        inherit (prev.stdenv.hostPlatform) system;
      in {
        inherit (inputs.codex.packages.${system}) codex-rs;
        inherit
          (inputs.llm-agents.packages.${system})
          beads-rust
          claude-code
          ccusage
          ccusage-codex
          ccusage-pi
          pi
          ;
        inherit
          (self.packages.${system})
          agent-tools
          beads-docs
          beads-primer
          claude-code-wrapped
          pi-wrapped
          jail
          ;

        codex = final.codex-rs;
        tuicr = final.agents.tuicr.overrideAttrs (_: {buildFeatures = ["jj"];});

        # Expose for convenience:
        agents = inputs.llm-agents.packages.${system};

        toolchain = final.rust-bin.selectLatestNightlyWith (toolchain:
          toolchain.default.override {
            extensions = ["rust-src"];
          });
      };
    };

    packages = forEachSystem (system: let
      pkgs = pkgsFor.${system};
      pkgs' = self.packages.${system};
      jail = inputs.jail.lib.extend {
        inherit pkgs;
        basePermissions = cs:
          with cs; [
            base
            bind-nix-store-runtime-closure
            fake-passwd
            mount-cwd
            network
            time-zone
            (try-fwd-env "ANTHROPIC_API_KEY")
            (try-fwd-env "GEMINI_API_KEY")
            (try-fwd-env "OPENAI_API_KEY")
          ];
      };
    in {
      lt = let
        rustPlatform = pkgs.makeRustPlatform {
          cargo = pkgs.toolchain;
          rustc = pkgs.toolchain;
        };
      in
        rustPlatform.buildRustPackage (finalAttrs: {
          pname = "lt";
          version = "0.0.0";
          src = lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = with pkgs; [
            cmake
            llvmPackages.clang
            llvmPackages.libclang.lib
            openssl
            pkg-config
          ];
          postPatch = ''
            sed -i 's/^version = "0\.0\.0"$/version = "${finalAttrs.version}"/' Cargo.toml
          '';
          meta = {
            description = "lt: a Linear tui for power users";
            license = with lib.licenses; [mit];
            mainProgram = "lt";
          };
        });

      agent-tools = pkgs.buildEnv {
        name = "agent-tools";
        paths = with pkgs; [
          bash
          beads-primer
          beads-rust
          coreutils
          curl
          diffutils
          fd
          file
          findutils
          gawk
          gnugrep
          gnused
          gnutar
          gzip
          jq
          jujutsu
          less
          patch
          python3
          python3.pkgs.ddgs # web search tool
          ripgrep
          sd
          sqlite
          tree
          unzip
          wget
          which
        ];
      };

      beads-primer = pkgs.writeShellApplication {
        name = "bp";
        runtimeInputs = [pkgs.beads-rust];
        text = ''
          arg="''${1:---all}"
          if [ "$arg" = "--help" ]; then
              echo "prime the context window with current bead state"
              echo "usage:"
              echo "# prime the context for the *entire workspace*"
              echo "bp"
              echo "# prime the context for *a specific bead*"
              echo "bp bd-xxx"
          fi

          echo "beads quickstart: $PWD/docs/agents/beads.md"
          echo "all beads docs: ${pkgs.beads-docs}"
          echo "(use your grep, ls, and/or read tools to read the docs)"
          echo

          br graph "$arg" 2>/dev/null
          if [ "$arg" != "--all" ]; then
            br ready -r --no-auto-flush --parent="$arg"
          else
            br ready --no-auto-flush
          fi 2>/dev/null

          printf "\n\n$ jj log -n10\n"
          jj log -n10

          printf "\n\n$ jj status\n"
          jj status
        '';
      };

      beads-docs = pkgs.runCommand "beads-docs" {} ''
        cp -r ${pkgs.beads-rust.src}/docs $out
      '';

      claude-code-unwrapped = pkgs.claude-code;
      claude-code-wrapped = jail "claude" pkgs.claude-code (cs:
        with cs; [
          # claude-code
          (readwrite (noescape "~/.claude"))
          (readwrite (noescape "~/.claude.json"))
          (set-env "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS" "1")
          ## readonly mount into the sandbox so claude can't hack around problems
          ## (esp. in auto-approve mode) by modifying its own settings/tools :)
          (ro-bind (noescape ''"$PWD/.claude"'') (noescape ''"$PWD/.claude"''))
          # toolchain
          (add-pkg-deps (
            with pkgs;
              [agent-tools]
              ++ pkgs'.lt.buildInputs
              ++ pkgs'.lt.nativeBuildInputs
          ))
          (readwrite (noescape "~/.cargo"))
          (set-env "BD_ACTOR" pkgs.claude-code.name)
          (set-env "SHELL" "${lib.getExe pkgs.bash}")
          (try-fwd-env "BR_OUTPUT_FORMAT")
        ]);

      pi-unwrapped = pkgs.pi;
      pi-wrapped = jail "pi" pkgs.pi (cs:
        with cs; [
          (add-pkg-deps (
            with pkgs;
              [agent-tools]
              ++ pkgs'.lt.buildInputs
              ++ pkgs'.lt.nativeBuildInputs
          ))
          (readwrite (noescape "~/.cargo"))
          (readwrite (noescape "~/.pi"))
          (set-env "BD_ACTOR" pkgs.pi.name)
          (try-fwd-env "BR_OUTPUT_FORMAT")
        ]);
    });
  };
}
