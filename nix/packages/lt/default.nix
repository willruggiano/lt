{
  inputs,
  lib,
  ...
}: {
  perSystem = {
    config,
    inputs',
    pkgs,
    system,
    ...
  }: {
    _module.args.pkgs = import inputs.nixpkgs {
      inherit system;
      overlays = [
        inputs.rust-overlay.overlays.default
      ];
    };

    devshells.default.packagesFrom = [config.packages.lt];

    jail.additionalCombinators = cs:
      with cs; [
        (add-pkg-deps [config.packages.toolchain])
      ];

    # rustfmt formats without resolving dependencies, so it runs fine inside the
    # offline `nix flake check` sandbox. clippy must compile the crate, which the
    # git-hooks hook cannot do offline (no vendored registry); it runs as the
    # checks.clippy derivation below, which reuses the lt package's vendoring.
    pre-commit.settings.hooks = {
      rustfmt = {
        enable = true;
        packageOverrides = {
          cargo = config.packages.toolchain;
          rustfmt = config.packages.toolchain;
        };
      };
    };

    # Run clippy as a build-sandbox check so it has the same vendored deps and
    # native build inputs as `nix build .#lt` (offline-capable).
    checks.clippy = config.packages.lt.overrideAttrs (old: {
      pname = "${old.pname}-clippy";
      nativeBuildInputs = old.nativeBuildInputs ++ [config.packages.toolchain];
      buildPhase = ''
        runHook preBuild
        cargo clippy --all-targets -- -D warnings
        runHook postBuild
      '';
      doCheck = false;
      installPhase = ''
        touch $out
      '';
    });

    # cargo-dupes only parses sources (no compile), so it runs as a light check
    # over the package source rather than reusing the lt build sandbox.
    checks.cargo-dupes =
      pkgs.runCommandLocal "lt-cargo-dupes" {
        nativeBuildInputs = [config.packages.cargo-dupes];
      } ''
        cargo-dupes check -p ${config.packages.lt.src} \
          --exclude-tests --min-nodes 25 --max-exact 0 --max-near 0
        touch $out
      '';

    packages = {
      default = config.packages.lt;

      lt = let
        rustPlatform = pkgs.makeRustPlatform {
          cargo = config.packages.toolchain;
          rustc = config.packages.toolchain;
        };
        cargoToml = builtins.fromTOML (builtins.readFile "${inputs.self}/Cargo.toml");
      in
        rustPlatform.buildRustPackage {
          pname = cargoToml.package.name;
          inherit (cargoToml.package) version;
          src = lib.fileset.toSource {
            root = ../../..;
            fileset = inputs.globset.lib.globs ../../.. [
              "**/*.rs"
              "build/*.graphql"
              "build/*.toml"
              "Cargo.lock"
              "Cargo.toml"
              "clippy.toml"
              "README.md"
            ];
          };
          auditable = false; # devshell error: conflicting paths between toolchain and cargo-auditable
          cargoLock.lockFile = ../../../Cargo.lock;
          nativeBuildInputs = with pkgs; [
            cargo-deny
            cargo-machete
            cmake
            gnumake
            inputs'.cpd.packages.default
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

      toolchain = pkgs.rust-bin.selectLatestNightlyWith (toolchain:
        toolchain.default.override {
          extensions = ["rust-analyzer" "rust-src"];
        });
    };
  };
}
