---
paths:
  - "**/*.rs"
---

# Rust conventions

- Configuration is the source of truth (`Cargo.toml`, `clippy.toml`, etc). Do
  not duplicate configuration values into documentation, per
  [[documentation.md]].
- Lint strictness — when to add a lint, when silencing one is acceptable — is
  governed by [[contributing.md#Strictness]]. A permitted `#[allow(...)]`
  carries a one-line justification comment and must be explicitly accepted by
  the user.
- `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, and `dbg!` are denied
  in non-test code; propagate errors with `Result` and `?` (the crate uses
  `anyhow`). Tests may use them freely.
- Route diagnostics through `tracing`. User-facing command output is the
  exception and lives in the presentation layer.
- Propagate fallibility, do not swallow it.
- Split the code rather than raising a budget.
- Prefer implementing traits over helper functions eg. `impl From<T> for V`
  rather than `convert_v_to_t(V) -> T`.
- Code should ideally _not require_ comments -- it should be self explanatory.
  Conversely, comments should not duplicate the code they annotate. Therefore,
  code comments should rarely be used, and only in places where they provide
  justifiable value and clarity.
- No breadcrumbs or tombstones in comments:
  - Don't: include pointers when moving or refactoring code (eg. "This was moved
    to foo/bar/lib.rs", "This used to be blah blah in version 1")
  - Don't: reference documentation without a full citation (eg. "Blah blah blah
    (phase 1)" -> "Blah blah blah [[design-doc.md#Phase 1]]")
  - Don't: reference individual stages or phases of an implementation plan in
    code comments, ever. Don't say "Stage 1 adds this. Stage 2 adds that."
  - Do: keep comments at the same level of abstraction as the code they are
    annotating. Generally speaking, comments should only describe the code they
    annotate, and never reference code that exists in other modules, calls the
    annotated code, or otherwise exists above or below the annotated code in the
    codebase architecture, even if related or part of the same system.
