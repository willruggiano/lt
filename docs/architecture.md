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
  `quote`/`syn`/`prettyplease` in `build.rs` for schema-driven codegen.
- Makefile as the task entrypoint (`make build`, `make check`, `make test`,
  `make fix`).
- Nix flake for reproducible builds and `nix run`; see [[nix.md]].

## System Design

### Data flow

`lt` is local-first: every read path queries SQLite; only populating the cache
touches Linear.

```text
  Linear GraphQL API ──(OAuth token)──> sync::{full,delta} ──┐
  (src/linear/client.rs)                (src/sync)           ├─upsert─> SQLite
                                                             │         (src/db)
  sim::generate ──────> Dataset ─────────────────────────────┘            │
  (feature "sim",       (no network, no token)                            │
   src/sim)                                                  query (no token)
                                          list / search / inbox / TUI <───┘
                                          (src/issues, src/search,
                                           src/inbox, src/tui)
```

The CLI is dispatched in `src/main.rs`: `auth`, `issues`, `tui`, `inbox`,
`sync`, `search`, and (under the `sim` feature) `sim`. Profile selection,
logging init, and DB open all happen before any subcommand runs.

### Profiles and state layout

Each `--profile` (or `$LT_PROFILE`, default `default`) gets isolated credentials
and database, so accounts/workspaces never share state and can run concurrently
(`src/config.rs`). Paths are XDG-based and per-profile:

- `$XDG_STATE_HOME/lt/profiles/<name>/auth.json` — OAuth token, mode `0600`.
- `$XDG_STATE_HOME/lt/profiles/<name>/logs/` — daily-rotated logs.
- `$XDG_DATA_HOME/lt/profiles/<name>/lt.db` — SQLite cache.

OAuth application credentials (`LINEAR_CLIENT_ID`/`LINEAR_CLIENT_SECRET` or
`$XDG_CONFIG_HOME/lt/config.json`) are shared across profiles.

### Storage

A single SQLite file holds the `issues` table, its FTS5 index (`issues_fts`,
kept in sync by triggers), `issue_comments`, and a `sync_meta` key/value table
(e.g. `last_synced_at`). Schema creation and additive column migrations run on
every open (`src/db/mod.rs`); there is no migration-version table — columns are
added if absent. Query and upsert helpers live under `src/db/`.

### Authentication

OAuth 2.0 against Linear (`src/auth/`). `lt auth login` runs a local redirect on
`http://localhost:7342/callback`; the token is persisted by `src/config.rs`.
`src/auth/refresh.rs` reloads or refreshes the token before each networked
operation. Login can also run non-interactively from the TUI's background
re-auth path.

### Sync

`src/sync/` paginates Linear's `issues` connection and upserts each page into
SQLite (`sync_pages` in `src/sync/mod.rs`), then stamps `last_synced_at`:

- **full** — fetch every issue (`src/sync/full.rs`).
- **delta** — fetch issues with `updatedAt >` `last_synced_at`; falls back to
  full when no prior sync exists (`src/sync/delta.rs`).
- **probe** — test whether a token is accepted by the API (`src/sync/probe.rs`).

`lt` deliberately polls the public GraphQL API rather than Linear's browser-only
sync engine; the README's "Why not Linear's sync protocol?" records the
rationale.

### Search and the codegen seam

`build.rs` reads a hand-maintained allowlist (`build/search_filter_fields.toml`)
and a snapshot of Linear's schema (`build/linear-schema-definition.graphql`),
validates every allowlisted filter and sort field against
`IssueFilter`/`IssueSortInput`, and emits a parser into `OUT_DIR`. A
schema/allowlist mismatch fails the build. Rationale and the parser design are
in [[search-parser-v2.md]] and its ADR [[search-parser-v2-adr.md]]; the
filter-expansion model is in [[search-codegen-and-filter-expansion-adr.md]].

Two front ends consume the grammar: the `lt search` command runs FTS5 queries
against `issues_fts` (`src/search.rs`), and the TUI search bar parses
`key:value` stems plus free-text into a query AST (`src/tui/search_query.rs`).
The AST is the single source of truth for TUI filter state (see
[[unified-filter-state.md]]).

### TUI

`src/tui/` is a single-threaded render/event loop (`run` in `src/tui/mod.rs`).
The UI thread only ever touches SQLite and in-memory state. Every networked
action — sync, mutations, modal data loads, comment posting, login — runs on a
spawned thread and reports back over an `mpsc` channel that the loop drains with
`try_recv` each frame.

```text
  [event loop] ──spawn──> [worker thread] ──HTTP──> Linear
       ^                        │
       └────── mpsc ────────────┘   (poll_* drains channels per frame)
```

Writes are optimistic: the local SQLite row and in-memory state are updated
immediately, the mutation fires on a worker thread, and the optimistic change is
reverted (by reloading from the cache) if the mutation fails. The modal redesign
is documented in [[tui-modal.md]].

### Logging

`tracing` with a daily-rotated file appender under the profile's `logs/`
directory (`src/logging.rs`). TUI mode logs only to file so stdout/stderr never
corrupt the display; CLI mode also mirrors INFO to stdout. Default filter is
DEBUG for the `lt` crate, WARN elsewhere; override with `RUST_LOG`. `panic!`,
`unwrap`, and `expect` are denied in non-test code (see
[[rust.md#Panic safety]]).

### Testing and simulation

`make test` runs the suite with and without the `sim` feature. The `sim` feature
compiles a deterministic, seeded fake-data generator into both the test binaries
and the CLI (`lt sim`), so the app can be driven with no Linear account or
network. Design and the data seam are in [[dst.md]].

### Build, run, deploy

`make` lists targets; the Makefile is the source of truth for build, lint, and
test workflows. Strictness gates (fmt, clippy, `cargo deny`, `cargo machete`,
copy/paste detection) run under `make check`. Setup, conventions, and the
strictness posture are in [[contributing.md]]; engineering principles are in
[[posture.md]].
