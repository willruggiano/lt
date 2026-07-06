# The Schema Codegen Program (ENG-16)

## Status

Proposed — a program-decomposition ADR. ENG-16 ("generate as much as possible
from the graphql spec") is broad enough that a single design would either
overreach or stay vague. This ADR fixes the _scope frontier_ — what is
mechanically generatable versus what is irreducibly hand-written — and
decomposes the work into independently-designable Tasks, each a future sub-issue
with its own detailed design. It does not itself design any single generator to
completion.

It builds on two delivered ADRs whose decisions bound this one:

- [[linear-api-types-codegen.md]] (ENG-31): established the cynic
  selection-shaped type layer and **rejected whole-schema type generation**.
- [[operation-seam-adr.md]] (ENG-28): made the operation type the sole
  vocabulary of both sides of the cache, and flagged every hand-written seam
  artifact as an ENG-16 codegen target (`operation-seam-adr.md:52-54`).
- [[mutation-seam-adr.md]] (ENG-67): makes the write path generic by hand,
  producing the mutation registry this program later generates.

## Context

ENG-16 reads as a wishlist: no hand-rolled GraphQL types, CLI args generated
from variables, the search parser subsumed, resolvable ID fields, autocomplete,
all SQL generated from the schema, `build/search_filter_fields.toml` dead. Taken
literally and together, several of these contradict decisions already made with
cause. The design work is therefore first a _scoping_ problem: separate the
genuinely-mechanical boilerplate (generate it) from the selection-shaped and
policy-shaped code (never generate it) from the net-new features that ENG-16
smuggles in under the word "generate."

### The codegen infrastructure already exists

There are two schema-driven codegen mechanisms today, both reading the committed
snapshot `build/linear-schema-definition.graphql` (37,149 lines):

```text
  A) Bespoke source generation (build.rs → OUT_DIR → include!)
     ┌ build/search_filter_fields.toml (8 filter + 7 sort fields, curated)
     ├ build/linear-schema-definition.graphql (the SDL snapshot)
     │        │ parsed + validated by lt-schema-codegen
     │        ▼
     ├ crates/lt-types/build.rs   → SortField enum, build_sort()   → src/query.rs include!
     └ crates/lt-storage/build.rs → StemKey/StemKind, the Chumsky   → src/search_query.rs include!
                                     parser (one arm per field)
       (both scripts duplicate the schema-parse + quote/syn/prettyplease pipeline)

  B) cynic derive-macro codegen (no file emission)
     #[cynic::schema("linear")] registered at build time (lt-types/build.rs:180)
     #[derive(cynic::QueryFragment/InputObject/Enum/Scalar)] — 182 derives across
     15 files in lt-types, each compile-checked against the registered schema.
```

Sources: `crates/lt-schema-codegen/src/lib.rs`, `crates/lt-types/build.rs`,
`crates/lt-storage/build.rs`, `crates/lt-types/src/lib.rs:24-25`. So ENG-16 is
**extending an existing pipeline**, not building one.

### The scope frontier (primary-source inventory)

An inventory of the seam (14 `GraphqlOperation` impls, ~43 registered SQL
statements, the filter lowering) sorts cleanly into three bins.

**Generatable — mechanical boilerplate around a hand-written core:**

- The `GraphqlOperation` impl bodies. `operation()` is _always_ `Self::build`;
  `extract()` is a one-liner projection for the 9 simple queries —
  `Ok(self.teams.nodes)` (`crates/lt-types/src/teams.rs:26`), `Ok(self.issues)`
  (`crates/lt-types/src/issues.rs:507`), `Ok(self.workflow_states)`
  (`states.rs:110`). Identical across all 14 impls modulo the projection path.
- `QueryVariables` structs and connection-wrapper fragments (`TeamConnection`,
  `UserConnection`, `IssueConnection` — all `{ nodes, page_info }`).
- ~70% of the ~43 SQL statements: id-keyed upsert/select/delete/count/meta. The
  6 reference-entity upserts already share one template via
  `entity_upsert_sql!("table")` (`crates/lt-storage/src/db/sql.rs:138-147`), and
  the row-mappers are macro-factored (`upsert_entities_fn!`, `teams.rs:35-63`).
- Most `Read`/`Upsert` impls: one-liners delegating to a query fn and returning
  a fixed `EntityKey` vec (`Read for TeamsQuery`, `teams.rs:177-185`).
- The mutation replay registry ([[mutation-seam-adr.md]] Decision 2).

**Irreducibly hand-written — selection-shaped or policy-shaped:**

- The **selection set itself.**
  [[linear-api-types-codegen.md#The core constraint: usage is selection-shaped, not type-shaped]]
  proved whole-schema generation wrong: `Comment` has ~50 fields, the code
  uses 3. The cynic fragment _is_ the per-operation intent; it stays
  hand-authored.
- The `IssueFilter`→wire lowering: ~10 comparator `InputObject`s and a
  hand-written `to_wire`/`Serialize` encoding real policy — AND-joining set
  fields, assignee name-or-email `or`, date gte/lt splitting
  (`issues.rs:25-451`).
- The overlay-merge read model: `COALESCE`/`CASE` effective-field expressions
  (`sql.rs:53-133`), FTS join + `LIKE` fallback (`sql.rs:212-229`), the dual SQL
  filter lowering (`crates/lt-storage/src/db/filters.rs:16-98`, 15 `Frag`s).
- The outbox machinery: optimistic writes, temp-id rewrite, command coalescing,
  replace-set deletes (`crates/lt-storage/src/db/outbox.rs`).
- Composed operations' bespoke `extract`: `IssueDetailQuery`'s cursor derivation
  (`detail.rs:69-81`), `NewIssueQuery`'s `@include` directive and cache-sourced
  viewer (`new_issue.rs:44-93`), `NotificationsQuery`'s InlineFragments enum
  (`notifications.rs`).

**Net-new features ENG-16 names but that do not exist yet:**

- Name→id resolution of any kind. Only `resolve_me` exists, and it is
  name→*name* (`crates/lt-storage/src/search_query.rs:175-183`). No
  `--project=<name>` → `projectId` path; no id-based filter on the wire or in
  SQL (every entity filter is name-substring only, `issues.rs:395-415`,
  `sql.rs:492`).
- Autocomplete. `Token::PartialStem` already carries
  `known_key: Option<StemKey>` (`search_query.rs:82-88`) — the natural hook —
  but nothing consumes it for value completion.

### The thesis

```text
   ┌─────────────────────────────────────────────────────────────────┐
   │  hand-written CORE, per operation:                                │
   │    · the cynic selection set (the fragment)                       │
   │    · the bespoke extract / filter lowering / cache-policy SQL      │
   └─────────────────────────────────────────────────────────────────┘
                    ▲ generate the mechanical shell AROUND it, never the core
   ┌─────────────────────────────────────────────────────────────────┐
   │  generated SHELL:  GraphqlOperation impl · Variables · connection │
   │  wrappers · id-keyed CRUD SQL · Read/Upsert glue · replay registry │
   └─────────────────────────────────────────────────────────────────┘
```

"Generate as much as possible" resolves to: **generate the shell, hand-write the
core, and treat the resolvable-ID/autocomplete asks as the features they are.**
This is the maximal honest reading — it does not resurrect the whole-schema
generation ENG-31 rejected, and it does not pretend the policy-shaped SQL is
mechanical.

## Decision 1: two codegen modalities, chosen per artifact

The program uses both existing mechanisms; each Task picks the fit:

- **Derive macros** (modality B) for artifacts keyed off a _Rust type_ — the
  `GraphqlOperation` impl, the reference-entity CRUD. cynic already derives on
  these types; a companion `#[derive(GraphqlOperation)]` with a
  `#[graphql_operation(name = "teams", extract = teams.nodes)]` attribute
  co-locates the spec with the type and avoids the `OUT_DIR`/`include!`
  indirection the search-codegen work found fragile
  ([[search-codegen-and-filter-expansion-adr.md]], "the code generation
  technique is fragile").
- **build.rs source generation** (modality A) for artifacts keyed off the _SDL +
  curated allowlist_ — the search grammar, sort vocabulary, and any per-field
  resolution metadata. The input there is the schema, not a Rust type.

Rejected alternatives:

| Option                                                                       | Why rejected                                                                                                                                                                                             |
| ---------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| One modality for everything                                                  | derive can't see the SDL allowlist; build.rs can't cleanly read a downstream Rust type's shape. Each artifact has a natural input; force-fitting one tool reintroduces the escaping/`include!` fragility |
| Generate operation _documents_ (`.graphql`) and run cynic's query-generation | inverts the selection-shaped design ENG-31 chose; the fragment is the ergonomic authoring surface, not a generated intermediate                                                                          |

## Decision 2: the allowlist survives; only its duplication dies

ENG-16 says "`build/search_filter_fields.toml` dies." It does not — it is the
_human curation_ of which of ~50 `IssueFilter` fields are exposed (8 today), and
that curation exists precisely because exposing all of them was rejected
([[search-codegen-and-filter-expansion-adr.md]]). What dies is the
**duplication**: the schema-parse + `quote` pipeline is copy-pasted across
`lt-types/build.rs` and `lt-storage/build.rs` ([[linear-api-types-codegen.md]]
left this unification as future work). The curation consolidates into one place
(`lt-schema-codegen`), and per-field metadata (e.g. "this field resolves a name
to an id") attaches there. "The TOML dies" is reframed as "the TOML stops being
duplicated and grows the metadata the resolvable-ID feature needs."

## The program: decomposed Tasks

Each Task is independently designable and shippable, and becomes its own
sub-issue with a detailed ADR. Ordered mechanical → structural → net-new.

```text
  T1 Operation scaffolding ─┐
  T2 Reference-entity CRUD ─┼─ generate the shell (derive macros)
  T3 Mutation registry ─────┘   (T3 needs ENG-67)
  T4 Pipeline unification ─── consolidate build.rs + kill duplication (enables T5–T7)
  T5 CLI args from variables ─ needs a CLI command surface (currently auth+sync only)
  T6 Resolvable ID fields ─── net-new: id filters + name→id resolvers + metadata
  T7 Autocomplete ─────────── net-new: consume known_key (needs T6 for id values)
```

### Task 1 — Operation scaffolding codegen

Generate the `GraphqlOperation` impl for the mechanical operations from a derive
macro. Attribute carries `NAME` and the `extract` projection path; `operation()`
is always `Self::build`. Also generate the trivial `QueryVariables` and
connection wrappers where they are pure `{ nodes, page_info }`.

- Generates: ~9 of 14 `GraphqlOperation` impls, connection wrappers.
- Excludes (opt-out, stay hand-written): `IssueDetailQuery`, `NewIssueQuery`,
  `ViewerQuery`, the three mutations, `NotificationsQuery` — every op whose
  `extract` is more than a projection (success-gating, cursor derivation, domain
  recomposition). Cited: `issues.rs:574-579`, `detail.rs:69-81`,
  `viewer.rs:15-33`, `notifications.rs`.
- Highest value, lowest risk. Independent of everything else.

### Task 2 — Reference-entity CRUD + Read/Upsert codegen

For the id-keyed reference entities (team, user, project, cycle, label) already
sharing `entity_upsert_sql!`, derive the upsert/select-by-id/delete SQL and the
`Read`/`Upsert` impl from the typed struct's fields instead of hand-listing
columns.

- Generates: the ~70%-mechanical CRUD for reference entities, their `Upsert`
  impls and fixed `EntityKey` returns.
- Excludes: `issues` (overlay merge), `comments` (replace-set, `local:%`
  preservation), FTS, the dynamic `select_issues` composer — all policy-shaped
  (`sql.rs:85-133,212-229,558-588`, `outbox.rs`).
- Design question deferred to its sub-ADR: derive-on-entity-type vs. an entity
  manifest. The current macros (`entity_upsert_sql!`, `upsert_entities_fn!`) are
  the halfway point to extend.

### Task 3 — Mutation replay registry codegen

Generate the `REPLAY_REGISTRY` slice ([[mutation-seam-adr.md]] Decision 2) from
the set of `Mutate` impls, replacing the hand-maintained table.

- Depends on: ENG-67 landing the hand-written registry first.
- Small; the last `NAME → replay` dispatch becomes generated data.

### Task 4 — Codegen pipeline unification

Collapse the duplicated schema-parse + `quote` pipeline from the two `build.rs`
into `lt-schema-codegen`, and move the filter/sort curation into one place with
room for per-field metadata.

- Enables T5–T7 (they need one canonical field registry, not two).
- Pure refactor of the generation machinery; no user-visible change. Cited
  duplication: subagent-confirmed across `lt-types/build.rs` and
  `lt-storage/build.rs`; deferral recorded in [[linear-api-types-codegen.md]].

### Task 5 — CLI arguments from `QueryVariables`

Derive clap arguments from a variables struct (`IssuesVariables` →
`--filter/--sort/--first/--after`), the non-goal [[operation-seam-adr.md]] named
("`IssueArgs → IssuesQuery` stays a hand-written `From` until ENG-16 derives
clap from variables", `operation-seam-adr.md:434-435`).

- **Blocked / forward-looking:** the CLI issue commands were removed; the
  surface is now auth + sync only (`crates/lt-cli/src/main.rs`). This Task needs
  a CLI command surface to exist, or is designed as the derive that would apply
  when one returns. Its sub-ADR must resolve that dependency first.

### Task 6 — Resolvable ID fields

Net-new feature: `--project=<name>` → `projectId`. Requires three pieces that do
not exist: (a) id-based filter fields on the wire and in SQL (today all
name-substring), (b) standalone name→id cache lookups for projects/cycles/labels
(upserted only as a side effect of issue upserts today, never queried standalone
— `sql.rs:154-160`), (c) a resolver at lowering time, the name→id generalization
of `resolve_me`. Per-field "resolvable entity" metadata lives in the unified
allowlist (T4).

- Largest net-new surface; bespoke-heavy. Not "generation" so much as a feature
  whose _resolver dispatch_ can be generated from field metadata.

### Task 7 — Autocomplete

Net-new: consume the already-present `Token::PartialStem.known_key`
(`search_query.rs:82-88`) to offer key and value completions in the search bar
and pickers, including resolvable-id values from caches.

- Depends on: T6 for id-value completion.
- The parser already surfaces the hook; nothing consumes it yet
  (`search_query.rs:262` skips it for display only).

## Non-goals (permanent — bounded by prior decisions)

- **Whole-schema type generation.** Rejected in
  [[linear-api-types-codegen.md#The core constraint: usage is selection-shaped, not type-shaped]];
  524 recursive mostly-unused types. The selection set stays hand-authored.
- **Generating policy-shaped SQL.** The overlay merge, FTS, filter lowering, and
  outbox/temp-id machinery encode local-cache policy with no schema counterpart.
  ENG-16's "all SQL is generated" applies to the id-keyed CRUD tier only; the
  policy tier is out of scope by construction.
- **Generating bespoke `extract`/composed operations.** `IssueDetailQuery`,
  `NewIssueQuery`, `NotificationsQuery`, and the mutations opt out of Task 1.
- **`NotificationsQuery` and the inbox.** Consistent with
  [[operation-seam-adr.md]]'s Notifications non-goal; no codegen until a
  notifications cache exists.

## Relationship to ENG-67 and ENG-63

- **ENG-67** ([[mutation-seam-adr.md]]) is a prerequisite for Task 3 and makes
  the write seam a stable codegen target, mirroring how ENG-28 (the read seam)
  preceded this program.
- **ENG-63** (generic `Table<'a, T>`/`Form<'a, T>`) is the TUI-side
  generalization that consumes generated operation outputs; orthogonal to this
  program but complementary — once operations are generated, the views over them
  can be generic.

## Test migration

Per Task; the program-level rule is that generation must be
_behavior-preserving_ and verified as such:

- Each generator Task keeps the existing tests of the artifacts it replaces
  (e.g. the `Read`/`Upsert` tests in `crates/lt-runtime/src/ops.rs`, the drain
  tests) green **unchanged** — generated code must pass the hand-written code's
  tests, or the generation is wrong.
- The build-time schema/allowlist validation
  ([[architecture.md#Search and the codegen seam]]) extends to any new metadata:
  a mismatch between a generated artifact and the schema fails the build, as the
  filter allowlist does today.
- New generators get golden-file tests of their emitted source
  (`prettyplease`-formatted), the pattern
  [[search-codegen-and-filter-expansion-adr.md]] established.

## Open questions

1. **Task 1 authoring surface** — derive-macro attribute vs. a small
   per-operation manifest. The ADR recommends the derive (Decision 1); the
   Task-1 sub-ADR confirms it against the composed-operation opt-out ergonomics.
2. **Task 5 dependency** — whether a CLI command surface is reintroduced (making
   Task 5 live) or Task 5 ships as a dormant derive. Owned by the Task-5
   sub-ADR.
3. **Task 2 boundary** — exactly which entities are "reference/mechanical."
   Team, user, project, cycle, label are clear; the sub-ADR fixes the line
   against anything that acquires overlay or replace-set policy.
