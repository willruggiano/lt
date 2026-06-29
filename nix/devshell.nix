{inputs, ...}: {
  imports = [
    inputs.devshell.flakeModule
  ];

  perSystem = {
    inputs',
    lib,
    ...
  }: {
    devshells.default = {
      packages = [inputs'.cpd.packages.default];
      motd = lib.mkForce "";
    };
  };
}
