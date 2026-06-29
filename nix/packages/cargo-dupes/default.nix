{
  perSystem = {
    config,
    pkgs,
    lib,
    ...
  }: let
    # Build with the project toolchain: cargo-dupes is edition 2024 /
    # rust-version 1.93, which the pinned nightly already satisfies.
    rustPlatform = pkgs.makeRustPlatform {
      cargo = config.packages.toolchain;
      rustc = config.packages.toolchain;
    };
  in {
    devshells.default.packages = [config.packages.cargo-dupes];

    # Not packaged in nixpkgs and upstream ships no flake, so vendor the
    # published crate from crates.io. cargoLock.lockFile uses the lock bundled
    # in the crate tarball, so no network-derived cargoHash is needed.
    packages.cargo-dupes = rustPlatform.buildRustPackage {
      pname = "cargo-dupes";
      version = "0.2.1";
      src = pkgs.fetchCrate {
        pname = "cargo-dupes";
        version = "0.2.1";
        hash = "sha256-wKwLoLF+kZI0Kfts6HC4fNmGkHcf8qYpBUBx0v3vfJc=";
      };
      cargoLock.lockFile = ./Cargo.lock;
      # The crate's test suite pulls in cargo-husky (git-hook installer) and
      # runs the binary against fixtures; neither is needed to vendor the tool.
      doCheck = false;
      meta = {
        description = "Detect duplicate code in Rust codebases";
        homepage = "https://github.com/mpecan/cargo-dupes";
        license = with lib.licenses; [mit];
        mainProgram = "cargo-dupes";
      };
    };
  };
}
