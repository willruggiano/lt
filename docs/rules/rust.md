---
paths:
  - "**/*.rs"
---

# Rust conventions

- Configuration is the source of truth (`Cargo.toml`, `clippy.toml`, etc). Do
  not duplicate configuration values into documentation, per
  [[documentation.md]]
- Do not silence a lint with `#[allow(...)]` without a one-line justification
  comment _and_ user approval (per [[contributing.md#Strictness]]).
- When a class of mistake could be caught by a stricter setting or an extra
  lint, add it rather than fixing instances one by one.
- `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, and `dbg!` are denied
  in non-test code; propagate errors with `Result` and `?` (the crate uses
  `anyhow`). Tests may use them freely.
- Route diagnostics through `tracing`. User-facing command output is the
  exception and lives in the presentation layer.
- Split the code rather than raising a budget.
