---
name: lt-code-writer
description:
  How to write and edit Rust in the lt workspace so it passes the strict gate
  the first time. Use when creating or changing `.rs` files or `Cargo.toml`.
  Distills the project's posture, lint, module, and test rules and points at
  their sources of truth.
---

# lt-code-writer

The authoritative rules live in the repo; this skill distills the operative ones
and links to the source. When a rule here and a `docs/rules/` file disagree, the
file wins.

- Posture and working principles: `docs/rules/posture.md`
- Rust conventions (lints, panic-safety): `docs/rules/rust.md`
- Strictness and commits: `docs/rules/contributing.md`
- Test conventions: `docs/rules/testing.md`
- System design: `docs/architecture.md`
- Validating a change: `.claude/skills/lt-check/SKILL.md`

## Posture (do this before typing)

- Think first. State assumptions; if multiple interpretations exist, surface
  them — don't pick silently. Push back when a simpler approach exists.
- Simplicity first: the minimum code that solves the problem. No speculative
  abstractions, config, or error handling for impossible states.
- Surgical changes: touch only what the task requires. Don't "improve" adjacent
  code or refactor what isn't broken. Match existing style. Remove only the
  orphans your own change creates; mention pre-existing dead code, don't delete
  it.
- This is 0.1.x: breaking compatibility is fine when it makes the design more
  correct or simpler. Prefer the direct idiomatic Rust design over compat shims.

## Strict lints (they are denied, not warned)

- Panic-safety: no `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`,
  `dbg!`, `print*` in non-test code. Propagate with `anyhow` and `?`. Tests may
  use them freely.
- `clippy::all` / `pedantic` / `cargo` are `deny`. Do not silence a lint with
  `#[allow(...)]` without a one-line justification comment **and** user
  approval. When a class of mistake could be caught by a stricter setting, add
  the setting rather than fixing instances one by one.
- Route diagnostics through `tracing`; user-facing output lives in the
  presentation layer.

## Workspace and module boundaries

- Virtual workspace, edition 2024, `resolver = "3"`. Crates use
  `dep.workspace = true` and `[lints] workspace = true`.
- Dependencies: a dep used by more than one crate lives in
  `[workspace.dependencies]`; a **single-consumer** dep is declared inline in
  the crate that uses it. `cargo machete` enforces "no unused deps".
- Respect the layering: the Linear API is reached only by `lt-upstream`; the TUI
  touches only the local DB. `lt-tui` must not depend on `lt-upstream` or
  `cynic`. `cynic` is confined to `lt-types`. Cross-layer wiring goes through
  ports/adapters (e.g. the `SyncService` port), injected by `lt-cli`.
- Prefer domain-scoped modules and re-exports over thin wrapper functions
  (`upstream::teams::fetch()`, not a `fetch_teams` shim).

## Refactors: let the compiler find usages

When renaming or moving items, make the rename and then run the build; the
compiler enumerates every broken reference. Do not grep for call sites — the
type checker is exhaustive and grep is not.

When splitting a module, keep shared generic helpers in one place (a private
helper module). Copy-pasting the shared logic into each new module re-introduces
clones that `cpd` and `cargo dupes` will reject.

## Tests

- Inline in a `#[cfg(test)] mod` in the same file (or a `#[path]`-attached
  `*_tests.rs` sibling). There is no `tests/` directory — tests assert on
  private seams.
- Rendering surfaces use `insta::assert_snapshot!`; snapshots are named by
  `module_path!()`, so renaming a crate/module renames its snapshots.
- Keep tests deterministic: thread wall-clock and other ambient inputs in as
  explicit parameters. Seeded data comes from the `sim` generator, gated
  `#[cfg(all(test, feature = "sim"))]`.

## After writing: validate, then commit

- Validate with the gate, not ad-hoc cargo: run `make check` and `make test`
  through the nix devshell (see `.claude/skills/lt-check/SKILL.md`), or delegate
  to the `lt-check-runner` agent. Format through the **nightly** devshell — a
  stable `rustfmt` silently skips the workspace's import-grouping rules and the
  push then fails CI.
- Commits are conventional: `<type>(<scope>): <subject>`. A commit that closes a
  Linear issue ends with `Closes: ENG-XXX`; partial progress uses
  `Refs: ENG-XXX` (one trailer per issue).
