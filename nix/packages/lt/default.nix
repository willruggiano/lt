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
        cargoToml = builtins.fromTOML (builtins.readFile "${inputs.self}/Cargo.toml");
      in
        rustPlatform.buildRustPackage {
          pname = cargoToml.package.name;
          inherit (cargoToml.package) version;
          src = lib.fileset.toSource {
            root = ../../..;
            fileset = inputs.globset.lib.globs ../../.. [
              "**/*.rs"
              "**/*.snap" # insta snapshots; the package build's tests read these
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
            inherit (cargoToml.package) description homepage;
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
