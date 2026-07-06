# The Schema Codegen Program (ENG-16)

## Status

Proposed — a program-decomposition ADR. ENG-16 ("generate as much as possible
from the graphql spec") is broad enough that a single design would either
overreach or stay vague. This ADR fixes the source of truth and the scope
frontier, then decomposes the work into independently-designable Tasks, each a
future sub-issue with its own detailed design. It does not itself design any
single generator to completion.

The keystone: **codegen is type-directed.** It follows from the hand-written
cynic fragments, not from the raw schema. This extends, rather than reopens,
[[linear-api-types-codegen.md]] (ENG-31), which established the cynic
selection-shaped type layer and rejected whole-schema type generation. It also
depends on [[unified-execute-adr.md]] (ENG-67) and builds on
[[operation-seam-adr.md]] (ENG-28), which flagged every hand-written seam
artifact as an ENG-16 codegen target (`operation-seam-adr.md:52-54`).

## Context

ENG-16 reads as a wishlist: no hand-rolled GraphQL boilerplate, CLI args from
variables, the search parser subsumed, resolvable ID fields, autocomplete, all
SQL generated, `build/search_filter_fields.toml` dead. Read naively — "generate
Rust from the SDL" — several items contradict decisions already made with cause.
Read correctly, they are one idea:

> The hand-written cynic fragments are the curated source of truth. Everything
> mechanically downstream of them is generated from _those types_.

### The fragments are the allowlist

We keep authoring cynic `QueryFragment`/`QueryVariables`/`InputObject` types by
hand. That is not the thing being generated — it is the curation. Its earlier
form was a TOML subset; its correct form is the fragment set itself, which can
select **any field of any type in the whole upstream schema** and stays fully
type-safe because cynic checks each fragment against the registered schema
(`crates/lt-types/src/lib.rs:24-25`, 182 derives across 15 files). The selection
set _is_ the intent
([[linear-api-types-codegen.md#The core constraint: usage is selection-shaped, not type-shaped]]:
`Comment` has ~50 fields, the code selects 3). Whole-schema generation stays
rejected; the fragment is what we author, and codegen mechanizes the rest.

```text
   raw SDL (524 types)  ──X── never the codegen input (ENG-31)
        │ cynic derive, hand-authored selection
        ▼
   FRAGMENTS  (the curated allowlist, type-safe, whole-schema reach)   ← SOURCE
        │ codegen follows from THESE types
        ├─ GraphqlOperation impl        (operation() + NAME)
        ├─ Query / Mutation             (SQL projection over the selected fields)
        ├─ Operation impl               (ENG-67's execute dispatch)
        └─ search grammar               (from IssueFilter's fields)
```

### `search_filter_fields.toml` dies outright

Not "de-duplicated" — dead. `IssueFilter`
(`crates/lt-types/src/issues.rs:41-66`) is already the curated allowlist as a
Rust type: it is a field of `IssuesVariables` (`issues.rs:477-482`), already
lowered to wire JSON by serde through `execute::<Op>`
(`crates/lt-upstream/src/client.rs:74-84`), and already lowered to SQL by
`lt-storage`. The TOML re-declares, in a second place, the field set
`IssueFilter` already states. The search grammar (`StemKey`/`StemKind`/the
parser) derives from `IssueFilter`'s fields; the SQL filter lowering from
per-field metadata on the same type. The TOML has nothing left to say.

### The generation frontier (primary-source inventory)

Sorted by whether an artifact is a function of the source types (generate) or of
local-cache behavior (hand-write):

**Derived from the fragment types — generate:**

- The `GraphqlOperation` impl. With `extract` removed
  ([[unified-execute-adr.md]] Decision 5), the impl reduces to
  `operation() = Self::build` plus `NAME` — identical across all operations,
  pure boilerplate.
- `Query`/`Mutation` (the two seam traits, [[unified-execute-adr.md]] Decision
  2): the SELECT projects the fragment's selected columns; the write applies the
  fragment's node types into their tables. The 6 reference-entity upserts
  already share `entity_upsert_sql!`
  (`crates/lt-storage/src/db/sql.rs:138-147`); the row-mappers are
  macro-factored (`upsert_entities_fn!`, `teams.rs:35-63`). ~70% of the ~43
  registered statements are id-keyed CRUD of this shape.
- The `Operation` impl and the outbox replay registry
  ([[unified-execute-adr.md]] Decisions 2, 4).
- The search grammar, from `IssueFilter`'s fields.

**A function of local-cache policy — hand-write, permanently:**

- The overlay-merge read model: `COALESCE`/`CASE` effective-field expressions
  (`sql.rs:53-133`), FTS join + `LIKE` fallback (`sql.rs:212-229`).
- The outbox machinery: optimistic writes, temp-id rewrite, command coalescing,
  replace-set deletes (`crates/lt-storage/src/db/outbox.rs`).
- Composed operations' bespoke wire→domain logic (the `From`/`TryFrom` impls
  that replace `extract`, [[unified-execute-adr.md]] Decision 5):
  `IssueDetailQuery`'s cursor derivation (`detail.rs:69-81`), `NewIssueQuery`'s
  `@include` directive and cache-sourced viewer (`new_issue.rs:44-93`),
  `NotificationsQuery`'s InlineFragments enum.
- The filter→SQL comparator _policy_ (name-or-id, `LIKE` vs exact,
  `filters.rs:16-98`): partly capturable as per-field metadata (Task 5), partly
  irreducible.

**Named by ENG-16 but not yet existing — net-new features, not "generation":**

- Name→id resolution. Only `resolve_me` exists, and it is name→*name*
  (`crates/lt-storage/src/search_query.rs:175-183`). No `--project=<name>` →
  `projectId`; no id-based filter on the wire or in SQL (all name-substring,
  `issues.rs:395-415`, `sql.rs:492`).
- Autocomplete. `Token::PartialStem.known_key` is populated but unconsumed
  (`search_query.rs:82-88`).

### The existing pipeline this extends

There are already two schema-driven codegen mechanisms, both reading
`build/linear-schema-definition.graphql`:

```text
  A) build.rs → OUT_DIR → include!  (bespoke source gen via lt-schema-codegen +
     quote/syn/prettyplease): SortField, build_sort, the Chumsky search parser.
     The two build.rs (lt-types, lt-storage) DUPLICATE this pipeline.
  B) cynic derive macros: the fragments, compile-checked against the registered
     schema. No file emission.
```

Sources: `crates/lt-schema-codegen/src/lib.rs`, `crates/lt-types/build.rs`,
`crates/lt-storage/build.rs`. ENG-16 shifts weight from A (schema-directed
source gen) toward B (type-directed derive), because the source is now the
fragment, not the SDL.

## Decision 1: type-directed derive is the primary modality

The generators read the hand-written Rust types (via companion derive macros
alongside cynic's), not the SDL. `#[derive(GraphqlOperation)]`,
`#[derive(Query)]`, `#[derive(Mutation)]` co-locate the generated impl with the
fragment it derives from, and avoid the `OUT_DIR`/`include!`/brace-escaping
fragility the search-codegen work documented
([[search-codegen-and-filter-expansion-adr.md]]).

The SDL-directed build.rs modality (A) survives only where the input genuinely
_is_ the schema — chiefly the registration `cynic-codegen::register_schema`
(`lt-types/build.rs:180`). The two duplicated build.rs collapse into
`lt-schema-codegen` (Task 4).

Rejected alternatives:

| Option                                                     | Why rejected                                                                                    |
| ---------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| Schema-directed generation (emit types from the SDL)       | the ENG-31 whole-schema rejection; also cannot express which selection an operation wants       |
| Generate operation _documents_ (`.graphql`) then run cynic | inverts the selection-shaped design; the fragment is the authoring surface, not an intermediate |

## Decision 2: the fragment set is the allowlist; the TOML dies

`IssueFilter` is the single curation of filterable fields. The search grammar
and the SQL filter lowering derive from it; `build/search_filter_fields.toml` is
deleted. Per-field metadata the derived generators need — the SQL column, the
comparator, whether the field resolves a name to an id (Task 6) — attaches to
`IssueFilter`'s fields as attributes, in one place, replacing the TOML's
parallel declaration.

Rejected alternatives:

| Option                                                  | Why rejected                                                                      |
| ------------------------------------------------------- | --------------------------------------------------------------------------------- |
| Keep the TOML as the filter allowlist                   | it duplicates `IssueFilter`'s fields; the type already is the curation            |
| Expose every `IssueFilter` schema field (drop curation) | the whole-schema exposure [[search-codegen-and-filter-expansion-adr.md]] rejected |

## The program: decomposed Tasks

Ordered derive-the-shell → structural → net-new. Each is a future sub-issue.

```text
  T1 GraphqlOperation derive ─┐
  T2 Query / Mutation derive ─┼─ derive the shell from the fragment types
  T3 Operation + replay registry ─┘  (needs ENG-67)
  T4 Pipeline unification: fold the two build.rs into lt-schema-codegen
  T5 IssueFilter-directed search grammar + SQL lowering; delete the TOML
  T6 Resolvable ID fields (net-new): id comparators + name→id resolvers
  T7 Autocomplete (net-new): consume known_key + T6 resolvers
  T8 CLI args from QueryVariables (forward-looking; CLI surface reduced to auth+sync)
```

- **T1 — `#[derive(GraphqlOperation)]`** from the fragment:
  `operation() = Self::build` + `NAME`, uniform now that `extract` is removed
  ([[unified-execute-adr.md]] Decision 5). The wire→domain transforms `extract`
  used to hold (`ViewerEnvelope -> Viewer`, the composed `IssueDetailData`)
  become `From`/`TryFrom` impls; the mutation success-gate moves to the write
  seam.
- **T2 — `#[derive(Query)]`/`#[derive(Mutation)]`** for id-keyed reference
  entities (team, user, project, cycle, label): the SELECT and the local write
  from the fragment's selected node types. Excludes issues/comments (overlay,
  replace-set, FTS).
- **T3 — the `Operation` impl + the replay registry**
  ([[unified-execute-adr.md]] Decisions 2, 4). Depends on ENG-67 landing them by
  hand.
- **T4 — pipeline unification**: one schema-parse + `quote` pipeline in
  `lt-schema-codegen`; the duplication across the two build.rs
  ([[linear-api-types-codegen.md]] deferred this) dies. Enables T5–T8.
- **T5 — `IssueFilter`-directed grammar + SQL lowering**: the search grammar and
  the mechanical part of `filters.rs` derive from `IssueFilter` + per-field
  metadata; the TOML is deleted. FTS and overlay-effective expressions stay
  hand-written.
- **T6 — resolvable ID fields** (net-new): id comparators on the wire and in
  SQL, standalone name→id cache lookups (projects/cycles/labels are upserted
  only as a side effect today, never queried standalone — `sql.rs:154-160`), and
  a name→id resolver generalizing `resolve_me`. The resolver _dispatch_
  generates from field metadata; the lookups are new code.
- **T7 — autocomplete** (net-new): consume `Token::PartialStem.known_key`
  (`search_query.rs:82-88`) for key and value completion, including T6
  resolvables.
- **T8 — CLI args from `QueryVariables`** ([[operation-seam-adr.md]] non-goal,
  `operation-seam-adr.md:434-435`). Forward-looking: the CLI issue commands were
  removed (`crates/lt-cli/src/main.rs` is auth + sync); this Task needs a
  command surface to exist, or ships as a dormant derive. Its sub-ADR resolves
  that first.

## Non-goals (permanent — bounded by prior decisions)

- **Generating the fragments/selection sets.** They are the source of truth. The
  whole-schema type generation [[linear-api-types-codegen.md]] rejected stays
  rejected.
- **Generating policy-shaped SQL.** Overlay merge, FTS, temp-id rewrite,
  composed-op cursor pagination, and the irreducible comparator policy are a
  function of local-cache behavior, not of the types.
- **`NotificationsQuery` / the inbox**, consistent with
  [[operation-seam-adr.md]]'s Notifications non-goal, until a cache table
  exists.

## Relationship to ENG-67 and ENG-63

- **ENG-67** ([[unified-execute-adr.md]]) hand-writes the `Operation` dispatch
  and the replay registry; T3 generates them. ENG-67 stabilizes the seam this
  program generates against, as ENG-28 did for the read seam.
- **ENG-63** (generic `Table<'a, T>`/`Form<'a, T>`) consumes generated operation
  outputs; orthogonal but complementary.

## Test migration

- Every generator Task keeps the tests of the artifacts it replaces green
  **unchanged** — generated code must pass the hand-written code's behavioral
  tests (`crates/lt-runtime/src/ops.rs`, the drain tests, the filter/grammar
  tests) or the generation is wrong.
- The compile-time schema check that cynic already performs on every fragment is
  the type-directed pipeline's front-line gate; the build-time allowlist
  validation ([[architecture.md#Search and the codegen seam]]) moves onto the
  per-field metadata.
- New generators get golden-file tests of their `prettyplease`-formatted output,
  the pattern [[search-codegen-and-filter-expansion-adr.md]] established.

## Open questions

1. **Derive reflection limits** (T1–T3, T5): can a companion derive see enough
   of a fragment's selected fields to emit the SELECT and the local write, or is
   a small per-operation attribute needed alongside cynic's? Owned by each
   Task's sub-ADR.
2. **T5 boundary**: how much of the filter→SQL comparator policy is
   metadata-derivable vs irreducibly bespoke.
3. **T8 dependency**: whether a CLI command surface is reintroduced (making T8
   live) or T8 ships as a dormant derive.
