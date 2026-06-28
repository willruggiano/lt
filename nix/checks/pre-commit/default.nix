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

    pre-commit.settings = {
      hooks = {
        # Formatting
        treefmt = {
          enable = true;
          package = config.packages.treefmt;
        };
        # Rust
        clippy = {
          enable = true;
          packageOverrides = {
            cargo = config.packages.toolchain;
            clippy = config.packages.toolchain;
          };
          settings.denyWarnings = true;
        };
        rustfmt = {
          enable = true;
          packageOverrides = {
            cargo = config.packages.toolchain;
            rustfmt = config.packages.toolchain;
          };
        };
        # GitHub Actions
        actionlint.enable = true;
        # Nix
        deadnix.enable = true;
        statix.enable = true;
      };
    };
  };
}
