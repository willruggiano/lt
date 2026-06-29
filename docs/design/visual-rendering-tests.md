---
issue: https://linear.app/willruggiano/issue/ENG-19/visual-rendering-tests
---

# Visual / Rendering Tests

## Context

`lt` has three render surfaces and almost no coverage of any of them:

| Surface          | Entry point                                                    | Output         | Coverage today         |
| ---------------- | -------------------------------------------------------------- | -------------- | ---------------------- |
| TUI frame        | `tui::ui::render(frame, &mut App)` (`src/tui/ui.rs`)           | ratatui buffer | none                   |
| CLI tables       | `issues::display::print_table*`, `inbox::display::print_table` | text           | inbox date helper only |
| Markdown → lines | `tui::markdown::render` (`src/tui/markdown.rs`)                | `Vec<Line>`    | done (line/span tests) |

This doc covers the first two. Markdown rendering is already tested per-span and
is out of scope.

The data comes from [[dst.md]]: `sim::generate(seed, size)` produces a
deterministic `Dataset` of `db::Issue`/`db::Comment` rows. ENG-19 renders a
known seeded dataset and asserts the output.

### The render seam

`ui::render` is pure with respect to IO: it reads `App` fields and draws. It
never opens the database or spawns threads. All DB/network coupling lives in
`App`'s action methods (`do_fetch`, `run_search`, `open_detail`), which call
`db::open_db()` — a process-global profile path (`src/config.rs` `OnceLock`).

```text
            ACTION PATH  (DB + threads, NOT under test)
   key ─▶ handle_*_key ─▶ App::do_fetch / run_search / open_detail
                                  │  db::open_db()  ← global path, threads
                                  ▼
                         App.issues / App.detail / App.mode     ← plain data
                                  │
            RENDER PATH  (pure, UNDER TEST)
   frame ◀── ui::render(frame, &mut App) ◀─────────┘
            reads fields, draws widgets, no IO
```

The tests populate `App` state directly, skip the action methods, and call
`ui::render` into an in-memory backend. No DB, no threads, no profile global.

## Decision

### TUI harness — ratatui `TestBackend`

ratatui 0.30 ships the harness (`ratatui::backend::TestBackend`, re-exported
from `ratatui-core`). A test draws one frame and asserts the resulting buffer,
including per-cell style:

```rust
let mut term = Terminal::new(TestBackend::new(80, 24))?;
let mut app = App::for_test(issues, Mode::List);
term.draw(|f| ui::render(f, &mut app))?;
// insta snapshot of the rendered buffer (see "Assertions").
insta::assert_snapshot!(term.backend());
```

### CLI harness — `Write` capture

`print_table*` already take `&mut dyn Write`. Capture into a `Vec<u8>` and
snapshot the string. Issues are driven from a seeded `sim` dataset (converted
with the existing `db_issue_to_list_issue`) or fed as `db::Issue` directly into
`print_table_cached`.

### Construction seam — `App::for_test`

`App::new` is private and requires a live `SyncState` channel. Add a test-only
`#[cfg(test)] fn for_test(...) -> App` in `tui/mod.rs` that fills the struct
with `sync_rx: None` and spawns no threads. Tests live in the `tui` module, so
private access needs no wider visibility. No production surface grows.

### Determinism — inbox clock seam

The TUI list/detail path has no wall-clock dependency: `date()` slices a fixed
ISO prefix from `sim`'s fixed `BASE_SECS` (`2026-01-01`). Deterministic.

`lt inbox` is not: `inbox::display::relative_age` calls `SystemTime::now()` on
every render, so the AGE column flaps. Fix by threading an explicit `now: i64`
(unix seconds) into `print_table`/`relative_age`; the binary passes the real
clock, tests pass a fixed value. This is a clock seam — explicit dependency
wiring per [[posture.md]] — not a test-only shim.

```text
   binary  ─▶ print_table(out, &n, now_unix_secs())
   test    ─▶ print_table(out, &n, FIXED_NOW)        ← stable AGE column
```

### Assertions — `insta` snapshots

Use `insta` for both surfaces. Full-frame TUI buffers (80×24) and multi-row CLI
tables are too large to inline as expected literals without noise, and
`insta accept` makes intentional layout changes a one-command review.

Cost: `insta` adds dev-dependencies that must clear the supply-chain gate
(`cargo deny`, see [[contributing.md#Strictness]]) and `cargo machete`. It is a
`[dev-dependencies]` entry, so it never ships in the binary and does not affect
the default or `sim` runtime builds.

### Feature gating

Tests that call `sim::generate` are gated `#[cfg(all(test, feature = "sim"))]`
(the capability is a cargo feature per [[dst.md]]). `make test` already runs
both `cargo test` and `cargo test --features sim`, and `make check` lints
`--features sim`; no CI change is needed. Pure-fixture render tests (no `sim`)
need no gate.

## Coverage

```text
TUI (TestBackend + insta)                 CLI (Write capture + insta)
 ├─ list: header + table + footer          ├─ issues::print_table (sim seed)
 ├─ list: empty ("No issues found.")       ├─ issues::print_table empty
 ├─ list: sort marker on active column     ├─ issues::print_table_cached + note
 ├─ detail overlay: meta/labels/children   └─ inbox::print_table (fixed now)
 ├─ detail: markdown description block
 ├─ popup (state/priority) border + anchor
 ├─ new-issue modal field pickers
 ├─ help popup search/filter
 └─ search overlay results table
```

TUI cases seed from one or two fixed `sim` seeds for stable, realistic data.

## Consequences

- New `[dev-dependencies]` entry (`insta`) gated through
  `cargo deny`/`cargo machete`; snapshots live under `src/**/snapshots/`.
- One production change: `inbox::display::print_table`/`relative_age` gain a
  `now: i64` parameter (clock seam).
- One test-only addition: `App::for_test` in `tui/mod.rs`.
- `ui::render` stays IO-free; if a future change makes it touch the DB, these
  tests break loudly — a useful guard on the render/action boundary.

## Rejected alternatives

- **Inline expected buffers** (`TestBackend::assert_buffer_lines`): zero new
  deps, but full-screen literals are unreadable and every layout tweak rewrites
  them by hand. `insta accept` is worth the dependency cost.
- **Exclude the AGE column instead of a clock seam**: leaves part of the inbox
  surface untested and bakes a gap into the assertion. The seam is ~10 lines and
  makes the whole surface deterministic.
- **Driving tests through `App`'s action methods**: would require faking the
  profile global and the DB on the read path. The render seam avoids all of it.
