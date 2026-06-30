{inputs, ...}: {
  imports = [
    inputs.git-hooks.flakeModule
  ];
  perSystem = {
    config,
    pkgs,
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

    # Nix, formatting, and Markdown linting here. Makefile for the cargo gates.
    pre-commit.settings = {
      hooks = {
        treefmt = {
          enable = true;
          package = config.packages.treefmt;
        };
        deadnix.enable = true;
        statix.enable = true;
        # markdownlint-cli2 reads .markdownlint-cli2.jsonc for its globs and the
        # custom wiki-link rule; git-hooks.nix ships only markdownlint (cli v1).
        markdownlint-cli2 = {
          enable = true;
          package = pkgs.markdownlint-cli2;
          entry = "${pkgs.markdownlint-cli2}/bin/markdownlint-cli2";
          files = "\\.md$";
          pass_filenames = false;
        };
      };
    };
  };
}
