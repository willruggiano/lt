{inputs, ...}: {
  imports = [
    inputs.git-hooks.flakeModule
  ];
  perSystem = {
    config,
    inputs',
    lib,
    ...
  }: let
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
        # Copy/paste detection (no git-hooks.nix builtin; use the cpd flake input)
        jscpd = {
          enable = true;
          name = "jscpd";
          entry = "${lib.getExe inputs'.cpd.packages.default} .";
          files = "\\.rs$";
          pass_filenames = false;
        };
        # AST-level Rust duplicate detection; complements the token-based jscpd.
        cargo-dupes = {
          enable = true;
          name = "cargo-dupes";
          entry = "${lib.getExe config.packages.cargo-dupes} check --exclude-tests --min-nodes 25 --max-exact 0 --max-near 0";
          files = "\\.rs$";
          pass_filenames = false;
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
