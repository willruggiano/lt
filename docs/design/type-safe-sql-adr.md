# Type-Safe SQL Queries (ADR)

## Status

Accepted (ENG-33; design only, implementation pending).

## Context

All production SQL lives in `lt-storage` (`src/db/*.rs`, `src/search_query.rs`),
executed through `rusqlite` 0.40 with the bundled SQLite (`Cargo.toml` workspace
dependency). Two trivial `COUNT(*)` diagnostics live in `lt-cli/src/search.rs`.
Nothing validates any of it before runtime: a renamed column, a typo'd table, or
a drifted column list fails only when the statement first executes -- and some
statements sit on rarely-exercised paths (outbox error recording, the LIKE
fallback search).

The statement surface has three shapes:

```text
fixed text                 fixed template                runtime-composed
-----------------          --------------------------    --------------------------
INSERT/UPDATE/DELETE       SELECT {ISSUE_COLUMNS}        WHERE built clause-by-clause
in outbox.rs,              FROM issues i {ISSUE_JOINS}   from user input:
comments.rs, issues.rs     WHERE <one param>             - filters.rs::build_sql_filter
(~25 statements)           (search/children/by-id,       - search_query.rs::
                            ~6 statements)                   build_conditions/build_sql
```

Specific weaknesses in today's code:

- `issue_from_row` maps 22 result columns by numeric index
  (`crates/lt-storage/src/db/issues.rs:40-111`); it must agree positionally with
  `ISSUE_COLUMNS` (`issues.rs:15-26`). Nothing checks that agreement.
- The filter builder's tests assert the generated clause _text_
  (`crates/lt-storage/src/db/filters.rs:147`), not that the clause is valid
  against the schema. A clause referencing a dropped alias would pass its tests.
- Migrations are hand-rolled idempotent probes against `pragma_table_info`
  (`crates/lt-storage/src/db/mod.rs:51-83`, `191-262`): no version number, every
  open re-probes every column, and the probe list grows monotonically.

Constraints any solution must respect:

- **The workspace is fully synchronous.** HTTP is `ureq`
  (`crates/lt-upstream/Cargo.toml:20`); no crate depends on an async runtime.
- **FTS5 is load-bearing.** A virtual table plus three sync triggers
  (`mod.rs:105-124`) and `MATCH` queries (`issues.rs:423-434`,
  `search_query.rs:438-447`).
- **Tests are offline and hermetic**, against per-test in-memory databases
  (`mod.rs:293`; conventions in [[testing.md]]).
- **`make check` / `make test` are the gate** ([[contributing.md]]).
- **Precedent:** `lt-storage/build.rs` already validates configuration against
  the Linear GraphQL schema at build time
  ([[search-codegen-and-filter-expansion-adr.md]]).

## What "type-safe" can mean on SQLite

