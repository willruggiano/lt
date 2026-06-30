# Generate Linear API Types From the GraphQL Schema (ADR)

## Status

Proposed — `Refs: ENG-31`

> **Revision (PR #26 review).** The first draft scoped this to "stop
> hand-writing response structs." The review pushed back on one line —
> *"per-operation modules re-emit small nested structs, so cross-operation
> sharing is lost"* — and asked us to do better, with three threads: a
> relational DB schema, offline-capable mutations, and **GraphQL fragments as
> the shared type at the `tui <-> db/api` boundary**, all under one rule:
> *Linear's GraphQL API is hit only by the sync thread; the TUI touches only the
> local database.* This revision answers that. The type-codegen decision is now
> one pillar of a local-first data architecture, and the recommendation changes
> accordingly (cynic over graphql_client — see [Pillar 1](#pillar-1)).

## Context

The Linear API response/variable types are hand-written serde structs scattered
across the crate, one ad-hoc set per GraphQL operation. ENG-31 asks to generate
them "from the graphql spec," replacing "the vast majority, if not all, of the
types defined in the `linear::types` crate."

Two pieces of infrastructure already exist and shape every option below:

- A committed schema snapshot: `build/linear-schema-definition.graphql`
  (37,149 lines; 524 `type`, 371 `input`, 95 `enum`, 8 `interface`, 8 `scalar`).
- A `build.rs` codegen seam that already parses that schema with
  `graphql-parser` and emits Rust via `quote`/`syn`/`prettyplease` into
  `OUT_DIR` (`build.rs:12`, `build.rs:63-81`, `build.rs:786-796`). It currently
  generates only the **search grammar** and validates the TOML allowlist against
  `IssueFilter`/`IssueSortInput` (`build.rs:656-689`). It generates no API
  response types.

### Current hand-written type surface

82 `struct`/`enum` deserialization types carry the API layer
(`grep -rE 'struct |enum ' $(grep -rl Deserialize src)`):

```
src/linear/types.rs          22   list Issue + envelope + IssueDetail/IssueRef
src/linear/mutations.rs      19   IssueUpdate/IssueCreate/CommentCreate/Teams/WorkflowStates
src/issues/new.rs             9   Viewer + TeamMembers (CLI)
src/tui/new_issue.rs          7   TeamMembers (TUI, duplicate shape)
src/linear/notifications.rs   7   Notifications (inline fragment on IssueNotification)
src/sync/comments.rs          5   IssueComments page
src/linear/viewer.rs          4   Viewer (inline)
src/auth/status.rs            3   viewer status (inline)
```

~13 operations across 7 files, each redeclaring its own response shape. The
duplication is acute for trivial wrappers: `State`, `IssueState`,
`IssueDetailState`, `NotificationIssueState` are all `{ name }` or `{ id, name }`;
the `Team*` family likewise. Every field carries a manual
`#[serde(rename = "camelCase")]` (`types.rs:60`, `notifications.rs:58-63`).

### The transport seam (preserved by every option below)

All operations flow through one object-safe trait and one free helper:

```
operation query string ─┐
serde_json variables   ─┤
                        ▼
   GraphqlTransport::query(query, vars) -> Value   (client.rs:14-18)
                        │  unwraps { data, errors } envelope  (client.rs:59-71)
                        ▼
   query_as::<T: DeserializeOwned>(...) -> T   serde_json::from_value  (client.rs:75-82)
```

`query_as` is generic over any `DeserializeOwned`. **Whatever produces `T` is
orthogonal to the transport.** Verified for both candidate libraries: their
generated types are plain serde types (graphql_client emits
`Serialize`/`Deserialize`; cynic's `GraphQlResponse<ResponseData>` is bounded
`ResponseData: DeserializeOwned` and decoded with `serde_json`,
`cynic-3.13.2/src/http.rs:101,222`). Neither requires adopting its bundled HTTP
client; both build a query string + variables we feed to `HttpTransport`. **No
option touches `client.rs`.**

---

## The core constraint: usage is selection-shaped, not type-shaped

The naive reading of ENG-31 — "one Rust type per GraphQL type" — is wrong, and
the schema proves it. The `Comment` object type
(`build/linear-schema-definition.graphql:2829`) has dozens of fields, many of
them paginated sub-connections taking arguments:

```graphql
type Comment implements Node {
  agentSession: AgentSession
  agentSessions(after: String, first: Int, ...): AgentSessionConnection!
  aiPromptProgresses(filter: AiPromptProgressFilter, ...): AiPromptProgressConnection!
  ... (dozens more)
}
```

The code uses three: `body`, `createdAt`, `user { name }`
(`sync/comments.rs:18-30`). Whole-schema generation would emit 524 recursive,
mostly-`Option`, mostly-unused structs with no representation for
field-arguments — the opposite of `posture.md` §2 ("Minimum code… Nothing
speculative"). The types the code wants are **shaped to the selection set of
each operation**. That fact drives the whole design.

```
   GraphQL object type (Comment: ~50 fields)   ──X── do NOT mirror
   Operation selection set ({ body createdAt user{name} })  ──> model THIS
```

---

## Target architecture (per review)

One rule reorganizes the data flow: **the GraphQL API is reached only by the
sync layer; the TUI/CLI read and write only the local SQLite database.** Today
this is violated on the write path — the TUI spawns a worker that calls
`HttpTransport` directly and reverts SQLite on failure
(`src/tui/popup.rs:352-393`, `architecture.md:122-128`). The target:

```
            ┌──────────────── sync thread / `lt sync` ────────────────┐
            │                                                          │
   Linear GraphQL API ──(reads: fragment-typed responses)──> relational SQLite
            ▲                                                          │
            │                                                          ▼
            └──(writes: drain mutation outbox)────────────  TUI / CLI read model
                                                                       ▲
   TUI / CLI ──read──>  DB query layer ──(fragment types)─────────────┘
   TUI / CLI ──write──> mutation outbox table (local, no network)
```

**Fragment types are the shared currency.** A GraphQL fragment is a named,
reusable selection set. We make those fragments the single definition of:

1. what the sync layer **fetches** from Linear,
2. what the relational schema must **store** to satisfy a read,
3. what the DB query layer **returns** and the TUI **renders**.

This is exactly the reuse the review asked for. The three pillars below realize
it: the type layer (fragments + library choice), the storage layer (relational
schema), and the write path (offline outbox).

---

<a id="pillar-1"></a>
## Pillar 1 — Shared fragment types: graphql_client vs cynic

The review's specific objection — *cross-operation struct sharing is lost* — is
real and library-dependent. Primary evidence from the vendored sources:

**graphql_client (0.16.0)** is *query-first*: a `#[derive(GraphQLQuery)]` marker
struct points at a `.graphql` file + schema, and codegen emits a module of
response types shaped to the selection set. It generates only used types
(`codegen/enums.rs:30`), maps nullability/list to `Option`/`Vec`
(`type_qualifiers.rs:1-10`), auto-emits serde renames (`codegen/shared.rs:26-32`),
gives enums an `Other(String)` forward-compat fallback (`codegen/enums.rs:62-95`),
and supports inline fragments (`query/selection.rs:18,68`) — needed for the
Notifications `... on IssueNotification` (`notifications.rs:19-21`). **But
fragments are rendered per `BoundQuery`** — `generate_fragment_definitions`
iterates one operation's `all_used_types.fragment_ids()` (`codegen.rs:248-252`).
A fragment used by two separate derives is emitted twice, in two modules. The
only way to share is to put every operation in one document under one derive;
even then the types are codegen-named, deeply nested, and `Deserialize`-only —
awkward to *construct* from DB rows, which the read model requires.

**cynic (3.13.2)** is *struct-first* ("a bring your own types GraphQL client",
`cynic-3.13.2/README.md`). You hand-author a struct and
`#[derive(cynic::QueryFragment)]`; cynic checks it against the schema and
generates the GraphQL from it. The decisive property, from the crate docs
(`cynic-3.13.2/src/lib.rs:54-67`):

> This `Film` struct can now be used as the type of a field on **any other**
> `QueryFragment` struct…

That is first-class cross-operation sharing: define `IssueRow` once, reuse it as
a field in the list query, the delta query, and the detail query. Because the
struct is **ours**, it can carry methods and `From`/`Into` impls and be
**constructed by hand from a SQL join** — which is precisely what the
"DB returns fragment types" contract needs. Interfaces/unions are supported
(README features; the Notifications case). Responses are serde-decoded
(`http.rs:101`), so the existing transport seam holds; we do not enable cynic's
`http-*` features.

```
  graphql_client                          cynic
  ──────────────                          ─────
  .graphql query ──derive──> module       struct + derive ──> GraphQL string
  types: codegen-owned, per-op            types: hand-owned, shared across ops
  share: only within one document         share: any struct as any field
  construct from DB row: awkward          construct from DB row: plain struct
  author cost: write .graphql only        author cost: write structs (querygen helps)
```

### Recommendation: cynic

For the *original* narrow scope (kill hand-written response structs, types stay
inside the sync layer), graphql_client is the lower-effort win and was the first
draft's pick. For the *expanded* architecture — where fragment types are the
durable, shared, hand-constructed contract between DB and TUI — cynic is the
right tool, because it makes the fragment type a normal owned Rust struct usable
in all three roles (fetch / store-read / render). graphql_client's per-operation,
codegen-owned, Deserialize-only types cannot fill the DB-return role without a
second hand-written domain layer, which defeats the point.

Honest costs of cynic:
- **Structs are hand-authored** (cynic verifies, doesn't write them). `querygen`
  bootstraps from a query; net authoring is higher than graphql_client.
- **A `use_schema!` module** must be generated from the snapshot
  (`lib.rs:18-22`) — one-time wiring.
- **Custom scalars** (`DateTime`, `ID`) need `impl Scalar`/newtypes rather than
  bare `String`; today the code uses `String` timestamps everywhere
  (`types.rs:66-69`). This is a small, deliberate typing win, not free.
- **One struct plays three roles** (wire selection + storage read model + view
  model). For a local-first cache this coupling is *desirable* — the cache
  stores exactly what the UI needs, sourced from exactly that selection — but a
  change to the UI's needs deliberately ripples to the fetch selection and the
  storage contract. We accept that as the single-source-of-truth property.

graphql_client remains the fallback if hand-authoring is rejected, in which case
fragment types stay a sync-internal concern and the TUI↔DB contract is a
separate hand-written domain layer (status quo, minus the response-struct
churn).

### Not every "type" is a fragment type

Wire-sourced selections become fragment types. Locally-assembled types stay
hand-written: `IssueDetail`/`IssueRef` are built from the cache, not a query
(`tui/detail.rs:206-280`); `Viewer` (`viewer.rs:21-27`) is a projection;
`db::Issue` (`db/issues.rs:8-27`) is the row type. Under the new architecture
several of these *become* DB-returned fragment types (e.g. the issue read
model), but the principle stands: generation/codegen targets wire selections,
not local projections.

---

## Pillar 2 — Relational schema

Today there is one denormalized table. `issues` flattens every relation into
name strings — `state_name`, `assignee_name`, `team_name`, `team_key`,
`project_name`, `cycle_name`, `creator_name`, and `labels` as a comma-joined
blob (`db/mod.rs:56-68`, migrations `db/mod.rs:114-150`; labels join
`db/issues.rs:54-63`). Comments are the only related entity with its own table
(`db/mod.rs:93-105`). FTS5 mirrors `issues(identifier, title)` via triggers
(`db/mod.rs:73-92`).

Costs of the flat model: no identity for related entities (the `From<db::Issue>`
synthesizes empty ids, `db/issues.rs:38-46`); labels unsearchable/filterable
relationally; a renamed team/user must be rewritten across every issue row; no
foundation for storing the other entities the API already returns.

The schema is natively relational. `Issue` references
`team: Team!`, `state: WorkflowState!`, `assignee: User`, `creator: User`,
`project: Project`, `cycle: Cycle`, `parent: Issue`, and `labels` (a connection)
(`build/linear-schema-definition.graphql`, `type Issue`). Proposed tables, one
per entity, FKs + indexes:

```
teams(id PK, key, name)
users(id PK, name, email)
workflow_states(id PK, team_id FK, name, type, position)
projects(id PK, name)
cycles(id PK, number, name NULL)
labels(id PK, name)
issues(id PK,
       identifier, number, title, description, priority, priority_label,
       team_id  FK -> teams,
       state_id FK -> workflow_states,
       assignee_id FK -> users   NULL,
       creator_id  FK -> users   NULL,
       project_id  FK -> projects NULL,
       cycle_id    FK -> cycles   NULL,
       parent_id   FK -> issues   NULL,
       created_at, updated_at, synced_at)
issue_labels(issue_id FK, label_id FK, PRIMARY KEY(issue_id, label_id))
comments(id PK, issue_id FK -> issues, user_id FK -> users NULL, body, created_at, updated_at, synced_at)
indexes: every FK column; issues(updated_at); issues(team_id, state_id)
FTS5: issues_fts(identifier, title[, description]) external-content over issues (kept)
```

### How fragment types bind to the relational schema

A fragment's selection set is the read contract; the DB layer reconstructs the
fragment struct from a join. Example:

```
fragment IssueRow on Issue {            SELECT i.*, t.name, s.name, a.name, ...
  id identifier title priority           FROM issues i
  state { name }            ───────►     JOIN workflow_states s ON s.id = i.state_id
  assignee { name }                      JOIN teams t           ON t.id = i.team_id
  team { key name }                      LEFT JOIN users a      ON a.id = i.assignee_id
  labels { nodes { name } }              LEFT JOIN issue_labels … (aggregated)
}                                        → build IssueRow { … } in Rust
```

Nested fragments (`StateName on WorkflowState { name }`, `Actor on User { name }`)
map to joined columns and are the reuse units across the list/delta/detail
read models. This makes the relational schema's coverage a checkable property:
**every field any fragment selects must have a column or join** — a drift gate
analogous to the existing allowlist gate (`build.rs:684-689`).

The sync layer performs the inverse: a fetched `IssueRow` (and its nested
entities) is **upserted into the entity tables**, not flattened — so a team
rename touches one `teams` row, and entities the UI later needs are already
stored.

---

## Pillar 3 — Offline-capable mutations (outbox)

Current writes are optimistic but **online-required**: the TUI updates SQLite,
spawns a thread that calls the API, and reverts SQLite if the call fails
(`popup.rs:352-393`). Offline = guaranteed revert; and the TUI talks to the API,
violating the target rule.

Invert it with a durable **outbox**:

```
   TUI write ──► (1) apply optimistically to entity tables
             └─► (2) INSERT into mutation_outbox   (local, no network)     ── UI thread ends here

   sync thread / `lt sync` ──► drain outbox in order:
        pending ─► call mutations.rs (existing fns) ─► success: mark applied,
                                                       reconcile returned entity into tables
                                                ─► failure: backoff + retry / surface permanent error
```

```
mutation_outbox(
  seq         INTEGER PK AUTOINCREMENT,   -- total order
  op_type     TEXT,                       -- IssueUpdate | IssueCreate | CommentCreate
  entity_id   TEXT,                       -- target (or client temp id for creates)
  variables   TEXT,                       -- JSON: the typed Variables payload
  status      TEXT,                       -- pending | in_flight | failed
  attempts    INTEGER, last_error TEXT,
  created_at  TEXT)
```

Design points / correctness concerns to settle:

- **No UI-thread network.** Step (2) replaces the `spawn → HttpTransport` in
  `popup.rs`; the existing `mutations.rs` functions move behind the sync drainer.
  This is the change that actually enforces "TUI only hits the DB."
- **Ordering vs delta sync.** A delta pull must not clobber a row that has
  un-acked outbox entries. Options: (a) apply outbox *after* each delta merge so
  local intent re-wins; (b) skip overwriting rows with pending mutations;
  (c) reconcile by `updatedAt`. Server remains source of truth once a mutation
  acks. This is the central correctness question and must be specified before
  implementation.
- **Creates need temp ids.** `IssueCreate` has no server id until acked; the
  outbox carries a client temp id, and reconciliation rewrites FKs on ack.
- **Idempotency / retries.** Retries must not double-apply; key on `seq` and
  treat a confirmed server state as terminal.

This pillar is the largest behavioral change and can land **after** Pillars 1–2;
the type and storage layers do not depend on it.

---

## Options considered (type layer)

| Option | Verdict |
|---|---|
| A. Whole-schema type generation | Rejected — the core constraint (524 unused recursive types) |
| B. graphql_client (query-first) | Fallback — least effort, but per-op types can't be the DB↔TUI contract |
| C. **cynic (struct-first, shared fragments)** | **Recommended** for the expanded architecture |
| D. Extend `build.rs` to a custom operation→type mapper | Rejected — reimplements solved codegen (`type_qualifiers`, `enums`, inline fragments) for no payoff (`posture.md`) |

---

## Migration plan

Phased; each phase compiles and passes `make test` + `make check` alone.

1. **Schema module + one fragment, prove the seam.** Add `cynic` (no `http-*`
   features), `use_schema!` over the snapshot. Model **Viewer** as a
   `QueryFragment`; build the operation, POST via existing `HttpTransport`,
   decode through `query_as`. → verify: `fetch_viewer` test green
   (`viewer.rs:53-68`); `cargo deny` (`make check`).
2. **Relational schema + sync upsert.** Add entity tables/indexes/migrations
   (Pillar 2). Rewrite the sync upsert to populate entity tables from fetched
   fragments instead of the flat row. Keep a compatibility read path until the
   query layer moves. → verify: existing fetcher tests with `FakeTransport`
   unchanged (`list.rs:180+`, `delta.rs:63+`, `comments.rs:123+`); new DB tests
   for joins.
3. **Read model = fragment types.** Reconstruct `IssueRow`/comment fragments
   from joins; retarget TUI/CLI render + `From`/`.into()` bridges
   (`db/issues.rs:29-81`, `render_tests.rs:222`, `loop_tests.rs:188`) onto the
   shared fragment types. Drop the flat `issues` columns. → verify: TUI/CLI
   snapshot tests (`insta`) re-accepted intentionally; `cpd`/`cargo dupes`
   confirm dedup.
4. **Typed mutation inputs + outbox.** Model `IssueUpdateInput`/`IssueCreateInput`/
   `CommentCreateInput` via cynic input objects; introduce `mutation_outbox`;
   move API mutations behind the sync drainer; replace `popup.rs` direct-spawn
   with enqueue (Pillar 3). → verify: mutation tests assert the same variables
   JSON (`mutations.rs:294-324`); new outbox drain + reconcile tests; an
   offline-write test (enqueue with no transport, drain later).
5. **Delete the corpse.** Remove unused hand-written structs; `cargo machete`
   clean.

### Success criteria

- `make test` and `make test --features sim` green at every phase.
- `make check` green (`cargo deny`, `cargo machete`, `cpd`, `cargo dupes`).
- API calls originate **only** in the sync layer (grep: no `HttpTransport` use
  under `src/tui/`); the TUI write path enqueues, never POSTs.
- A query/schema mismatch and a fragment-field-without-storage both fail fast.

---

## Risks / open questions

Resolved (primary-source verified):
- **cynic shares structs across operations?** Yes — `lib.rs:54-67`.
- **cynic decodes via serde, so the transport survives?** Yes — `http.rs:101,222`
  (`DeserializeOwned`); we skip its `http-*` features.
- **Inline fragments / unions (Notifications)?** Yes — cynic README features;
  graphql_client `query/selection.rs:18`.
- **graphql_client fragment sharing is per-operation?** Yes — `codegen.rs:248`.
- **Whole-schema bloat?** Avoided by both (used-types-only / hand-authored).

Open (to settle at/after review):
- **Outbox vs delta-sync ordering** — the central write-path correctness
  question (Pillar 3). Must be specified before Phase 4.
- **Custom scalar policy** — `DateTime`/`ID` newtypes vs `String`. Affects every
  timestamp field.
- **Supply chain** — `cynic` + `cynic-codegen` must pass `cargo deny`; confirm
  empirically in Phase 1. Footprint (`cynic-parser`, `proc-macro2`, `syn`,
  `quote`) overlaps existing build-deps. Fall back to graphql_client if rejected.
- **`build.rs` unification** — the search-grammar codegen and the cynic schema
  module both consume the snapshot. Leave separate for now; unify later.

## Decisions required at review

1. Architecture: adopt the **API-only-via-sync / TUI-only-via-DB** target?
2. Type layer: **cynic** (shared owned fragment types, recommended) vs
   graphql_client (types stay sync-internal)?
3. Scope/sequencing: land Pillars 1–2 first and treat the outbox (Pillar 3) as a
   follow-up, or commit to all three together?
