#!/usr/bin/env bash
#
# Install the git-hooks.nix pre-commit hooks in Claude Code remote sessions, so
# coding agents exercise the same gates (treefmt, deadnix, statix,
# markdownlint-cli2) on commit that CI runs -- before their changes reach CI.
#
# Runs as a SessionStart hook AFTER setup.sh. setup.sh provisions the devshell
# via `nix print-dev-env`, which captures the environment but does NOT run the
# devshell's `startup` scripts -- so the `install-git-hooks` startup that wires
# .git/hooks is never executed on that path. `nix run .#install-pre-commit` runs
# git-hooks.nix's installationScript directly, decoupled from the devshell (and
# thus from the claude-code/jail inputs the devshell drags in).
#
# No-op outside Claude Code cloud sessions; idempotent (the installer compares
# before it writes).
set -euo pipefail

# Only act in Claude Code cloud sessions, matching setup.sh.
if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

nix_bin=/nix/var/nix/profiles/default/bin
project_dir="$CLAUDE_PROJECT_DIR"

log() { printf '[claude-pre-commit] %s\n' "$*" >&2; }

# setup.sh installs Nix and must have run first; skip rather than fail if not.
if [ ! -x "$nix_bin/nix" ]; then
  log "Nix not found; setup.sh must run first -- skipping"
  exit 0
fi

# Mirror setup.sh: nix on PATH, accept the flake's nixConfig (binary cache), and
# inherit the proxy CA the environment configured.
export PATH="$nix_bin:$PATH"
export NIX_CONFIG="accept-flake-config = true"
: "${NIX_SSL_CERT_FILE:=${SSL_CERT_FILE:-}}"
[ -n "${NIX_SSL_CERT_FILE:-}" ] && export NIX_SSL_CERT_FILE

log "installing git hooks (nix run .#install-pre-commit)"
(cd "$project_dir" && nix run ".#install-pre-commit")
log "done"
