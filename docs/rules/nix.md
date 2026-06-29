---
paths:
  - "**/*.nix"
  - ".claude/bin/setup.sh"
---

# Nix setup

Nix is the single source of truth for tooling: it pins the Rust toolchain,
builds the `lt` package, and provisions the devshell that every other workflow
runs inside. See [[contributing.md]] for the strictness posture these gates
enforce.

## Module layout

`flake.nix` is a [flake-parts](https://flake.parts) tree; each `nix/` module
owns one concern:

```text
flake.nix
└─ imports
   ├─ nix/jailed.nix     jail.nix wrapper plumbing (the `jail.programs.*` option)
   ├─ nix/formatter.nix  treefmt -> `nix fmt`; exposes packages.treefmt
   ├─ nix/packages/      packages.{lt,toolchain,cargo-dupes,claude-code}
   ├─ nix/checks/        the flake's `pre-commit` checks (nix-only)
   └─ nix/devshell.nix   devshells.default — the dev and CI environment
```

## Devshell provisioning

- `make check` and CI run inside `nix develop .#lt`.
- On Anthropic-managed remote sessions (no Nix), `.claude/bin/setup.sh`
  bootstraps it:
  - **install** — determinate-nix, daemonless (`--init none`); the VM has no
    PID-1 init.
  - **start daemon** — `nohup nix-daemon`; not in the filesystem snapshot, so it
    runs every session.
  - **capture env** — `nix print-dev-env .#lt >> $CLAUDE_ENV_FILE`; every agent
    shell then runs inside the devshell toolchain.
- Idempotent and a no-op outside `CLAUDE_CODE_REMOTE=true`, so it serves as both
  the cloud "Setup script" and a `SessionStart` hook.

## cargo-deny git proxy

- `cargo deny check` clones `rustsec/advisory-db` to fetch the RustSec advisory
  database.
- The repo-scoped git proxy in remote sessions injects global/system git config
  (proxy and `url.*.insteadOf` rewrites) that 403s the clone.
- The `Makefile` runs the gate as
  `GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null cargo deny check`, so
  global/system git config is ignored for that step only and the clone goes
  direct.

## Binary cache

- `flake.nix` declares the `lt.cachix.org` substituter via `nixConfig`.
- CI pushes to it (`cachix-action`).
- `setup.sh` and CI pass `accept-flake-config = true` so builds pull from the
  cache without prompting.
