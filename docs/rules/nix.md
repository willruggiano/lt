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
   ├─ nix/checks/        the flake's `pre-commit` checks (nix-only, see below)
   └─ nix/devshell.nix   devshells.default — the dev and CI environment
```

## Gate boundaries

Four disjoint gates; each check belongs to exactly one. Do not duplicate across
them.

```text
nix flake check   nix tooling only  treefmt (fmt), deadnix, statix
make check        rust + project    fmt, clippy, cargo-deny, machete, cpd,
                                    cargo-dupes, test — runs in the devshell
make cov          coverage          cargo-llvm-cov line-coverage floor — runs in
                                    the devshell; see [[test-coverage-gate.md]]
nix fmt           formatting        treefmt across all languages
```

- `nix flake check` answers "is the nix code well-formed?"; `make check` answers
  "is the project correct?".
- Rust, supply-chain, dedup, copy/paste, test, and coverage gates live in the
  `Makefile`, never in `nix flake check`.
- CI (`.github/workflows/ci.yml`) runs all three: `nix flake check` →
  `nix build .#lt` → `nix develop .#lt -c make check` →
  `nix develop .#lt -c make cov`.

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
