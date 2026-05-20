{
  inputs,
  flake-parts-lib,
  ...
}: {
  options.perSystem = flake-parts-lib.mkPerSystemOption ({
    config,
    pkgs,
    lib,
    ...
  }: let
    cfg = config.jail;

    gitconfigFor = programCfg:
      lib.generators.toGitINI (lib.recursiveUpdate cfg.git programCfg.git);

    combinatorsType = lib.mkOptionType {
      name = "combinators";
      description = "jail.nix combinator function (cs -> [combinators])";
      check = builtins.isFunction;
      merge = _loc: defs: cs: lib.concatMap (def: def.value cs) defs;
    };

    programModule = lib.types.submodule ({
      name,
      config,
      ...
    }: {
      options = {
        package = lib.mkOption {
          type = lib.types.package;
          description = "The unwrapped package to jail.";
        };

        git = lib.mkOption {
          type = lib.types.attrs;
          default = {};
          description = "Per-program gitconfig overrides, merged with jail.git.";
        };

        additionalCombinators = lib.mkOption {
          type = combinatorsType;
          default = _: [];
          description = "Additional jail.nix combinators for this program.";
        };

        build.wrapped = lib.mkOption {
          type = lib.types.package;
          readOnly = true;
          description = "The jailed package (read-only, computed).";
        };
      };

      config.build.wrapped = let
        jail = inputs.jail.lib.extend {
          inherit pkgs;
          basePermissions = cs:
            with cs; [
              base
              bind-nix-store-runtime-closure
              fake-passwd
              gpu
              mount-cwd
              network
              open-urls-in-browser
              readonly-runtime-args
              time-zone
              (add-pkg-deps (with pkgs; [
                bash
                coreutils
                curl
                diffutils
                fd
                file
                findutils
                gawk
                git
                gnugrep
                gnused
                gnutar
                gzip
                jq
                less
                patch
                python3
                python3.pkgs.ddgs # web search tool
                ripgrep
                sd
                tree
                unzip
                wget
                which
              ]))
              (write-text "/etc/gitconfig" (gitconfigFor config))
              (set-env "SHELL" (pkgs.lib.getExe pkgs.bash))
            ];
        };
        combinators = cs:
          (cfg.additionalCombinators cs) ++ (config.additionalCombinators cs);
      in
        jail name config.package combinators;
    });
  in {
    options.jail = {
      git = lib.mkOption {
        type = lib.types.attrs;
        default = {};
        description = "Gitconfig attrset, passed directly to lib.generators.toGitINI.";
      };

      additionalCombinators = lib.mkOption {
        type = combinatorsType;
        default = _: [];
        description = "Additional jail.nix combinators applied to all programs.";
      };

      programs = lib.mkOption {
        type = lib.types.attrsOf programModule;
        default = {};
        description = "Programs to jail.";
      };
    };
  });
}
