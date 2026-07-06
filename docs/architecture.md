# Architecture

`lt` is a local-first [Linear](https://linear.app) TUI. Reads query a local
SQLite cache and never touch the network; writes apply optimistically and a
background thread reconciles them against Linear's GraphQL API. The search bar's
filter grammar is generated at build time from a snapshot of Linear's GraphQL
schema, so filter keys stay in sync with the API. This document describes the
design.

## Toolchain

- Rust, edition 2024 (nightly toolchain).
- [ratatui](https://github.com/ratatui/ratatui) +
  [crossterm](https://github.com/crossterm-rs/crossterm) for the TUI.
- [rusqlite](https://github.com/rusqlite/rusqlite) (bundled SQLite, FTS5) for
  the local cache.
- [ureq](https://github.com/algesten/ureq) for blocking HTTP to Linear's GraphQL
  API.
- [chumsky](https://github.com/zesterer/chumsky) for the search-query parser.
- [clap](https://github.com/clap-rs/clap) for the CLI.
- `tracing` + `tracing-subscriber` + `tracing-appender` for structured logging.
- [graphql-parser](https://github.com/graphql-rust/graphql-parser) +
  `quote`/`syn`/`prettyplease` in `crates/lt-schema-codegen` (a build dependency
  driven by each consuming crate's `build.rs`) for schema-driven codegen.
- Makefile as the task entrypoint (`make build`, `make check`, `make test`,
  `make fix`).
- Nix flake for reproducible builds and `nix run`; see [[nix.md]].

## System Design

### Workspace layout

A Cargo workspace of eight crates under `crates/`:

```text
  lt-types ────────── vocabulary: GraphqlOperation, cynic fragment types,
  │                   typed inputs and the IssueFilter/sort vocabulary, Clock
  lt-config ───────── profiles, credentials, XDG state paths
  lt-schema-codegen ─ build-time codegen library (build dependency of
  │                   lt-types and lt-storage)
  lt-storage ──────── the SQLite store: migrations, statement registry,
  │                   Read/Upsert impls, search-query parser, sim generator
  lt-upstream ─────── the API edge: GraphqlTransport, execute::<Op>, OAuth
  lt-runtime ──────── composes storage + upstream: Runtime, the load/refresh/
  │                   subscribe drivers, the sync engine
  lt-tui ──────────── ratatui event loop, views, layouts
  lt-cli ──────────── clap dispatch, output formatting, logging
```

`lt-tui` depends only on `lt-runtime` and `lt-types`; `lt-runtime` re-exports
the store facade, so the TUI and CLI reach both the store and the API edge
through it alone.

### Data flow

`lt` is local-first: every read path queries SQLite; only populating the cache
touches Linear.

```text
  Linear GraphQL API ──(OAuth token)──> sync / refresh ──┐
  (crates/lt-upstream)          (crates/lt-runtime/src/sync)
                                                         ├─upsert─> SQLite
  sim::generate ──────> Dataset ─────────────────────────┘  (crates/lt-storage)
  (feature "sim",       (no network, no token)                     │
   crates/lt-storage/src/sim.rs)                     query (no token)
                                    list / search / inbox / TUI <──┘
                                    (crates/lt-cli, crates/lt-tui)
```

The CLI is dispatched in `crates/lt-cli/src/main.rs`: `auth`, `issues`, `tui`,
`inbox`, `sync`, `search`, and (under the `sim` feature) `sim`. Profile
selection, logging init, and DB open all happen before any subcommand runs.

### The operation seam

The GraphQL operation type is the single vocabulary on both sides of the cache:
every read, refresh, and view data contract is an operation plus its typed
variables. Upstream, `execute::<Op>(transport, variables)` runs any operation
against Linear (`crates/lt-upstream/src/client.rs`). Locally, each operation
implements `Read` (SQL over the replica, plus the `reads` entity set it depends
on) and `Upsert` (write a response into the cache, report the `EntityKey`s it
touched) in `crates/lt-storage/src/db/ops.rs`. `lt-runtime` provides the generic
drivers:

```text
  load::<Op>       = read                          one-shot cached read
  refresh::<Op>    = upsert ∘ execute              upstream → cache
  subscribe::<Op>  = read + live typed slot        view data, kept current

  any upsert ──touched EntityKeys──> Runtime::propagate:
    for each live subscription, reads ∩ touched ≠ ∅ → re-read, fill slot,
    emit payload-free RuntimeEvent::Updated(key)
```

The TUI holds a concrete `Runtime` (`crates/lt-runtime/src/runtime.rs`); data
crosses to views only through typed `Subscription` slots
(`crates/lt-runtime/src/subscription.rs`). Design and rationale:
[[operation-seam-adr.md]].

### Profiles and state layout

Each `--profile` (or `$LT_PROFILE`, default `default`) gets isolated credentials
and database, so accounts/workspaces never share state and can run concurrently
(`crates/lt-config/src/lib.rs`). Paths are XDG-based and per-profile:

- `$XDG_STATE_HOME/lt/profiles/<name>/auth.json` — OAuth token, mode `0600`.
- `$XDG_STATE_HOME/lt/profiles/<name>/logs/` — daily-rotated logs.
- `$XDG_DATA_HOME/lt/profiles/<name>/lt.db` — SQLite cache.

OAuth application credentials (`LINEAR_CLIENT_ID`/`LINEAR_CLIENT_SECRET` or
`$XDG_CONFIG_HOME/lt/config.json`) are shared across profiles.

### Storage

A single SQLite file holds the issue replica (`issues`, its FTS5 index
`issues_fts` kept in sync by triggers, `issue_comments`), the reference data the
replica joins against (`teams`, `users`, `workflow_states`, `projects`,
`cycles`, `labels`, `issue_labels`, `team_memberships`, `organizations`), the
optimistic write path (`pending_overlay`, `outbox`), and a `sync_meta` key/value
table (e.g. `last_synced_at`). Versioned migrations (SQLite's `user_version`,
via `rusqlite_migration`) run on open (`crates/lt-storage/src/db/mod.rs`). All
statement text lives in a registered statement module
(`crates/lt-storage/src/db/sql.rs`) whose entries the test gate prepares against
the migrated schema; see [[type-safe-sql-adr.md]]. Query and upsert helpers live
under `crates/lt-storage/src/db/`.

### Authentication

OAuth 2.0 against Linear (`crates/lt-upstream/src/auth/`). `lt auth login` runs
a local redirect on `http://localhost:7342/callback`; the token is persisted by
`lt-config`. `crates/lt-upstream/src/auth/refresh.rs` reloads or refreshes the
token before each networked operation. Login can also run non-interactively from
the TUI's background re-auth path.

### Sync

`crates/lt-runtime/src/sync/` paginates Linear's `issues` connection and upserts
each page into SQLite (`sync_pages` in `crates/lt-runtime/src/sync/mod.rs`),
then stamps `last_synced_at`:

- **full** — fetch every issue (`crates/lt-runtime/src/sync/full.rs`).
- **delta** — fetch issues with `updatedAt >` `last_synced_at`; falls back to
  full when no prior sync exists (`crates/lt-runtime/src/sync/delta.rs`).
- **probe** — test whether a token is accepted by the API
  (`crates/lt-runtime/src/sync/probe.rs`).

Both full and delta first drain the mutation outbox
(`crates/lt-runtime/src/sync/drain.rs`), replaying queued local edits and
creates against the API before fetching, so all base writes are serialized
through the sync thread. They also persist the viewer identity and the reference
data (teams, workflow states) before issue pages. Every step returns the
`EntityKey`s it touched; the runtime propagates them to live subscriptions (see
[the operation seam](#the-operation-seam)).

`lt` deliberately polls the public GraphQL API rather than Linear's browser-only
sync engine; the README's "Why not Linear's sync protocol?" records the
rationale.

### Search and the codegen seam

Each consuming crate's `build.rs` reads a hand-maintained allowlist
(`build/search_filter_fields.toml`) and a snapshot of Linear's schema
(`build/linear-schema-definition.graphql`), validates every allowlisted filter
and sort field against `IssueFilter`/`IssueSortInput` via `lt-schema-codegen`,
and emits a parser into `OUT_DIR`. A schema/allowlist mismatch fails the build.
Rationale and the parser design are in [[search-parser-v2.md]] and its ADR
[[search-parser-v2-adr.md]]; the filter-expansion model is in
[[search-codegen-and-filter-expansion-adr.md]].

Two front ends consume the grammar: the `lt search` command runs FTS5 queries
against `issues_fts` (`crates/lt-cli/src/search.rs`), and the TUI search bar
parses `key:value` stems plus free-text into a query AST
(`crates/lt-storage/src/search_query.rs`). The AST is the single source of truth
for TUI filter state (see [[unified-filter-state.md]]) and lowers into the typed
`IssueFilter`, the one filter-to-SQL path
(`crates/lt-storage/src/db/filters.rs`).

### TUI

`crates/lt-tui` is a single-threaded render/event loop (`run` in
`crates/lt-tui/src/lib.rs`). One long-lived `mpsc::channel<AppEvent>` carries
key input and every runtime event, so the loop is a single blocking wait
([[tui-app-event-queue-adr.md]]). The UI thread only ever touches SQLite and
in-memory state; networked work — sync, mutations, refreshes, login — runs on
runtime worker threads that report back onto the same queue.

```text
  [input thread] ──────────── Key ───────────┐
  [runtime loop + workers] ── Updated(key) ──┼──> App.events_rx ── App::apply
  (sync, login, refresh)      Lifecycle(..) ─┘
```

Each view owns a `Subscription` to its operation; when a write or sync touches
the entities that operation reads, the runtime re-reads and fills the slot, and
the view consumes it with `take` (latest-or-nothing).

Writes never touch the network from the UI thread. An edit writes its intent to
the local DB and returns: a `pending_overlay` row plus a command in the `outbox`
table, committed together. The read model is `merge(base, overlay)` — the
overlay wins per field — so the edit renders immediately without a base write,
and a concurrent delta sync (which writes only the base) cannot clobber it. The
sync drainer (`crates/lt-runtime/src/sync/drain.rs`) is the single writer that
replays the outbox against the API and reconciles the base on success. This
split, the typed inputs, and the offline outbox are documented in
[[linear-api-types-codegen.md]]; the modal redesign in [[tui-modal.md]].

### Logging

`tracing` with a daily-rotated file appender under the profile's `logs/`
directory (`crates/lt-cli/src/logging.rs`). TUI mode logs only to file so
stdout/stderr never corrupt the display; CLI mode also mirrors INFO to stdout.
Default filter is DEBUG for the `lt` crate, WARN elsewhere; override with
`RUST_LOG`. `panic!`, `unwrap`, and `expect` are denied in non-test code (see
[[rust.md#Panic safety]]).

### Testing and simulation

The `sim` feature compiles a deterministic, seeded fake-data generator
(`crates/lt-storage/src/sim.rs`) into both the test binaries and the CLI
(`lt sim`, `crates/lt-cli/src/sim.rs`), so the app can be driven with no Linear
account or network. Design and the data seam are in [[dst.md]]. Test procedures
and conventions are in [[testing.md]]; the coverage gate and its ratchet in
[[test-coverage-gate.md]].

### Build, run, deploy

`make` lists targets; the Makefile is the source of truth for build, lint, and
test workflows. Strictness gates (fmt, clippy, `cargo deny`, `cargo machete`,
copy/paste detection) run under `make check`. Setup, conventions, and the
strictness posture are in [[contributing.md]]; engineering principles are in
[[posture.md]].
