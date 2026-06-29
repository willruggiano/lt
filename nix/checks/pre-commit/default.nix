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

    # `nix flake check` is scoped to nix tooling only: dead-code, lint, and
    # format gates for the flake itself. Rust, copy/paste, and CI gates live in
    # the Makefile; cross-language formatting lives in `nix fmt` (treefmt).
    pre-commit.settings = {
      hooks = {
        alejandra.enable = true;
        deadnix.enable = true;
        statix.enable = true;
      };
    };
  };
}
