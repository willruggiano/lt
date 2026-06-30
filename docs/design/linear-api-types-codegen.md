# Generate Linear API Types From the GraphQL Schema (ADR)

## Status

Proposed — `Refs: ENG-31`

> **Revision (PR #26 review).** The first draft scoped this to "stop
> hand-writing response structs." The review pushed back on one line —
> _"per-operation modules re-emit small nested structs, so cross-operation
> sharing is lost"_ — and asked us to do better, with three threads: a
> relational DB schema, offline-capable mutations, and **GraphQL fragments as
> the shared type at the `tui <-> db/api` boundary**, all under one rule:
> _Linear's GraphQL API is hit only by the sync thread; the TUI touches only the
> local database._ This revision answers that. The type-codegen decision is now
> one pillar of a local-first data architecture, and the recommendation changes
> accordingly (cynic over `graphql_client` — see
> [Pillar 1](#pillar-1--shared-fragment-types-graphql_client-vs-cynic)).

## Context

The Linear API response/variable types are hand-written serde structs scattered
across the crate, one ad-hoc set per GraphQL operation. ENG-31 asks to generate
them "from the graphql spec," replacing "the vast majority, if not all, of the
types defined in the `linear::types` crate."

Two pieces of infrastructure already exist and shape every option below:

- A committed schema snapshot: `build/linear-schema-definition.graphql` (37,149
  lines; 524 `type`, 371 `input`, 95 `enum`, 8 `interface`, 8 `scalar`).
- A `build.rs` codegen seam that already parses that schema with
  `graphql-parser` and emits Rust via `quote`/`syn`/`prettyplease` into
  `OUT_DIR` (`build.rs:12`, `build.rs:63-81`, `build.rs:786-796`). It currently
  generates only the **search grammar** and validates the TOML allowlist against
  `IssueFilter`/`IssueSortInput` (`build.rs:656-689`). It generates no API
  response types.

### Current hand-written type surface

82 `struct`/`enum` deserialization types carry the API layer
(`grep -rE 'struct |enum ' $(grep -rl Deserialize src)`):

```text
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
`IssueDetailState`, `NotificationIssueState` are all `{ name }` or
`{ id, name }`; the `Team*` family likewise. Every field carries a manual
`#[serde(rename = "camelCase")]` (`types.rs:60`, `notifications.rs:58-63`).

### The transport seam (preserved by every option below)

All operations flow through one object-safe trait and one free helper:

```text
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
generated types are plain serde types (`graphql_client` emits
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
field-arguments — the opposite of [[posture.md#2. Simplicity First]] ("Minimum
code… Nothing speculative"). The types the code wants are **shaped to the
selection set of each operation**. That fact drives the whole design.

```text
   GraphQL object type (Comment: ~50 fields)   ──X── do NOT mirror
   Operation selection set ({ body createdAt user{name} })  ──> model THIS
```

---

## Target architecture (per review)

One rule reorganizes the data flow: **the GraphQL API is reached only by the
sync layer; the TUI/CLI read and write only the local SQLite database.** Today
this is violated on the write path — the TUI spawns a worker that calls
`HttpTransport` directly and reverts SQLite on failure
(`src/tui/popup.rs:352-393`, [[architecture.md]] §TUI). The target:

```text
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

### Enforce the rule structurally, not by convention

"The TUI never hits the API" is worthless as a comment and a grep. Make it a
_compile error_. Split the single crate into a Cargo workspace where the
dependency edges encode the rule:

```text
   lt-types   (cynic QueryFragment structs + schema module; the shared currency)
      ▲   ▲
      │   └──────────────┐
   lt-db                 lt-sync ──depends on──> cynic / HttpTransport (the ONLY API edge)
      ▲                     ▲
      │                     │
   lt-tui ──depends on──> lt-db        lt-tui has NO dependency on lt-sync or cynic
   lt-cli ──┘
```

`lt-tui` not listing `lt-sync` or the GraphQL client in its `[dependencies]`
means an API call from the render/event path _does not compile_. This is the
systematic answer to "how do we prevent this class of error rather than
best-effort": the cargo dependency graph is the enforcement mechanism, checked
on every build. It also gives the type layer a natural home (`lt-types`) that
both `lt-db` (read-model reconstruction) and `lt-sync` (fetch/decode) import
without a cycle.

The workspace split is larger than the type migration and is sequenced as its
own PR in the stack (below); the architecture is designed for it from the start
so the crate boundaries are not retrofitted.

---

## Pillar 1 — Shared fragment types: `graphql_client` vs cynic

The review's specific objection — _cross-operation struct sharing is lost_ — is
real and library-dependent. Primary evidence from the vendored sources:

**`graphql_client` (0.16.0)** is _query-first_: a `#[derive(GraphQLQuery)]`
marker struct points at a `.graphql` file + schema, and codegen emits a module
of response types shaped to the selection set. It generates only used types
(`codegen/enums.rs:30`), maps nullability/list to `Option`/`Vec`
(`type_qualifiers.rs:1-10`), auto-emits serde renames
(`codegen/shared.rs:26-32`), gives enums an `Other(String)` forward-compat
fallback (`codegen/enums.rs:62-95`), and supports inline fragments
(`query/selection.rs:18,68`) — needed for the Notifications
`... on IssueNotification` (`notifications.rs:19-21`). **But fragments are
rendered per `BoundQuery`** — `generate_fragment_definitions` iterates one
operation's `all_used_types.fragment_ids()` (`codegen.rs:248-252`). A fragment
used by two separate derives is emitted twice, in two modules. The only way to
share is to put every operation in one document under one derive; even then the
types are codegen-named, deeply nested, and `Deserialize`-only — awkward to
_construct_ from DB rows, which the read model requires.

**cynic (3.13.2)** is _struct-first_ ("a bring your own types GraphQL client",
`cynic-3.13.2/README.md`). You hand-author a struct and
`#[derive(cynic::QueryFragment)]`; cynic checks it against the schema and
generates the GraphQL from it. The decisive property, from the crate docs
(`cynic-3.13.2/src/lib.rs:54-67`):

> This `Film` struct can now be used as the type of a field on **any other**
> `QueryFragment` struct…

That is first-class cross-operation sharing: define `IssueRow` once, reuse it as
a field in the list query, the delta query, and the detail query. Because the
struct is **ours**, it can carry methods and `From`/`Into` impls and be
**constructed by hand from a SQL join** — which is precisely what the "DB
returns fragment types" contract needs. Interfaces/unions are supported (README
features; the Notifications case). Responses are serde-decoded (`http.rs:101`),
so the existing transport seam holds; we do not enable cynic's `http-*`
features.

```text
  `graphql_client`                          cynic
  ──────────────                          ─────
  .graphql query ──derive──> module       struct + derive ──> GraphQL string
  types: codegen-owned, per-op            types: hand-owned, shared across ops
  share: only within one document         share: any struct as any field
  construct from DB row: awkward          construct from DB row: plain struct
  author cost: write .graphql only        author cost: write structs (querygen helps)
```

### Recommendation: cynic

For the _original_ narrow scope (kill hand-written response structs, types stay
inside the sync layer), `graphql_client` is the lower-effort win and was the
first draft's pick. For the _expanded_ architecture — where fragment types are
the durable, shared, hand-constructed contract between DB and TUI — cynic is the
right tool, because it makes the fragment type a normal owned Rust struct usable
in all three roles (fetch / store-read / render). `graphql_client`'s
per-operation, codegen-owned, Deserialize-only types cannot fill the DB-return
role without a second hand-written domain layer, which defeats the point.

Honest costs of cynic:

- **Structs are hand-authored** (cynic verifies, doesn't write them). `querygen`
  bootstraps from a query; net authoring is higher than `graphql_client`.
- **A `use_schema!` module** must be generated from the snapshot
  (`lib.rs:18-22`) — one-time wiring.
- **Custom scalars** (`DateTime`, `ID`) get **newtypes** (`impl cynic::Scalar`),
  not bare `String` — decided in review. Today every timestamp is a `String`
  (`types.rs:66-69`) and ids are bare `String`; distinct types stop a raw id or
  an unparsed timestamp being passed where the other is expected, per
  [[posture.md]] ("distinct named types over bare primitives where they carry
  meaning"). Small, deliberate, decided.
- **One struct plays three roles** (wire selection + storage read model + view
  model). For a local-first cache this coupling is _desirable_ — the cache
  stores exactly what the UI needs, sourced from exactly that selection — but a
  change to the UI's needs deliberately ripples to the fetch selection and the
  storage contract. We accept that as the single-source-of-truth property.

`graphql_client` remains the fallback if hand-authoring is rejected, in which
case fragment types stay a sync-internal concern and the TUI↔DB contract is a
separate hand-written domain layer (status quo, minus the response-struct
churn).

### Not every "type" is a fragment type

Wire-sourced selections become fragment types. Locally-assembled types stay
hand-written: `IssueDetail`/`IssueRef` are built from the cache, not a query
(`tui/detail.rs:206-280`); `Viewer` (`viewer.rs:21-27`) is a projection;
`db::Issue` (`db/issues.rs:8-27`) is the row type. Under the new architecture
several of these _become_ DB-returned fragment types (e.g. the issue read
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

The schema is natively relational. `Issue` references `team: Team!`,
`state: WorkflowState!`, `assignee: User`, `creator: User`, `project: Project`,
`cycle: Cycle`, `parent: Issue`, and `labels` (a connection)
(`build/linear-schema-definition.graphql`, `type Issue`). Proposed tables, one
per entity, FKs + indexes:

```text
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

```text
fragment IssueRow on Issue {            SELECT i.*, t.name, s.name, a.name, ...
  id identifier title priority           FROM issues i
  state { name }            ───────►     JOIN workflow_states s ON s.id = i.state_id
  assignee { name }                      JOIN teams t           ON t.id = i.team_id
  team { key name }                      LEFT JOIN users a      ON a.id = i.assignee_id
  labels { nodes { name } }              LEFT JOIN issue_labels … (aggregated)
}                                        → build IssueRow { … } in Rust
```

Nested fragments (`StateName on WorkflowState { name }`,
`Actor on User { name }`) map to joined columns and are the reuse units across
the list/delta/detail read models. This makes the relational schema's coverage a
checkable property: **every field any fragment selects must have a column or
join** — a drift gate analogous to the existing allowlist gate
(`build.rs:684-689`).

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

An outbox alone does **not** fix the core race, and the first draft was wrong to
frame it as an ordering problem. A bare outbox still writes the user's edit into
the same physical row that delta sync overwrites; "apply outbox after merge" /
"skip rows with pending" / "reconcile by updatedAt" are all best-effort patches
over a shared-mutable-row model — they depend on getting a predicate exactly
right on every code path, forever. The root cause is that **one row holds two
different things**: confirmed server truth and unconfirmed local intent.
Separate them and the race becomes unrepresentable.

### Base / overlay split (the structural fix)

```text
   issues          ─ confirmed server truth ONLY. Written by sync + acks.  (BASE)
   pending_overlay ─ local intent, keyed by (entity_id, field). Written by  (OVERLAY)
                     UI enqueue; deleted on ack. 1:1 with the outbox.
   read model      = merge(BASE, OVERLAY)  — overlay wins per field, computed at read
```

A delta pull writes only `issues` (base); it has **no SQL statement that can
touch `pending_overlay`**. The value the UI renders is `merge(base, overlay)`,
so a concurrent delta merge cannot clobber pending intent — there is no ordering
to get right because the delta write cannot reach the cell that holds intent:

```text
   UI:   t0  BEGIN; upsert pending_overlay(issue,state=Done); INSERT outbox; COMMIT   [no network]
   sync: t1  upsert issues(state=Todo)                 -- base only
   read: ∀t  merge(base, overlay) => state=Done        -- no flicker, no clobber
   ack:  t2  BEGIN; upsert issues(state=Done); DELETE pending_overlay(issue,state); COMMIT
```

The outbox is still needed — it records the _command_ to replay against the API
— but it is paired with the overlay, not a substitute for it:

```text
mutation_outbox(
  seq        INTEGER PK AUTOINCREMENT,  -- total order
  op_type    TEXT,                      -- IssueUpdate | IssueCreate | CommentCreate
  entity_id  TEXT,                      -- target (or client temp id for creates)
  variables  TEXT,                      -- JSON: the typed Variables payload
  status     TEXT, attempts INTEGER, last_error TEXT, created_at TEXT)
```

The mechanism is a stack of four primitives, in order of how much they carry:

1. **Base/overlay split** — load-bearing; makes the clobber unrepresentable.
2. **Transactional outbox** — the overlay write and the outbox `INSERT` commit
   in one rusqlite transaction (`Connection::transaction()`), so intent is never
   half-recorded. Replaces today's unrelated spawn-then-maybe-revert
   (`popup.rs:343-389`).
3. **Single-writer reconcile loop** — one owner serializes all base writes (sync
   upserts + acks) so they never interleave; fits the existing worker+mpsc model
   ([[architecture.md]] §TUI). Today there are two independent writers
   (`popup.rs:354`, `sync/mod.rs`).
4. **Hardening:** an `updated_at`/version guard keeps the base monotonic against
   stale delta pages; an **outbox rebase** on each new base retires
   server-satisfied commands and surfaces genuine field-level conflicts. These
   carry conflict _resolution/UX_, layered on a default that is already safe.

This also enforces the target rule on the write path: the TUI opens no
`HttpTransport` (today it does, `popup.rs:354-358`) — it writes overlay + outbox
and reads the merge; the API edge lives only in the sync drainer.

> Evidence: this section follows a focused research pass that compared the model
> against event-sourcing, CRDTs (cr-sqlite — rejected: it merges cr-sqlite
> _peers_, and Linear's server is a non-CRDT authority, not a peer), and
> snapshot-diff; and re-confirmed **SQLite/rusqlite** over redb / native_db (no
> FTS, no joins) and sled (beta; its README recommends SQLite) — SQLite is the
> only store giving FTS5 + relational joins + multi-table transactional
> atomicity at once. Claims are grounded in repo `file:line` and crate sources
> read on disk.

---

## Options considered (type layer)

| Option                                                 | Verdict                                                                                                              |
| ------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------- |
| A. Whole-schema type generation                        | Rejected — the core constraint (524 unused recursive types)                                                          |
| B. `graphql_client` (query-first)                      | Fallback — least effort, but per-op types can't be the DB↔TUI contract                                               |
| C. **cynic (struct-first, shared fragments)**          | **Recommended** for the expanded architecture                                                                        |
| D. Extend `build.rs` to a custom operation→type mapper | Rejected — reimplements solved codegen (`type_qualifiers`, `enums`, inline fragments) for no payoff ([[posture.md]]) |

---

## Migration plan — stacked PRs

Decided in review: commit to all three pillars, shipped as a **stack** of PRs,
each rebasing on the one below. Each PR compiles and passes `make test` +
`make check` on its own; the stack lets reviewers approve and merge
incrementally without a single mega-diff.

1. **`cynic` + schema module + one fragment.** Add `cynic` (no `http-*`
   features), `use_schema!` over the snapshot. Model **Viewer** as a
   `QueryFragment`; build the operation, POST via existing `HttpTransport`,
   decode through `query_as`. → verify: `fetch_viewer` test green
   (`viewer.rs:53-68`); `cargo deny` (`make check`).
2. **Relational schema + base/overlay + sync upsert** (Pillar 2). Entity tables/
   indexes/migrations **plus the `pending_overlay` table** (introduced with the
   schema, not retrofitted); rewrite the sync upsert to populate entity tables
   (the base) from fetched fragments instead of the flat row. Keep a
   compatibility read path until the query layer moves. → verify: existing
   fetcher tests with `FakeTransport` unchanged (`list.rs:180+`, `delta.rs:63+`,
   `comments.rs:123+`); new DB tests for joins; a **clobber test** (delta write
   to base leaves overlay intact).
3. **Read model = fragment types.** Reconstruct `IssueRow`/comment fragments
   from joins; retarget TUI/CLI render + `From`/`.into()` bridges
   (`db/issues.rs:29-81`, `render_tests.rs:222`, `loop_tests.rs:188`) onto the
   shared fragment types. Drop the flat `issues` columns. → verify: TUI/CLI
   snapshot tests (`insta`) re-accepted intentionally; `cpd`/`cargo dupes`
   confirm dedup.
4. **Typed inputs + outbox drain** (Pillar 3). Read model =
   `merge(base, overlay)`; model
   `IssueUpdateInput`/`IssueCreateInput`/`CommentCreateInput` via cynic input
   objects; the transactional outbox (overlay write + enqueue in one txn); move
   API mutations behind the single-writer sync drainer; replace `popup.rs`
   direct-spawn with enqueue. Per-field overlay rows coalesce per entity into
   one typed patch (the `Field<T>` model). → verify: mutation tests assert the
   same variables JSON (`mutations.rs:294-324`); a coalescing test (multiple
   per-field edits to one issue → one `issueUpdate` with `null` preserved);
   outbox drain + reconcile + offline-write tests.
5. **Workspace split** (structural enforcement). Break the crate into `lt-types`
   / `lt-db` / `lt-sync` / `lt-tui` / `lt-cli` so `lt-tui` cannot depend on
   `cynic`/`HttpTransport`. → verify: the workspace builds; an API import from
   `lt-tui` fails to compile.
6. **Delete the corpse.** Remove unused hand-written structs; `cargo machete`
   clean.

### Success criteria

- `make test` and `make test --features sim` green at every PR.
- `make check` green (`cargo deny`, `cargo machete`, `cpd`, `cargo dupes`).
- API calls originate **only** in `lt-sync` — enforced by the workspace
  dependency graph (PR 5), not a grep.
- A query/schema mismatch and a fragment-field-without-storage both fail fast.

---

## Risks / open questions

Resolved (primary-source verified):

- **cynic shares structs across operations?** Yes — `lib.rs:54-67`.
- **cynic decodes via serde, so the transport survives?** Yes —
  `http.rs:101,222` (`DeserializeOwned`); we skip its `http-*` features.
- **Inline fragments / unions (Notifications)?** Yes — cynic README features;
  `graphql_client` `query/selection.rs:18`.
- **`graphql_client` fragment sharing is per-operation?** Yes —
  `codegen.rs:248`.
- **Whole-schema bloat?** Avoided by both (used-types-only / hand-authored).

Resolved in review:

- **Supply chain — validated now, not deferred.** Ran `cargo deny check` against
  this repo's exact `deny.toml` on a probe crate depending on `cynic` +
  `cynic-codegen`: **licenses ok, bans ok, sources ok**. The cynic crates are
  `MPL-2.0` (allowed, `deny.toml:18`); the transitive tree is entirely
  `Apache-2.0`/`MIT`/`Unicode-3.0` (all allowed). No banned multiple-versions
  error (the policy is `warn`), no unknown registry/git source. The `advisories`
  check could not run in this environment (it fetches the RustSec git DB, which
  the sandbox proxy blocks); it must be run in CI, but is orthogonal to crate
  choice. Footprint note: cynic adds `cynic-parser`, `logos`, `lalrpop-util`,
  `ouroboros`, `darling` at build time; `proc-macro2`/`syn`/`quote`/`serde`
  overlap existing deps.
- **Custom scalar policy → newtypes.** `DateTime`/`ID` become distinct
  `impl Scalar` newtypes, not `String` (see
  [Pillar 1](#pillar-1--shared-fragment-types-graphql_client-vs-cynic) costs).
- **`build.rs` unification → leave separate.** The search-grammar codegen and
  the cynic schema module both read the snapshot; keep them separate for now,
  unify later. (Confirmed.)

Resolved by research (folded into Pillar 3):

- **Outbox vs delta-sync ordering** — answered structurally, not by ordering
  rules: split confirmed **base** from pending **overlay**, read model =
  `merge(base, overlay)`, so a delta pull physically cannot reach pending
  intent. Keep the outbox (records the command) + transactional enqueue +
  single-writer base loop; `updated_at` guard and outbox rebase as hardening.
- **Is SQLite the right database?** Yes — only candidate giving FTS5 +
  relational joins + multi-table transactional atomicity at once; redb/native_db
  lack FTS/joins, sled is beta. Keep `rusqlite` (bundled).

Resolved in review:

- **No version/fingerprint token exists** — checked the snapshot:
  `interface Node` exposes only `id: ID!`, and `type Issue` carries
  `updatedAt: DateTime!`, `history` (`IssueHistoryConnection`), and
  `previousIdentifiers`, but **no** `version`/`hash`/`fingerprint` field (the
  `version` fields in the schema are on unrelated types like `Release`). So the
  base-monotonicity guard uses `updatedAt`; we accept it is a server clock, not
  a strict per-entity version. If staleness bites in practice, the fallback is
  to compare a content hash we compute locally, not a server token (there is
  none).
- **Read-model materialization → start simple, then benchmark.** Begin with the
  straightforward form (`LEFT JOIN pending_overlay` + `COALESCE`, or a Rust-side
  fold) and benchmark it at scale using the seeded `sim::generate` dataset
  (`docs/design/dst.md`), per [[posture.md]]'s "add or update a focused
  benchmark." No premature materialized view.
- **Overlay sequencing → introduce with, not retrofit.** The base/overlay split
  lands **with** the relational schema (PR 2), not bolted on in PR 4 —
  confirmed.
- **Conflict UX → deferred.** Conflict reconciliation is out of scope for the
  stack; the overlay default (overlay wins per field until ack) is already safe.
- **Overlay granularity → per-field, with per-entity coalescing.** Research
  confirmed it is feasible type-safely. `IssueUpdateInput` is a fully-optional
  partial input (`build/linear-schema-definition.graphql:15321-15435`), so N
  per-field edits to one issue collapse into one `issueUpdate`. cynic supports
  type-safe partial inputs via `#[derive(InputObject)]` + per-field
  `#[cynic(skip_serializing_if=...)]`. The one gap — neither cynic 3.13.2 nor
  `graphql_client` 0.16.0 ships a three-valued optional, so a bare `Option<T>`
  encodes only two of {absent, null, value} — is closed by a ~15-line
  `Field<T> = Absent | Null | Value` newtype wired through that skip attribute.
  Per-field overlay rows then fold deterministically into one typed patch per
  entity, with `assigneeId: null` (clear) preserved while untouched fields are
  omitted — exactly today's behavior at `mutations.rs:214`, now type-checked.
  Per-op buys nothing and loses the natural per-field merge, so per-field wins.

Follow-up (post-stack):

- **`lt outbox` command** — mirror `lt inbox`: a CLI view of pending local
  changes (the overlay/outbox contents). Conflict reconciliation deferred with
  it.

Resolved in PR 4:

- **One compiling spike** — confirmed: cynic accepts `Field<T>` against a
  nullable input field. The derive aligns it to `Option<Field<T>>` and asserts
  `Option<Field<T>>: IsScalar<Option<Marker>>`, which reduces to
  `Field<T>: IsScalar<Marker>` — satisfied by a blanket
  `impl<T, U> IsScalar<U> for Field<T> where T: IsScalar<U>`
  (`src/linear/inputs.rs`). Serialization is wired through
  `skip_serializing_if = "Field::is_absent"`.
- **`null` = "clear" scope** — `Field::Null` is exposed only for `assigneeId`
  (the one UI "clear", via the unassign popup). The other nullable FK fields are
  not yet editable, so the live-API confirmation is deferred until they are.

Still open (deferred):

- **Labels model** — `labelIds` (full replace) vs `addedLabelIds`/
  `removedLabelIds` (incremental); the incremental pair maps poorly to one
  `(entity, field)` overlay row. Labels are not yet mutated in the UI, so the
  model choice is deferred. Product call.

## Decisions (resolved in review)

1. Architecture: **adopt** API-only-via-sync / TUI-only-via-DB, enforced
   structurally by a Cargo **workspace split** (PR 5), not convention.
2. Type layer: **cynic** (shared, owned, composable fragment types). Confirmed.
3. Scope/sequencing: **all three pillars**, shipped as a **stack of PRs** (see
   migration plan). Confirmed.
