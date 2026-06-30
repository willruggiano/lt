# Generate Linear API Types From the GraphQL Schema (ADR)

## Status

Proposed — `Refs: ENG-31`

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
  generates only the **search grammar** (filter/sort stems) and validates the
  TOML allowlist against `IssueFilter`/`IssueSortInput`
  (`build.rs:656-689`). It does **not** generate any API response types.

### Current hand-written type surface

82 `struct`/`enum` deserialization types carry the API layer
(`grep -rE 'struct |enum ' $(grep -rl Deserialize src)`). The concentration:

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

There are ~13 GraphQL operations across 7 files, each redeclaring its own
response shape. The duplication is acute for trivial wrappers: `State`,
`IssueState`, `IssueDetailState`, `NotificationIssueState` are all
`{ name: String }` or `{ id, name }`; `Team`/`IssueTeam`/`IssueDetailTeam`/
`NotificationIssueTeam` likewise; `PageInfo` is shared by hand
(`types.rs:16-22`), the rest are copies. Every field carries a manual
`#[serde(rename = "camelCase")]` (e.g. `types.rs:60`, `notifications.rs:58-63`).

### The transport seam (unaffected by this work)

All operations flow through one object-safe trait and one free helper:

```
operation const &str ──┐
serde_json variables ──┤
                       ▼
        GraphqlTransport::query(query, vars) -> Value     (client.rs:14-18)
                       │  unwraps { data, errors } envelope (client.rs:59-71)
                       ▼
        query_as::<T>(...) -> T   serde_json::from_value    (client.rs:75-82)
```

`query_as` is generic over any `DeserializeOwned`. **Whatever generates `T` is
orthogonal to the transport.** This is the seam every option plugs into; none of
the options below touch `client.rs`, `HttpTransport`, or `FakeTransport`.

### Not every "type" is an API type

Three categories masquerade as API types but are **domain/view types** assembled
locally, not deserialized from any response:

- `IssueDetail` / `IssueRef` (`types.rs:48-75`) are built from the **SQLite
  cache**, not a GraphQL detail query (`src/tui/detail.rs:206-243`,
  `populate_relations` at `detail.rs:245-280`).
- `Viewer` (`viewer.rs:21-27`) is a flattened projection of `ViewerData`.
- `db::Issue` (`src/db/issues.rs:8-27`) is the row type, bridged to the API
  `Issue` by hand-written `From` impls (`db/issues.rs:29-81`).

These must **stay hand-written**. Generation targets only the
deserialized-from-the-wire response shapes and the typed variable inputs.

---

## The core constraint: usage is selection-shaped, not type-shaped

The naive reading of ENG-31 — "generate a Rust type per GraphQL type" — is
wrong, and the schema proves it. The `Comment` object type in the schema
(`build/linear-schema-definition.graphql:2829`) has dozens of fields, many of
which are themselves paginated sub-connections taking arguments:

```graphql
type Comment implements Node {
  agentSession: AgentSession
  agentSessions(after: String, before: String, first: Int, ...): AgentSessionConnection!
  aiPromptProgresses(after: String, filter: AiPromptProgressFilter, ...): AiPromptProgressConnection!
  ... (dozens more)
}
```

The code that consumes a comment uses exactly three fields — `body`,
`createdAt`, `user { name }` (`sync/comments.rs:18-30`, `types.rs:24-41`).

A whole-schema generator would emit 524 structs — recursive, almost entirely
`Option`, almost entirely unused — which is the precise opposite of the
posture this repo mandates: "Minimum code that solves the problem. Nothing
speculative." (`docs/rules/posture.md` §2). It would also have to invent a
policy for field-arguments, which have no struct representation.

The types the codebase actually wants are **shaped to the selection set of each
operation** — the same shape they have today, minus the hand-maintenance. That
is the design target, and it determines the tool choice.

```
   GraphQL object type (Comment: ~50 fields)   ──X── do NOT mirror
   Operation selection set ({ body createdAt user{name} })  ──> generate THIS
```

---

## Goals / Non-goals

**Goals**

- Replace the hand-written response structs and `#[serde(rename)]` churn with
  types generated from operations + the committed schema.
