{inputs, ...}: {
  imports = [
    inputs.git-hooks.flakeModule
  ];
  perSystem = {
    config,
    lib,
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

    pre-commit.settings = {
      hooks = {
        # Formatting
        treefmt = {
          enable = true;
          package = config.packages.treefmt;
        };
        # GitHub Actions
        actionlint.enable = true;
        # Nix
        deadnix.enable = true;
        statix.enable = true;
        # Task state schema coherence
        check-jsonschema = {
          enable = true;
          package = pkgs.check-jsonschema;
          entry = lib.getExe pkgs.check-jsonschema;
          args = ["--schemafile" "schemas/TASKS.schema.json"];
          language = "system";
          files = "^TASKS.json$";
          types = ["file" "json"];
        };
        check-metaschema = {
          enable = true;
          package = pkgs.check-jsonschema;
          entry = lib.getExe pkgs.check-jsonschema;
          args = ["--check-metaschema"];
          language = "system";
          files = "^schemas/";
          types = ["file" "json"];
        };
      };
    };
  };
}
