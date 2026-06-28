{inputs, ...}: {
  imports = [
    inputs.devshell.flakeModule
  ];

  perSystem = {
    inputs',
    lib,
    ...
  }: {
    devshells.default.packages = [inputs'.cpd.packages.default];
    devshells.default.motd = lib.mkForce "";
  };
}
