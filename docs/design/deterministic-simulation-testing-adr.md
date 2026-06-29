# Deterministic Simulation Testing (ENG-18)

## Status

Accepted

## Context

`lt` needs fake, generated data driven by deterministic RNG so we can:

- run rendering/visual tests against a known dataset (ENG-19),
- let a coding agent "test drive" the app mid-session without a Linear account,
- build a seeded CLI and browse locally with no auth configured.

Requirements from ENG-18: knobs for **seed** and **dataset size**; datasets are
**generated, not hand-rolled**; the capability is a **cargo feature** compiled
into both tests _and_ the CLI.

### Where the seam goes

`lt` is local-first. Every read path queries SQLite; only _populating_ the DB
touches Linear:

```text
   Linear GraphQL API ──(OAuth token)──> sync::{full,delta} ──┐
                                                               ├─upsert─> SQLite
   seed, size ─────────> sim::generate() ──> Dataset ─────────┘            │
                         (no network, no token)                            │
                                                          query (no token) │
                                            list / TUI / search / inbox <──┘
```

The only thing tying the app to a real account on the read path is the DB being
empty. So the seam is the **DB-population boundary**: `sim` is a second
populator alongside `sync`, producing the same `db::Issue` / `db::Comment` rows
the network sync produces.

Rejected alternative — a fake GraphQL transport behind `linear::client`: it
would have to fake pagination, cursors, and the response envelope, yet still not
help rendering tests (which read SQLite, not the client). Seeding the DB is the
smaller seam and exercises the exact code path the TUI uses.

## Decision

Add a `sim` cargo feature (`[features] sim = []`, no new dependencies — `rand`
is already required by auth). When enabled it compiles `src/sim/` into both the
binary and the test binary.

### Generator

`sim::generate(seed: u64, size: usize) -> Dataset` is **pure and
deterministic**:

- RNG is `StdRng::seed_from_u64(seed)` — portable/reproducible across platforms
  (unlike `SmallRng`).
- No wall clock: timestamps derive from a fixed base (`2026-01-01T00:00:00Z`)
  plus seeded offsets, with `updated_at >= created_at`.
- Records are templated from word lists (verbs/adjectives/nouns, teams, users,
  states, priorities, labels, projects), not hand-written.
- Referential integrity holds by construction: per-team sequential identifiers
  (`ENG-1`, `ENG-2`, …); `parent_id` only ever points at an earlier issue on the
  same team; every comment's `issue_id` exists.

Same `(seed, size)` ⇒ byte-identical issues and comments.

### CLI surface

```text
lt sim --seed <u64> --size <usize>   # defaults: seed=0, size=100
```

`sim::run` generates a dataset, upserts it into the active profile's DB, marks
the cache fresh (`last_synced_at`) so the offline list/TUI serve it without a
network sync, and records a `viewer_name` so `--assignee=me` resolves.

Reads are token-free, so browsing works offline. Mutations (state/priority/
assignee changes, new issue, posting comments) still require a token and report
"Not logged in" — out of scope for ENG-18.

## Consequences

- A coding agent or developer runs
  `cargo run --features sim -- sim --seed N --size M` then `lt issues` /
  `lt tui` with no Linear account.
- Tests call `generate()` directly for property tests, or seed an in-memory
  SQLite DB for rendering tests (ENG-19).
- The feature is off by default; the standard build and CLI are unchanged.
