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

    packages = {
      default = config.packages.lt;

      lt = let
        rustPlatform = pkgs.makeRustPlatform {
          cargo = config.packages.toolchain;
          rustc = config.packages.toolchain;
        };
        # The root manifest is a virtual workspace (no `[package]`); shared
        # metadata lives under `[workspace.package]`. The `lt` binary and its
        # description come from the `lt-cli` member crate.
        workspaceToml = builtins.fromTOML (builtins.readFile "${inputs.self}/Cargo.toml");
        cliToml = builtins.fromTOML (builtins.readFile "${inputs.self}/crates/lt-cli/Cargo.toml");
      in
        rustPlatform.buildRustPackage {
          pname = "lt";
          inherit (workspaceToml.workspace.package) version;
          src = lib.fileset.toSource {
            root = ../../..;
            fileset = inputs.globset.lib.globs ../../.. [
              "**/*.rs"
              "**/*.snap" # insta snapshots; the package build's tests read these
              "**/Cargo.toml" # workspace root + every member crate manifest
              "build/*.graphql"
              "build/*.toml"
              "Cargo.lock"
              "clippy.toml"
              "README.md"
            ];
          };
          auditable = false; # devshell error: conflicting paths between toolchain and cargo-auditable
          cargoLock.lockFile = ../../../Cargo.lock;
          nativeBuildInputs = with pkgs; [
            cargo-deny
            cargo-llvm-cov
            config.packages.cargo-dupes
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
            inherit (cliToml.package) description;
            inherit (workspaceToml.workspace.package) homepage;
            license = with lib.licenses; [mit];
            mainProgram = "lt";
          };
        };

      toolchain = pkgs.rust-bin.selectLatestNightlyWith (toolchain:
        toolchain.default.override {
          extensions = ["rust-analyzer" "rust-src" "llvm-tools-preview"];
        });
    };
  };
}
