{inputs, ...}: {
  imports = [
    inputs.devshell.flakeModule
  ];

  perSystem = {
    inputs',
    lib,
    pkgs,
    ...
  }: let
    advisorySetup = import ./advisory-db.nix {
      inherit pkgs;
      advisoryDb = inputs.advisory-db;
    };
  in {
    devshells.default.packages = [inputs'.cpd.packages.default];
    devshells.default.motd = lib.mkForce "";

    # Bake the vendored advisory database into the shell so `make check`'s
    # `cargo deny --offline check` runs hermetically. Materialized under
    # $PRJ_ROOT/.cache (the devshell's cache dir, gitignored); cargo-deny
    # resolves the relative db-path against the project root.
    devshells.default.devshell.startup.advisory-db.text =
      advisorySetup "$PRJ_ROOT/.cache/advisory-db";
  };
}
