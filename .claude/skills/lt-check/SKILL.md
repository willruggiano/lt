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
nix develop .#lt --command nix fmt | tee /tmp/format.log
nix develop .#lt --command make check | tee /tmp/check.log
nix develop .#lt --command make test | tee /tmp/test.log
```

Reminder: **always run commands via the nix devshell**.

## Execution discipline

- Cold compiles can exceed two minutes. Use Bash subagents.
- `tee` command output to a file -- you can grep it afterward if necessary
  instead of re-running the command.

## When a gate fails

Distinguish real failures from environmental ones:

- `no such command: <tool>` -> you ran outside the devshell. Re-run with
  `nix develop .#lt --command ...`. This is not a code failure.
- `multiple-versions = warn` lines from `cargo deny` are warnings, not failures.
  The pass signal is `advisories ok, bans ok, licenses ok, sources ok`.
- `HTTP error 502` on a nix input fetch (e.g. `git.sr.ht`) -> environmental
  network policy block. Ignore.

Only after a gate has failed, run the single offending command directly (e.g.
`nix develop .#lt --command cargo clippy -p lt-tui --all-targets`) to iterate.
