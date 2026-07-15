{
  perSystem = {
    config,
    inputs',
    pkgs,
    ...
  }: {
    devshells.default.packages = [config.packages.pi];

    jail = {
      programs.pi = {
        additionalCombinators = cs:
          with cs; [
            (add-pkg-deps [config.packages.pi-unwrapped])
            (readwrite (noescape "~/.pi"))
          ];
        git = {
          user.email = "noreply@pi.dev";
          user.name = config.packages.pi-unwrapped.name;
        };
        package = config.packages.pi-unwrapped;
      };
    };

    packages = {
      pi = let
        drv = config.jail.programs.pi.build.wrapped;
      in
        drv
        // {
          name = "${config.packages.pi-unwrapped.name}-jailed";
          unjailed = config.packages.pi-unwrapped;
        };

      pi-unwrapped = inputs'.agents.packages.pi;
    };
  };
}
