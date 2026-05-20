{
  imports = [
    ./claude-code
    ./lt
  ];

  perSystem = {
    config,
    inputs',
    ...
  }: {
    devshells.default.packages = [config.packages.ox];

    jail.additionalCombinators = cs:
      with cs; [
        (add-pkg-deps [config.packages.ox])
        (readonly (noescape "~/.config/sageox"))
        (readwrite (noescape "~/.local/share/sageox"))
      ];

    packages = {
      ox = inputs'.sageox.packages.default;
    };
  };
}
