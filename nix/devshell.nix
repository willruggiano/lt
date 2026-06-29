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
    devshells.default = {
      packages = [inputs'.cpd.packages.default];
      motd = lib.mkForce "";

      devshell.startup.advisory-db.text =
        advisorySetup "$PRJ_ROOT/.cache/advisory-db";
    };
  };
}
