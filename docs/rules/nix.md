---
paths:
  - "**/*.nix"
  - "**/setup.sh"
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

Three disjoint gates exist. A check belongs to exactly one; do not duplicate it
across them.

```text
nix flake check   nix tooling only  alejandra, deadnix, statix (+ the treefmt
                                    formatting check)
make check        rust + project    fmt, clippy, cargo-deny, machete, cpd,
                                    cargo-dupes, test — runs in the devshell
nix fmt           formatting        treefmt across all languages
```

Rule: rust, supply-chain (`cargo-deny`), dedup (`cargo-dupes`), copy/paste
(`cpd`), and test gates live in the `Makefile`, not in `nix flake check`. The
flake checks are limited to nix's own tooling so `nix flake check` answers one
question — "is the nix code well-formed?" — and `make check` answers "is the
project correct?". CI runs both (`.github/workflows/ci.yml`): `nix flake check`,
then `nix build .#lt`, then `nix develop .#lt -c make check`.

## Devshell provisioning

`make check` and CI both run inside `nix develop .#lt`. On Anthropic-managed
remote sessions (which ship no Nix) `.claude/bin/setup.sh` bootstraps that
environment in three phases:

```text
install      determinate-nix, daemonless (--init none): the VM has no PID-1 init
start daemon nohup nix-daemon: not captured by the filesystem snapshot, so it
             runs every session
capture env  nix print-dev-env .#lt >> $CLAUDE_ENV_FILE: every agent shell then
             runs inside the devshell toolchain, not merely with nix on PATH
```

It is idempotent and a no-op outside `CLAUDE_CODE_REMOTE=true`, so the same
script is safe as both the cloud "Setup script" and a `SessionStart` hook.

## Offline cargo-deny

`cargo deny check` needs the RustSec advisory database, but cloning
`github.com/rustsec/advisory-db` 403s behind the repo-scoped git proxy in remote
sessions. `nix/advisory-db.nix` vendors the pinned `advisory-db` flake input as
a git-shaped checkout; `nix/devshell.nix` bakes it into `$PRJ_ROOT/.cache` on
shell startup so `make check`'s `cargo deny --offline check` resolves it
locally. The subdir name and `FETCH_HEAD` shape are cargo-deny version-specific
— see the header comment in `nix/advisory-db.nix`.

## Binary cache

`flake.nix` declares the `lt.cachix.org` substituter via `nixConfig`. CI pushes
to it (`cachix-action`); `setup.sh` and CI pass `accept-flake-config = true` so
builds pull from the cache without prompting.
