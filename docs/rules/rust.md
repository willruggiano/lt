---
paths:
  - "**/*.rs"
---

# Rust conventions

These rules are enforced by the toolchain, not just convention. The lint set is
the source of truth; see `Cargo.toml` (`[lints]`), `clippy.toml`, `rustfmt.toml`,
and `deny.toml`. See [[contributing.md#Strictness]] for the strictness posture.

## Lints

- The build runs under `clippy::all`, `clippy::pedantic`, and `clippy::cargo`,
  all denied. Warnings are errors (`-D warnings`).
- Do not silence a lint with `#[allow(...)]` without a one-line justification
  comment _and_ user approval (per [[contributing.md#Strictness]]).
- When a class of mistake could be caught by a stricter setting or an extra
  lint, add it rather than fixing instances one by one.

## Panic safety

- `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, and `dbg!` are denied
  in non-test code. Propagate errors with `Result` and `?` (the crate uses
  `anyhow`). Tests may use these freely.
- `print!`/`println!`/`eprint!`/`eprintln!` are denied; route diagnostics through
  `tracing`. User-facing command output is the exception and lives in the
  presentation layer.

## Complexity budgets

Thresholds live in `clippy.toml`, mirroring the sibling backends:

- cognitive complexity: 20
- function length: 80 lines
- function arguments: 4
- nesting depth: 5

Split the code rather than raising a budget.

## Formatting and imports

- `cargo fmt` is authoritative (`rustfmt.toml`): edition 2024, module-granular
  imports, grouped std / external / crate.

## Dependencies

- `cargo deny` gates advisories, licenses, and sources; `cargo machete` rejects
  unused dependencies. Both run in CI.
