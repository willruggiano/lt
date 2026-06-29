#!/usr/bin/env bash
#
# Provision the Nix devshell in Claude Code remote (cloud) sessions so that
# `nix develop .#lt -c make check` runs the same toolchain as CI. Nix is the
# single source of truth for tooling (see flake.nix / nix/); this script just
# makes that toolchain reachable on the Anthropic-managed VM, which ships no Nix.
#
# No-op everywhere else (local shells, CI), so it is safe to wire into both the
# cloud environment "Setup script" field and a repo SessionStart hook.
#
# Idempotent and dual-purpose by design:
#   - run as the cloud "Setup script" -> the install + warm land in the cached
#     filesystem snapshot, so later sessions start with /nix already populated.
#   - run as a SessionStart hook       -> starts the nix-daemon, which does NOT
#     survive in the snapshot (snapshots capture files, not processes), and
#     exports PATH for the agent's shells via $CLAUDE_ENV_FILE.
set -euo pipefail

# Only act in Claude Code cloud sessions. CLAUDE_CODE_REMOTE=true is set there.
if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

nix_bin=/nix/var/nix/profiles/default/bin
project_dir="${CLAUDE_PROJECT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"

log() { printf '[claude-setup] %s\n' "$*" >&2; }

# 1. Install Nix if absent. Daemonless (--init none): the cloud VM has no
#    systemd as PID 1, so the standard daemon planner would fail.
if [ ! -x "$nix_bin/nix" ]; then
  log "installing Nix (determinate, --init none)"
  curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix \
    | sh -s -- install linux --init none --no-confirm
else
  log "Nix already installed"
fi

# 2. Start the daemon if its socket is down. --init none does not start it, and
#    there is no init system to do so; a started daemon is not captured by the
#    environment snapshot, so this must run every session.
if [ ! -S /nix/var/nix/daemon-socket/socket ]; then
  log "starting nix-daemon"
  nohup "$nix_bin/nix-daemon" >/tmp/nix-daemon.log 2>&1 &
  for _ in $(seq 1 30); do
    [ -S /nix/var/nix/daemon-socket/socket ] && break
    sleep 1
  done
  [ -S /nix/var/nix/daemon-socket/socket ] || { log "nix-daemon failed to start"; exit 1; }
fi

# 3. Expose Nix to subsequent agent shells. Inherit the proxy CA the environment
#    already configured (do not hard-code a path); only set it if present.
export PATH="$nix_bin:$PATH"
: "${NIX_SSL_CERT_FILE:=${SSL_CERT_FILE:-}}"
[ -n "${NIX_SSL_CERT_FILE:-}" ] && export NIX_SSL_CERT_FILE
if [ -n "${CLAUDE_ENV_FILE:-}" ]; then
  printf 'PATH=%s:$PATH\n' "$nix_bin" >>"$CLAUDE_ENV_FILE"
  [ -n "${NIX_SSL_CERT_FILE:-}" ] && \
    printf 'NIX_SSL_CERT_FILE=%s\n' "$NIX_SSL_CERT_FILE" >>"$CLAUDE_ENV_FILE"
fi

# 4. Warm the devshell so the first `make check` is fast. Best-effort: with a
#    binary cache this is a quick fetch; a cold first build can be slow but must
#    not block the session.
log "warming devshell (nix develop .#lt)"
if (cd "$project_dir" && nix develop ".#lt" --command true); then
  log "devshell ready"
else
  log "devshell warm failed (non-fatal; tools build on first use)"
fi

log "done"
