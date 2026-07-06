---
paths:
  - "**/*.rs"
---

# Test conventions

Procedures for running tests and conventions for writing them. Design rationale
lives in the linked ADRs; this document records the rules. The strictness
posture is in [[contributing.md#Strictness]]; engineering principles in
[[posture.md]].

## Running

- `make test` runs the suite twice: `cargo test` (the shipped, feature-off
  build), then `cargo test --all-features` (which enables `sim`). Both must
  pass: the two runs exercise distinct compile configurations, so neither alone
  is sufficient.
- `make cov` enforces the line-coverage floor; `make cov-html` writes a
  browsable report for finding gaps. The gate and its ratchet are in
  [[test-coverage-gate.md]].
- Tests touch no network and no real profile or database state. Offline data
  comes from the `sim` generator (below); render and CLI tests construct their
  inputs directly.

## Layout

- Default: tests live in the same file as the code under test, inside a
  `#[cfg(test)] mod` block, so they can exercise private items. This is the
  idiomatic Rust default
  ([The Rust Programming Language, ch. 11.3](https://doc.rust-lang.org/book/ch11-03-test-organization.html)).
- There is no `tests/` directory. Files under `tests/` compile as separate
  crates and reach only the public API; our tests assert on private seams (the
  render path into a buffer, private `App` and parser internals), so they must
  stay crate-internal. Rule of thumb: a test that needs a private item is
  inline; only public-API behavior would belong under `tests/`, and we expose no
  such surface.
- When a module's inline tests dominate its file size, the `#[cfg(test)] mod`
  may move to an in-crate sibling `*_tests.rs` file, which keeps private access:
  - from a `mod.rs`: `#[cfg(test)] mod foo_tests;` resolving to `foo_tests.rs`;
  - from any other file: `#[cfg(test)] #[path = "foo_tests.rs"] mod foo_tests;`
    — the `#[path]` keeps it a child module; a bare `mod` would be a sibling and
    see only `pub` items
    ([Rust Reference: Modules](https://doc.rust-lang.org/reference/items/modules.html)).

  This is a readability lever only: the `#[cfg(test)]` gate already keeps tests
  out of `cargo build`, so moving them changes no compile time. Prefer inline
  for normally-sized modules.

- Shared fixtures and helpers go at the top of the test module — e.g. `draw`,
  `sim_issues`, `app_with_issues` in `crates/lt-tui/src/render_tests.rs`.
- A test that needs the seeded data generator is gated
  `#[cfg(all(test, feature = "sim"))]`, so it compiles only under the
  `--features sim` run.

## What to test

- Exercise behavior through the seams a module exposes, not its internals: the
  render path (`ui::render` into a buffer), the `Write` sink that `print_table*`
  accept, an explicit clock parameter. The seam design is in
  [[visual-rendering-tests.md]].
- Drive realistic, broad input coverage from deterministic, seeded `sim`
  datasets (`sim::generate(seed, size)`) rather than hand-built fixtures where
  the shape of real data matters. The simulation model is in [[dst.md]].
- Keep tests deterministic. Wall-clock and other ambient inputs are threaded in
  as explicit parameters (e.g. `relative_age(iso, now_secs)`): the binary
  supplies the real value, the test a fixed one. This is dependency wiring per
  [[posture.md]], not a test-only shim.

## Snapshots

- Rendering surfaces (TUI buffers, CLI tables, markdown) are asserted with
  `insta::assert_snapshot!`. Snapshots live in `src/**/snapshots/`; intentional
  changes are reviewed and accepted with `cargo insta accept`.
- `insta` is a `[dev-dependencies]` entry: it never ships in the binary and must
  clear the supply-chain gates (see [[contributing.md#Strictness]]).

## Panic safety

- `unwrap`, `expect`, `panic!`, and `print*` are denied in non-test code but
  allowed in test bodies (`clippy.toml`: `allow-*-in-tests`). Use them freely in
  tests; never in the code under test (see [[rust.md#Panic safety]]).
