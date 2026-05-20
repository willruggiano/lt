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

    devshells.default.packages = [config.packages.toolchain];

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
            fileset = lib.fileset.unions [
              ../../../Cargo.lock
              ../../../Cargo.toml
              ../../../build
              ../../../build.rs
              ../../../docs
              ../../../src
            ];
          };
          cargoLock.lockFile = ../../../Cargo.lock;
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

      toolchain = pkgs.rust-bin.selectLatestNightlyWith (toolchain:
        toolchain.default.override {
          extensions = ["rust-src"];
        });
    };
  };
}
