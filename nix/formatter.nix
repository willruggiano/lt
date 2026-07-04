{inputs, ...}: {
  imports = [
    inputs.treefmt.flakeModule
  ];

  perSystem = {
    config,
    lib,
    ...
  }: {
    devshells.default.packages = [config.packages.treefmt];

    jail.additionalCombinators = cs:
      with cs; [
        (add-pkg-deps [config.packages.treefmt])
      ];

    packages.treefmt = config.treefmt.build.wrapper;

    treefmt = {
      projectRootFile = "flake.nix";
      programs = {
        alejandra.enable = true;
        prettier = {
          enable = true;
          settings.proseWrap = "always";
        };
        shfmt.enable = true;
      };
      settings.formatter = {
        prettier.includes = lib.mkForce ["*.json" "*.md"];
      };
    };
  };
}
