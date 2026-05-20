{
  inputs = {
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
          cargo-nextest
          ccusage
          claude-code-wrapped
          python3
          ruff
          ty
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
        inherit
          (inputs.llm-agents.packages.${system})
          claude-code
          ccusage
          ;
        inherit
          (self.packages.${system})
          agent-tools
          claude-code-wrapped
          jail
          ;

        # Expose for convenience:
        agents = inputs.llm-agents.packages.${system};

        toolchain = final.rust-bin.selectLatestNightlyWith (toolchain:
          toolchain.default.override {
            extensions = ["rust-src"];
          });

        tuicr = final.agents.tuicr.overrideAttrs (_: {buildFeatures = ["jj"];});
      };
    };

    packages = forEachSystem (system: let
      pkgs = pkgsFor.${system};
      pkgs' = self.packages.${system};
      jail = inputs.jail.lib.extend {
        inherit pkgs;
        basePermissions = cs:
          with cs; [
            # base
            base
            bind-nix-store-runtime-closure
            fake-passwd
            mount-cwd
            network
            time-zone
            (try-fwd-env "ANTHROPIC_API_KEY")
            (try-fwd-env "GEMINI_API_KEY")
            (try-fwd-env "OPENAI_API_KEY")
            # application state
            (readonly (noescape "~/.local/state/lt"))
            # toolchain
            (add-pkg-deps (
              with pkgs;
                [agent-tools]
                ++ pkgs'.lt.buildInputs
                ++ pkgs'.lt.nativeBuildInputs
            ))
            (readwrite (noescape "~/.cargo"))
            (set-env "SHELL" "${lib.getExe pkgs.bash}")
          ];
      };
    in {
      lt = let
        rustPlatform = pkgs.makeRustPlatform {
          cargo = pkgs.toolchain;
          rustc = pkgs.toolchain;
        };
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      in
        rustPlatform.buildRustPackage {
          pname = cargoToml.package.name;
          inherit (cargoToml.package) version;
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.lock
              ./Cargo.toml
              ./build
              ./build.rs
              ./docs
              ./src
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = with pkgs; [
            cmake
            llvmPackages.clang
            llvmPackages.libclang.lib
            openssl
            pkg-config
          ];
          meta = {
            inherit (cargoToml.package) description homepage;
            license = with lib.licenses; [mit];
            mainProgram = "lt";
          };
        };

      agent-tools = pkgs.buildEnv {
        name = "agent-tools";
        paths = with pkgs; [
          bash
          coreutils
          curl
          diffutils
          fd
          file
          findutils
          gawk
          git
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

      claude-code-wrapped = jail "claude" pkgs.claude-code (cs:
        with cs; [
          (add-pkg-deps [pkgs.sox])
          (readwrite (noescape "~/.claude"))
          (readwrite (noescape "~/.claude.json"))
          (set-env "CLAUDE_CODE_EFFORT_LEVEL" "max")
          (set-env "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS" "1")
          (wrap-entry (entry: ''
            # The program is already sandboxed. For this reason we opt to
            # start in this mode to facilitate rapid iteration.
            ${entry} --allow-dangerously-skip-permissions
          ''))
        ]);
    });
  };
}
