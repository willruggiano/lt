{inputs, ...}: {
  imports = [
    inputs.devshell.flakeModule
  ];

  perSystem = {
    lib,
    pkgs,
    ...
  }: {
    devshells.default = {
      packages = with pkgs; [scc sqlite];
      motd = lib.mkForce "";
    };
  };
}
