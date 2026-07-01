---
name: lt-check
description:
  Run the lt build/lint/test gate correctly. Use before committing or pushing
  Rust changes, or when validating that the workspace builds and passes checks.
  Encodes the nix-devshell and nightly-rustfmt traps that make local checks
  disagree with CI.
---

# lt-check

`make check` and `make test` are the gate (see `.claude/CLAUDE.md`,
`docs/rules/testing.md`, `docs/rules/nix.md`). Run them, not ad-hoc `cargo`
invocations. Reach for an individual command only to debug a gate that has
already failed.

## Always run the gate inside the nix devshell

```sh
nix develop .#lt --command make check
nix develop .#lt --command make test
```

`make check` runs `cargo deny`, `cargo machete`, `cargo dupes`, and `cpd`
(jscpd). Those binaries are provided by the devshell, **not** by a plain
`rustup` toolchain. Running `make check` from an unprepared shell fails at the
first devshell-only tool:

```text
error: no such command: `deny`
make: *** [Makefile:19: check] Error 101
```

`clippy` and the tests will still run outside the devshell, but the gate is not
complete until `deny`/`machete`/`cpd`/`dupes` have run. When in doubt, prefix
with `nix develop .#lt --command`.

## The nightly-rustfmt trap (this is why CI goes red on formatting)

`rustfmt.toml` sets nightly-only options:

```toml
imports_granularity = "Module"
group_imports = "StdExternalCrate"
```

A **stable** `rustfmt` cannot apply them. It prints a warning and passes anyway:

```text
Warning: can't set `imports_granularity = Module`, unstable features are only
available in nightly channel.
```

So `cargo fmt --check` on a stable toolchain reports **clean** while imports are
actually misgrouped. CI runs the **nightly** toolchain and enforces both rules,
so the push goes red on formatting even though local `make check` was green.

Always format through the devshell (nightly) before pushing:

```sh
nix develop .#lt --command cargo fmt   # rewrites imports to satisfy CI
nix develop .#lt --command make check   # treefmt + cargo fmt --check, nightly
```

## Toolchain

- The devshell pins a **nightly** toolchain (rustc 1.98.x at time of writing);
  it is the source of truth for fmt, clippy, and CI parity.
- Floor: `libsqlite3-sys` (via `rusqlite`) uses `cfg_select!` and needs rustc
  **>= 1.96**. A 1.94 stable toolchain fails to build the workspace.

## Cold compiles are slow — do not let a short timeout kill them

A full `cargo check --workspace --all-targets` or a devshell `make check` from
cold can exceed two minutes. Run it with `run_in_background: true` (or a Bash
`timeout` of >= 300000 ms) and poll the log, rather than blocking a default 120
s call that will be killed mid-compile.

## Reading the output

- **Clippy** is the code gate: `--all-targets` and
  `--all-targets --all-features`, both `-D warnings`. Strict
  `all`/`pedantic`/`cargo` plus panic-safety lints (see `docs/rules/rust.md`).
- **cargo deny** may print `multiple-versions = warn` duplicate warnings
  (`thiserror`, `hashbrown`, `windows-sys`, ...). Those are warnings from
  transitive deps, not failures. Success is the final line:
  `advisories ok, bans ok, licenses ok, sources ok`. The Makefile runs deny with
  `GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null` so the advisory-db
  clone bypasses the repo git proxy (see `docs/rules/nix.md`).
- **cargo machete** flags declared-but-unused dependencies.
- **cpd** / **cargo dupes** flag duplicated code. `cpd` reports
  `0 clones · 0.0% duplication`; `dupes` reports `Check passed.`
- **make test** runs twice: feature-off, then `--all-features` (enables `sim`).
  Both must pass — they are distinct compile configurations.

## When a gate fails

Distinguish real failures from environmental ones:

- `no such command: <tool>` → you are outside the devshell; re-run with
  `nix develop .#lt --command ...`.
- Formatting diff only under nightly → the import-grouping trap; run `cargo fmt`
  through the devshell.
- `HTTP error 502` fetching a nix input (e.g. `git.sr.ht`) → an upstream/network
  outage, not your change. Confirm by checking a clean commit.

Only after a gate has failed, run the single offending command directly (e.g.
`nix develop .#lt --command cargo clippy -p lt-tui --all-targets`) to iterate.