- Validate every operation against the schema at build time; a drift between a
  query and the schema fails the build (parity with the existing allowlist
  gate, `build.rs:684-689`).
- Generate typed **variable inputs** for the mutations currently built with
  untyped `serde_json::json!` maps (`mutations.rs:257-272`).
- Generate the closed **enums** the code currently stringly-types — notably
  `PaginationSortOrder { Ascending, Descending }`
  (`build/linear-schema-definition.graphql:22406`), hardcoded as `"Ascending"`/
  `"Descending"` in `build_sort` (`build.rs:606-612`).
- Preserve the `GraphqlTransport` / `query_as` seam unchanged.

**Non-goals**

- Generating the whole schema (rejected above).
- Generating domain/view types (`IssueDetail`, `IssueRef`, `Viewer`,
  `db::Issue`) — they are not wire types.
- Replacing the search-grammar codegen (`search_stems.rs`); that is a separate
  concern and can be unified later, not now.
- Changing the HTTP client or the optimistic-write model.

---

## Options considered

### A. Whole-schema type generation — rejected

Emit one Rust type per schema type. Rejected by the core constraint above: 524
recursive, mostly-`Option`, mostly-unused structs; no answer for
field-arguments; violates `posture.md` §2. This is the literal reading of the
issue and it is the wrong one.

### B. `graphql_client` (operation-first codegen) — recommended

`graphql_client` generates a module per operation from a `.graphql` query file +
the schema, with types shaped exactly to the selection set. Primary evidence
from the vendored source
(`~/.cargo/registry/.../graphql_client_codegen-0.16.0`):

- **Operation-first, used-types-only.** Enums and input objects are generated
  only when an operation references them, via `all_used_types`
  (`codegen/enums.rs:30`, `codegen.rs:27`). No unused surface.
- **Selection-shaped responses.** Nullability → `Option`, list → `Vec` via the
  `Required`/`List` type qualifiers (`type_qualifiers.rs:1-10`).
- **Automatic serde rename.** `camelCase` → `snake_case` emits
  `#[serde(rename = ...)]` only when needed (`codegen/shared.rs:26-32`) —
  deletes every manual rename we maintain today.
- **Forward-compatible enums.** Generated enums carry an `Other(String)`
  catch-all and a hand-rolled de/serialize, so a new server enum value does not
  break deserialization (`codegen/enums.rs:62-95`). This matters: see
  `notifications.rs:56` where `type` is already kept as a bare `String` to dodge
  exactly this.
- **Inline fragments.** Supported (`query/selection.rs:18,68`,
  `query/validation.rs:8-16`), which the Notifications operation requires
  (`... on IssueNotification`, `notifications.rs:19-21`).

Mechanism: a `#[derive(GraphQLQuery)]` proc-macro on a marker struct with
`#[graphql(schema_path=..., query_path=...)]` (`graphql_client/src/lib.rs`).
Queries move from inline `const &str` into `.graphql` files (required by
`query_path`).

Interop with the existing transport is clean — `query_as` already takes any
`DeserializeOwned`:

```
   marker struct  ──derive──>  module { ResponseData, Variables, QUERY const }
                                          │
   Op::build_query(vars) -> QueryBody { query, variables }   (graphql_client trait)
                                          │
   transport.query(body.query, to_value(body.variables)) -> Value   (UNCHANGED)
                                          │
   serde_json::from_value::<ResponseData>(...)                       (query_as, UNCHANGED)
```

Cost / cons:
- New deps: `graphql_client` (runtime trait crate) + `graphql_client_codegen` +
  `graphql_query_derive`. Footprint largely overlaps existing build-deps
  (`graphql-parser`, `syn`, `quote`, `proc-macro2` are already present;
  `Cargo.toml:34-40`). Disable the `reqwest` feature — we keep our own client.
  Must clear `cargo deny` (`make check`).
- Per-operation modules re-emit small nested structs (each query gets its own
  `State`), so cross-operation struct *sharing* is lost. In exchange the shapes
  are guaranteed correct against the schema and free to maintain. Shared
  projections (e.g. mapping into `db::Issue`) live in our `From` impls, not in
  shared response types.
- Two codegen mechanisms in the tree (this + the search `build.rs`). They serve
  different concerns; unification is a later, optional step.

