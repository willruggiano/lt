{
  perSystem = {
    config,
    inputs',
    pkgs,
    ...
  }: {
    devshells.default.packages = [config.packages.claude-code];

    jail = {
      programs.claude = {
        additionalCombinators = cs:
          with cs; [
            (add-pkg-deps [
              pkgs.sox
            ])
            (readwrite (noescape "~/.claude"))
            (readwrite (noescape "~/.claude.json"))
            (set-env "CLAUDE_CODE_EFFORT_LEVEL" "max")
            (set-env "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS" "1")
            (wrap-entry (entry: ''
              ${entry} --allow-dangerously-skip-permissions
            ''))
          ];
        git = {
          user.email = "noreply@anthropic.com";
          user.name = config.packages.claude-code-unwrapped.name;
        };
        package = config.packages.claude-code-unwrapped;
      };
    };

    apps.claude-code = {
      type = "app";
      program = pkgs.lib.getExe config.packages.claude-code;
      meta.description = config.packages.claude-code.name;
    };

    packages = {
      claude-code = let
        drv = config.jail.programs.claude.build.wrapped;
      in
        drv
        // {
          name = "${config.packages.claude-code-unwrapped.name}-jailed";
          unjailed = config.packages.claude-code-unwrapped;
        };

      claude-code-unwrapped = inputs'.agents.packages.claude-code;
    };
  };
}
