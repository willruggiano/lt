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

    # Standalone installer for the git hooks, decoupled from the devshell so a
    # remote agent session can run it (see .claude/bin/install-pre-commit.sh).
    packages.install-pre-commit =
      pkgs.writeShellScriptBin "install-pre-commit" config.pre-commit.installationScript;

    jail.additionalCombinators = cs:
      with cs; [
        (add-pkg-deps [cfg.settings.package])
        (add-pkg-deps cfg.settings.enabledPackages)
        (readonly cfg.settings.configFile)
      ];

    pre-commit.settings = {
      hooks = {
        deadnix.enable = true;
        markdownlint = {
          enable = true;
          package = pkgs.markdownlint-cli2;
          entry = "${pkgs.markdownlint-cli2}/bin/markdownlint-cli2";
          files = "\\.md$";
          pass_filenames = false;
        };
        shellcheck.enable = true;
        statix.enable = true;
        treefmt = {
          enable = true;
          package = config.packages.treefmt;
        };
      };
    };
  };
}
