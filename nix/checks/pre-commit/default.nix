{inputs, ...}: {
  imports = [
    inputs.git-hooks.flakeModule
  ];
  perSystem = {config, ...}: let
    cfg = config.pre-commit;
  in {
    devshells.default.devshell.startup.install-git-hooks.text = config.pre-commit.shellHook;

    jail.additionalCombinators = cs:
      with cs; [
        (add-pkg-deps [cfg.settings.package])
        (add-pkg-deps cfg.settings.enabledPackages)
        (readonly cfg.settings.configFile)
      ];

    # Only Nix-related + formatting here. Makefile for everything else.
    pre-commit.settings = {
      hooks = {
        treefmt = {
          enable = true;
          package = config.packages.treefmt;
        };
        deadnix.enable = true;
        statix.enable = true;
      };
    };
  };
}