SQLite is dynamically typed: columns have type _affinity_, not enforced types
([SQLite: Datatypes](https://www.sqlite.org/datatype3.html)). "Type-safe
queries" therefore decomposes into four independently checkable properties:

| #   | Property                                                 | Failure today                  |
| --- | -------------------------------------------------------- | ------------------------------ |
| P1  | Statement is valid against the schema (syntax, names)    | runtime, first execution       |
| P2  | Bind-parameter count matches the call site               | runtime (fail-fast)            |
| P3  | Result columns agree with the row-mapping code           | runtime, or silent misread     |
| P4  | Column type plausibly matches the Rust type it's read as | runtime coercion, often silent |

P1 is checkable by SQLite itself: compiling a statement resolves every table and
column name without executing anything
([SQLite: prepare](https://www.sqlite.org/c3ref/prepare.html)). Demonstrably --
`EXPLAIN` compiles its inner statement without running it
([SQLite: EXPLAIN](https://www.sqlite.org/lang_explain.html)):

```text
sqlite> CREATE TABLE t(a TEXT);
sqlite> EXPLAIN SELECT nope FROM t;      -- error: no such column: nope
sqlite> EXPLAIN SELECT * FROM missing;   -- error: no such table: missing
```

`rusqlite::Connection::prepare` surfaces exactly these errors, and the prepared
`Statement` exposes `parameter_count()`, `column_count()`, and `column_names()`
without feature flags
([rusqlite: Statement](https://docs.rs/rusqlite/latest/rusqlite/struct.Statement.html)).

P4 is structurally weak on SQLite regardless of tooling: this schema declares
almost every column `TEXT`, so even a checker that compares declared types
against Rust types learns little.

## Options Considered

| Option                        | Sync | P1 fixed SQL | P1 dynamic SQL           | FTS5         | Migrations           | Cost                    |
| ----------------------------- | ---- | ------------ | ------------------------ | ------------ | -------------------- | ----------------------- |
| sqlx `query!`                 | No   | compile time | unchecked                | raw SQL only | `migrate!`           | full rewrite + async    |
| Diesel                        | Yes  | compile time | typed (`into_boxed`)     | unchecked    | `embed_migrations!`  | full DSL rewrite        |
| SeaORM                        | No   | --           | --                       | --           | --                   | rejected with sqlx      |
| rust-query                    | Yes  | compile time | typed                    | unsupported  | owned, unstable      | full rewrite, loses FTS |
| rusqlite + prepare-check gate | Yes  | test gate    | test gate (per fragment) | native       | `rusqlite_migration` | additive                |

### Option 1: sqlx

The issue's named candidate. Rejected on three grounds:

1. **Async-only.** sqlx is "Truly Asynchronous. Built from the ground-up using
   async/await"; consumers must select a `runtime-tokio` / `runtime-async-std`
   feature ([sqlx README](https://github.com/launchbadge/sqlx)). There is no
   sync API. Adopting it means introducing an async runtime into a deliberately
   synchronous workspace, solely to read a local SQLite file. That inverts
   [[posture.md]] ("the direct, idiomatic Rust design").
2. **The checking misses where the risk is.** `query!` macros accept only
   literal SQL; runtime-composed statements go through `QueryBuilder`, whose
   docs state "It is your responsibility to ensure that you produce a
   syntactically correct query here, this API has no way to check it for you"
   ([sqlx: QueryBuilder](https://docs.rs/sqlx/latest/sqlx/struct.QueryBuilder.html)).
   Our highest-risk SQL is precisely the dynamic filter and search composition;
   it would remain unchecked.
3. **Stateful build input.** The macros need `DATABASE_URL` pointing at a
   prepared dev database at build time, or a committed `.sqlx` offline cache
   maintained via `cargo sqlx prepare`
   ([sqlx README](https://github.com/launchbadge/sqlx)) -- a second schema
   source of truth to keep synchronized inside the nix gate.

### Option 2: Diesel

The strongest conventional alternative: synchronous, SQLite-supported,
compile-time-checked DSL from a generated `schema.rs`, embedded migrations via
`diesel_migrations`
([Diesel: getting started](https://diesel.rs/guides/getting-started)). Dynamic
filter composition stays typed through boxed expressions, which would genuinely
fix `filters.rs`.

Rejected on cost/coverage:

- **FTS5 has no DSL.** `MATCH`, `rank`, and virtual-table joins drop to the
  `diesel::dsl::sql` escape hatch, which is explicitly unchecked: "The compiler
  will be unable to verify the correctness of the annotated type"
  ([Diesel: dsl::sql](https://docs.rs/diesel/latest/diesel/dsl/fn.sql.html)).
  The search overlay -- the largest dynamic query surface we have -- would be
  rewritten _and_ still unchecked.
- **The read model fights the DSL.** The fragment read model is a 7-way join
  plus a correlated `GROUP_CONCAT` subquery (`issues.rs:15-37`); expressible,
  but at a DSL complexity that fails the "would a senior engineer say this is
  overcomplicated?" test for a schema whose columns are all `TEXT` (P4 payoff
  ~zero).
- **Full rewrite of `lt-storage`** and a second schema source of truth
  (`schema.rs`) for a working system of ~40 statements.

### Option 3: SeaORM, rust-query

- **SeaORM** describes itself as "An async & dynamic ORM for Rust"
  ([SeaORM docs](https://www.sea-ql.org/SeaORM/docs/index/)); rejected with sqlx
  for the runtime reason.
- **rust-query** is synchronous and fully typed, but owns schema and migrations,
  warns "Do not use `rust-query` migrations if you plan to keep those migrations
  around for a long time", self-describes as "still in relatively early stages",
  and offers no raw-SQL escape
  ([rust-query docs](https://docs.rs/rust-query/latest/rust_query/)) -- FTS5
  becomes unrepresentable. Rejected.

### Option 4 (recommended): rusqlite + schema-adherence gate

Keep `rusqlite`. Make the gate compile every statement the code can ever issue
against the real, fully-migrated schema, using SQLite itself as the checker.

```text
        migration list (single schema source)
                      |
        +-------------+----------------------+
        v                                    v
  open_db() at runtime                sql_validation test (gate)
  (rusqlite_migration)                in-memory DB + same migrations
        |                                    |
        v                                    v
   real database                 for each registry entry:
                                   prepare(sql)            -> P1
                                   column_names() == decl  -> P3
                                   parameter_count() == n  -> P2 (const side)
                                        ^
                                        |
              db/sql.rs statement registry
              (Sql newtype: sole source of statement text)
              - fixed statements: named consts
              - fragments: filter clauses, sort columns,
                ISSUE_COLUMNS / ISSUE_JOINS probe templates
                                        ^
                                        | referenced by
        issues.rs  outbox.rs  comments.rs  filters.rs  search_query.rs
```

#### Statement registry

Every fixed statement becomes a named `const` referenced by its call site and
listed in a registry slice. The dynamic builders already draw from closed sets:
`build_sql_filter` has ~10 clause templates, `build_conditions` has ~8, and
`sort_column` is a total match over an enum (`filters.rs:102-112`). Each
fragment registers a probe template, e.g.
`SELECT 1 FROM issues i {ISSUE_JOINS} WHERE {clause}`.

Composition validity follows from fragment validity: both builders only conjoin
clauses with `AND` inside a fixed `SELECT` template, and `ORDER BY` columns come
from the probed enum. There is no free-form splicing of user input into SQL text
(values are always bound parameters).

#### Enforcement: the `Sql` newtype

Statement text is a type, not a `&str`, per [[posture.md]] ("use the type system
to encode invariants"):

```rust
// db/sql.rs -- the only module that can construct these.
pub(crate) struct Sql(&'static str);
pub(crate) struct ComposedSql(String);
```

- Fixed statements are `Sql` consts declared through a small `macro_rules!` that
  emits both the const and its registry entry, so declaration and registration
  are the same line -- an unregistered statement cannot exist.
- The `db` module's execution helpers (generalizing the existing `execute()`
  wrapper, `mod.rs:34`) accept `Sql` / `ComposedSql`, never `&str`.
- The two dynamic builders produce `ComposedSql` via constructors in `db/sql.rs`
  that accept only registered fragment consts and the sort-column enum. Composed
  text is therefore AND-joins of validated pieces inside registered templates,
  by construction.
- Tests are exempt by design: they keep raw `Connection` access to assert
  directly on rows (per [[testing.md]]), so no workspace-wide lint ban on
  `rusqlite` methods is added.

#### Validator

A `#[cfg(test)]` module in `lt-storage`: open an in-memory database, run the
migrations, `prepare()` every registry entry, and assert each entry's declared
column names and parameter count. Failures name the statement and carry SQLite's
own error ("no such column: ..."). This runs in both `make test` configurations
and under the coverage gate; no new dependencies.

Gate-time rather than `build.rs`, deliberately: the merge gate already runs the
tests, so the assurance is identical, while a `build.rs` checker would add
`rusqlite` (and the bundled SQLite C) as a build-dependency -- compiling SQLite
twice per cold build for no additional guarantee. The registry is data; if a
true compile-time failure ever becomes worth that cost, the same probe loop
moves into `build.rs` mechanically.

#### Row mapping (P3)

Alias every computed column in `ISSUE_COLUMNS` (`AS assignee_name`, ...) and
switch `issue_from_row` / `query_comments` from numeric indices to named access
(`row.get("assignee_name")`). The magic-index/column-order coupling disappears;
the validator's `column_names()` assertion pins the contract. The per-row name
lookup cost is irrelevant at this scale (result sets are capped at 250 rows,
`issues.rs:316`).

#### Migrations

Adopt
[`rusqlite_migration`](https://docs.rs/rusqlite_migration/latest/rusqlite_migration/):
versioned migrations tracked in SQLite's `user_version` pragma, defined as a
slice of `M::up(...)` entries, with a built-in `MIGRATIONS.validate()` test
hook.

- **Migration 1 is the clean final schema** -- one plain `M::up` batch, no
  probes, no compatibility hook. Pre-v1, databases created under the old probing
  scheme are a manual hard cutover: delete the file and re-sync (review decision
  on the implementation PR). Un-synced `outbox` / `pending_overlay` rows die
  with the file; accepted pre-v1.
- **Opening an incompatible database fails with an actionable error**: a
  connection at `user_version = 0` that already contains tables predates
  versioned migrations, and `open_db` reports the path and tells the user to
  delete it and re-run `lt sync`, instead of surfacing a bare "table already
  exists".
- The migration list becomes the single schema source shared by runtime
  `open_db()` and the validator, preserving the current property that tests
  exercise the exact production schema. Migration runs only inside `open_db`;
  tests reach the migrated schema through `Database::memory()`.

#### Property coverage

| #   | Coverage after this ADR                                                                                                                                       |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| P1  | Gate: every fixed statement and every dynamic fragment prepared against the real schema                                                                       |
| P2  | Gate for the registry side (`parameter_count`); call-site arity stays runtime, fail-fast (`InvalidParameterCount`), covered by existing per-module unit tests |
| P3  | Gate: `column_names()` assertion + named row access                                                                                                           |
| P4  | Not pursued: all columns are `TEXT`; declared-type spot checks via the `column_decltype` feature remain possible later if the schema grows real types         |

## Decisions

1. **Stay on `rusqlite`.** Reject sqlx and SeaORM (async-only, and checking that
   misses the dynamic SQL), Diesel (full rewrite whose checking still excludes
   the FTS5 search path), and rust-query (early-stage, no FTS5).
2. **Add a registered-statement schema-adherence check to the test gate**:
   statement/fragment registry in `lt-storage`, validated by preparing against
   the migrated in-memory schema (P1, P2-const, P3). Gate-time, not `build.rs`.
3. **Enforce the registry with the `Sql` / `ComposedSql` newtypes**: statement
   text is constructible only inside `db/sql.rs`; execution helpers take the
   newtypes, never `&str`.
4. **Replace index-based row mapping with aliased, named column access.**
5. **Adopt `rusqlite_migration`** with the clean final schema as migration 1;
   delete the `pragma_table_info` probe helpers. No pre-v1 compatibility bridge:
   an old-scheme database is deleted manually, and opening one fails with an
   error saying so.
6. **Move the `lt-cli` `COUNT(*)` diagnostics** (`lt-cli/src/search.rs:33-42`)
   behind `lt-storage` functions, bringing them into the registry.

## Residual gaps

- Call-site bind arity and value types remain runtime-checked (fail-fast).
- User-supplied FTS5 `MATCH` syntax errors are inherently runtime; already
  handled as query errors (`search_query.rs:465-474`).
- The newtype enforcement is API-shape, not hermetic: `Connection` stays exposed
  (tests assert on raw rows), so non-test code _could_ still call `rusqlite`
  methods directly with a string literal. The newtype makes that a visible
  anomaly in review rather than an impossibility.
