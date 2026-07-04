{inputs, ...}: {
  imports = [
    inputs.devshell.flakeModule
  ];

  perSystem = {
    inputs',
    lib,
    pkgs,
    ...
  }: {
    devshells.default = {
      packages = [inputs'.cpd.packages.default pkgs.scc];
      motd = lib.mkForce "";
    };
  };
}