### C. `cynic` — rejected for this issue

`cynic` is "bring your own types": you write Rust structs with
`#[derive(QueryFragment)]` and it generates **GraphQL from them**, checking
against a registered schema (`cynic-3.13.2/README.md`, "uses Rust structs to
define queries and generates GraphQL from them"). That is the inverse of
"generate types from the spec." It gives more cross-query struct reuse but adds
a schema-registration model and keeps the structs hand-written — more
boilerplate, less aligned with ENG-31. Reasonable for a query-authoring DX
overhaul; not what this issue asks for.

### D. Extend the existing `build.rs` to generate operation types — alternative

Keep a single codegen mechanism: teach `build.rs` to parse our `.graphql`
operations and emit selection-shaped response + variable types into `OUT_DIR`,
consumed via `include!` (the model already used for `search_stems.rs`,
`build.rs:794`). No new runtime deps; full control; consistent with the repo's
existing codegen culture.

The honest cost: a correct operation→Rust mapper is materially harder than the
current allowlist extractor (`build.rs:63-81`, which only reads input-object
field name→type pairs). It must handle selection sets, nested objects,
nullability/list wrapping, naming/dedup, enum generation with a fallback
variant, and **inline fragments on interfaces** (the Notifications case). That
is precisely the logic `graphql_client_codegen` already implements and tests
(`type_qualifiers.rs`, `codegen/enums.rs`, `query/selection.rs`). Reimplementing
it is the "clever abstraction without a real payoff" `posture.md` warns against
— unless avoiding the dependency is judged worth the maintenance.

### Recommendation

**Option B (`graphql_client`).** It matches the actual usage shape, generates
only what operations use, deletes the entire manual-rename burden, gives
forward-compatible enums, supports the one inline-fragment operation, and plugs
into the existing transport without touching it. Option D is the fallback if the
dependency is rejected at review; its cost is reimplementing a solved problem.

The decision to settle at review:

| Axis                    | B `graphql_client` | D extend `build.rs` |
|-------------------------|--------------------|---------------------|
| New runtime deps        | yes (3, overlapping)| none               |
| Code we maintain        | `.graphql` files    | mapper + `.graphql` |
| Schema-drift gate       | built in            | we build it         |
| Inline fragments        | done                | we build it         |
| Enum fwd-compat         | done                | we build it         |
| Codegen mechanisms      | 2                   | 1                   |

---

## Detailed design (Option B)

### Layout

```
build/linear-schema-definition.graphql      (existing snapshot, reused)
src/linear/operations/
  issues.graphql            Issues(list) + Issues(delta share the doc)
  issue_comments.graphql
  notifications.graphql
  viewer.graphql
  teams.graphql
  workflow_states.graphql
  team_members.graphql
  issue_update.graphql      mutation
  issue_create.graphql      mutation
  comment_create.graphql    mutation
src/linear/operations.rs    marker structs, one per operation, #[derive(GraphQLQuery)]
```

Each marker:

```rust
#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "build/linear-schema-definition.graphql",
    query_path  = "src/linear/operations/issues.graphql",
    response_derives = "Debug, Clone, PartialEq"
)]
pub struct Issues;
```

### What gets generated vs kept

```
GENERATED (replaces hand-written):
  response structs for all 13 operations  ── types.rs / mutations.rs /
                                              notifications.rs / viewer.rs /
                                              comments.rs / new.rs response halves
  typed Variables structs                  ── replaces serde_json::json! maps
  input objects (IssueUpdateInput, IssueCreateInput, CommentCreateInput)
  enums referenced by ops (PaginationSortOrder, WorkflowState type, ...)

KEPT (hand-written, adapted):
  GraphqlTransport / query_as / HttpTransport / FakeTransport   (client.rs)
  IssueDetail, IssueRef                                          (view types)
  Viewer projection
  db::Issue + From<db::Issue> for <generated Issue node>         (db/issues.rs)
  priority_label_to_u8                                           (types.rs:178)
```

### Bridging generated types to domain types

The `From` impls in `db/issues.rs:29-81` and the `.into()` conversions
(`render_tests.rs:222`, `loop_tests.rs:188`) currently target
`crate::linear::types::Issue`. They retarget to the generated `issues::IssuesIssuesNodes`
(or a re-export aliasing it to a stable name). The conversion logic is
unchanged; only the type path moves. `priority_label_to_u8` stays as the
lossy label→u8 parse it is today (`types.rs:175-186`).

### Sort order enum

`build_sort` hardcodes `"Ascending"`/`"Descending"` (`build.rs:606-612`). Once
`IssueSortInput` variables are generated, that string pair becomes the generated
`PaginationSortOrder` enum, removing a stringly-typed value. This is an
incidental win, not a forcing function; the search-grammar codegen can keep its
own copy until a later unification.

---

## Schema provenance

The snapshot `build/linear-schema-definition.graphql` is committed and is the
single source of truth for both this codegen and the existing allowlist gate
(`docs/architecture.md:97-100`). There is currently **no documented refresh
procedure**. This ADR adds one as a follow-up note (out of scope to implement
here): a `make` target that re-downloads the schema via introspection, so the
snapshot is reproducible rather than mystery-committed. Generation correctness is
pinned to whatever snapshot is committed, which is the desired property —
generation is deterministic and offline, consistent with the test posture
("Tests touch no network", `docs/rules/testing.md`).

---

## Migration plan

Phased, each phase compiles and passes `make test` + `make check` on its own.

1. **Add the dep, prove the seam.** Add `graphql_client` (no `reqwest`
   feature). Migrate the **Viewer** operation only (smallest, 4 types). Wire
   `Viewer::build_query` → existing transport. Verify `fetch_viewer` test still
   passes (`viewer.rs:53-68`). → verify: `make test`, `make check` (deny gate).
2. **Migrate read operations.** Issues(list+delta), IssueComments,
   Notifications (exercises inline fragments), Teams, WorkflowStates,
   TeamMembers. Retarget `From`/`.into()` bridges. → verify: existing fetcher
   tests with `FakeTransport` are unchanged and green
   (`list.rs:180+`, `delta.rs:63+`, `comments.rs:123+`, `notifications.rs:149+`).
3. **Migrate mutations + typed inputs.** IssueUpdate, IssueCreate,
   CommentCreate; replace the `serde_json::json!` input builders
   (`mutations.rs:257-272`) with generated `*Input` structs. → verify: mutation
   tests assert the same variables JSON (`mutations.rs:294-324`).
4. **Delete the corpse.** Remove the now-unused hand-written structs from
   `types.rs`/`mutations.rs`/etc. Keep only view/domain types and helpers.
   → verify: `cargo machete` (unused dep gate) + `cpd`/`cargo dupes`
   (`make check`) confirm the duplication is gone.

Rollback per phase is a revert; the transport seam never changes, so phases are
independent.

### Success criteria

- `make test` and `make test --features sim` green at every phase
  (`docs/rules/testing.md`).
- `make check` green, including `cargo deny`, `cargo machete`, `cpd`,
  `cargo dupes` (`Makefile:12-22`).
- Net deletion in the API type layer; the 82-type hand-written surface drops to
  view/domain types + helpers only.
- A deliberate query/schema mismatch fails the build (drift gate).

---

## Risks / open questions (resolved)

- **Does `graphql_client` handle the inline-fragment notification query?** Yes —
  `query/selection.rs:18,68`, `query/validation.rs:8-16`. (Verified in source.)
- **Does it bloat the build with the whole schema?** No — used-types-only
  (`codegen/enums.rs:30`). (Verified.)
- **Will new server enum values break deserialization?** No — `Other(String)`
  fallback (`codegen/enums.rs:62-95`). (Verified.)
- **Does the transport need changes?** No — `query_as` is generic over
  `DeserializeOwned` (`client.rs:75-82`); generated `ResponseData` plugs in.
- **Supply chain.** New deps must pass `cargo deny`; footprint overlaps existing
  build-deps. To be confirmed empirically in Phase 1 — if `deny` rejects them,
  fall back to Option D.

## Decision required at review

1. Option **B** (`graphql_client`, recommended) vs **D** (extend `build.rs`).
2. If B: accept the three new dependencies pending the Phase-1 `cargo deny`
   check.
