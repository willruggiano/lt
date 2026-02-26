# Nix - Agent Instructions

This project uses **Nix flakes** to manage the development environment.
All tools (compiler, linters, etc.) are provided by Nix - do not install packages with system package managers (apt, brew, etc.).

## DO NOT CHANGE NIX FILES

You are not allowed to modify the Nix infrastructure without explicit
instruction from the user. If you need a tool that is not available in your
environment, **stop and ask the user for guidance**.
