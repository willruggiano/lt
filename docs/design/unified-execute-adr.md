# One `execute` for Reads and Writes (ENG-67)

## Status

Proposed. Design target for ENG-67. Collapses the runtime's data surface to a
single generic verb and moves everything else behind it. This supersedes the
write-path non-goal of [[operation-seam-adr.md]] and **replaces its Decision 4
(typed subscription slots) and Decisions 5–6 (entity-keyed propagation and
freshness)**: there is no scoped invalidation. A single unscoped signal
refreshes every active view.

## Context

After [[operation-seam-adr.md]], the read side is generic but split across three
verbs (`load`, `subscribe`, `refresh`), and the write side is three
entity-specific methods (`create_issue`, `update_issue`, `create_comment`,
`crates/lt-runtime/src/runtime.rs:754-772`). The generality is real but the
surface is wide, and it leaks implementation: `subscribe` hands the TUI a typed
`Subscription<T>` slot (`crates/lt-runtime/src/subscription.rs`), the verbs
return three different shapes, and the write methods each name an entity.

The machinery to unify is already in place. The transport runs _any_ operation
generically and returns its typed output:

```rust
// crates/lt-upstream/src/client.rs:74-84
pub fn execute<Op: GraphqlOperation>(transport: &dyn GraphqlTransport, variables: Op::Variables)
    -> Result<Op::Output>
{ /* serialize vars → query → decode the response as Op::Output */ }
```

Each operation is statically a query or a mutation: `IssuesQuery` selects
`#[cynic(graphql_type = "Query")]` (`crates/lt-types/src/issues.rs:491`),
`IssueCreateMutation` selects `"Mutation"` (`issues.rs:559`). The _kind_ is a
property of the type, and it is the only classification the local seam needs.

The intent of ENG-67 is to expose one verb over all of it:

```rust
pub fn execute<Op>(&self, vars: Op::Variables) -> Result<Op::Output>;
```

Everything else — optimistic writes, the op-log, live refresh and invalidation,
the SQL — is an implementation detail, hidden behind that surface and derived
from `Op`.

```text
   TODAY                                     ENG-67
   ─────────────────────────                ─────────────────────────
   load::<Op>(vars)      -> Op::Output       ┐
   refresh::<Op>(vars)   -> touched keys     │
   subscribe::<Op>(vars) -> (Sub<T>, Output) ├──►  execute::<Op>(vars) -> Op::Output
   create_issue(input)   -> String           │     (query → cache read;
   update_issue(vars)    -> ()               │      mutation → optimistic apply,
   create_comment(input) -> ()               ┘      read the optimistic Output back)

   Subscription<T>, the op-log, scoped invalidation  ── all internal, derived from Op
```

## Decision 1: the single surface, dispatched by operation kind

```rust
impl Runtime {
    /// The entire data surface. Reads return the cache projection; writes
    /// apply optimistically and return the optimistic projection. Never
    /// touches the network on the caller's thread.
    pub fn execute<Op: Operation>(&self, vars: Op::Variables) -> Result<Op::Output>;
}
```

- **Query `Op`** — `execute` returns `Query::query(cache, vars)`. Cache-first,
  instant, no network, exactly today's `load`. `execute::<IssuesQuery>(vars)`
  returns the `IssueConnection` from the replica.
- **Mutation `Op`** — `execute` runs the operation's optimistic local write and
  op-log enqueue (atomically), then returns the **optimistic `Op::Output` read
  back from the cache** — the created or updated entity as it now renders.
  `IssueCreateMutation::Output = Issue` (`issues.rs:567`), so
  `execute::<IssueCreateMutation>(vars)` returns the optimistic `Issue`,
  retiring `create_issue`'s `-> String` temp-id hack (`runtime.rs:767-772`; the
  caller wanted an identifier, and the whole entity carries it).
  `IssueUpdateMutation::Output = Option<Issue>` (`issues.rs:530`) returns the
  updated issue.

The network is never on the synchronous path in either case — a read is cache, a
write is the optimistic cache write. Upstream is reconciled behind the surface
(Decision 3).

`refresh` as a public verb disappears: it was "pull upstream, then the caller
reads." Under `execute`, the pull is internal freshness (Decision 3) and the
read is `execute` itself.

Rejected alternatives:

| Option                                                       | Why rejected                                                                                                      |
| ------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------- |
| Keep `load`/`refresh`/`subscribe` + add `execute` for writes | the surface stays wide and still leaks `Subscription`; ENG-67 asks for _one_ verb                                 |
| `execute` returns `()` for writes                            | discards the optimistic entity the create path needs (its new identifier)                                         |
| `execute` returns the wire response for writes               | not available synchronously — the mutation drains later; the synchronous truth is the optimistic cache projection |

## Decision 2: three seam traits — `Query`, `Fill`, and `Mutation`

The local seam is three operation-kind traits, named for what they are
(`crates/lt-runtime/src/ops.rs`):

```rust
// lt-runtime — the local-cache seams (crates/lt-runtime/src/ops.rs).
pub trait Query: GraphqlOperation {
    /// Read the operation's result out of the local replica.
    fn query(conn: &Connection, vars: &Self::Variables) -> Result<Self::Output>;
}

pub trait Fill: GraphqlOperation {
    /// Write an already-fetched response into the cache — the read path's
    /// fetch-and-fill. Every query-kind operation implements this.
    fn fill(conn: &Connection, vars: &Self::Variables, out: &Self::Output) -> Result<()>;
}

pub trait Mutation: GraphqlOperation {
    /// The op-log write side: the optimistic in-place local write plus its
    /// op-log enqueue, the replay-variables rebuild, and the drain ack. The
    /// op-log stores no variables — `replay_vars` re-reads the row.
    fn enqueue(conn: &Connection, vars: Self::Variables) -> Result<String>;
    fn replay_vars(conn: &Connection, id: &str) -> Result<Self::Variables>;
    fn ack(conn: &Connection, ctx: AckContext<'_>, out: Self::Output) -> Result<()>;
}
```

- `Query` replaces `Read` (and drops the `reads` predicate — Decision 3 removes
  scoped invalidation, so nothing consumes it).
- `Fill` replaces `Upsert`: writing a fetched query response into the cache is
  the read path's own concern, not a mutation. **No query operation implements
  `Mutation`** — the two never overlap on one operation
  (`crates/lt-runtime/src/ops.rs:37-39`).
- `Mutation` replaces `Mutate` and is the op-log seam: only the three real
  mutations (`IssueCreateMutation`, `IssueUpdateMutation`,
  `CommentCreateMutation`) implement it, each supplying
  `enqueue`/`replay_vars`/`ack` (`crates/lt-runtime/src/ops.rs:53-67`).

The upstream-freshness path is the **`Refresh` trait**
(`crates/lt-runtime/src/ops.rs:347`). Most operations get a blanket single-page
`refresh` — `fill ∘ client::execute::<Op>`
(`crates/lt-upstream/src/client.rs:74`) — while a composed operation whose
refresh spans multiple wire requests supplies its own impl: `IssueDetailQuery`
paginates its comment thread to exhaustion
(`crates/lt-runtime/src/ops.rs:395-436`).

`execute<Op: Operation>` dispatches through a thin `Operation` supertrait
implemented per operation — a query op wires to `Query`, a mutation op to the
optimistic-write path over `Mutation`. `Operation` is **not a third seam**; the
local seams are exactly `Query` and `Mutation`. It is dispatch glue, and it
earns its place under two hard constraints:

- **Crate layering.** `GraphqlOperation` is the wire contract in `lt-types` (the
  leaf vocabulary crate); it cannot reference the cache or `Runtime` without
  inverting the workspace's dependency direction. "Apply this operation to the
  Runtime's cache" is an `lt-runtime` concept, so it lives on an `lt-runtime`
  trait, not on `GraphqlOperation`.
- **Coherence.** One `execute<Op>` must compile to a read for a query and a
  write for a mutation. `Query` and `Mutation` are disjoint, and Rust allows
  neither two same-named methods with disjoint bounds nor overlapping blanket
  impls (no negative bounds). A single unifying bound is the only way to keep
  one method.

The per-op impl is three lines choosing the seam — an ENG-16 codegen target (the
fragment's `graphql_type` says which kind), so its marginal cost is ~nil
([[schema-codegen-program-adr.md]]). It exists only to serve the
single-`execute` surface; drop that requirement and `Operation` goes with it.

Rejected alternatives:

| Option                                                  | Why rejected                                                                                                                                 |
| ------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| Keep the opaque `Read`/`Mutate` names                   | renamed for what they are: `Query` (cache read), `Fill` (fetched-response write), `Mutation` (op-log write)                                  |
| Fold the fetched-response write into `Mutation`         | a query's cache-fill is not an optimistic user write and never enqueues an op-log row; `Fill` keeps the read and write paths disjoint        |
| Fetch through a write seam instead of a `Refresh` trait | fetching needs `lt-upstream`, which `lt-storage` does not depend on; the on-open refresh is the `Refresh` trait in `lt-runtime` (Decision 3) |

## Decision 3: one unscoped `Update` refreshes every active view

There is no scoped invalidation. `EntityKey` is deleted. On any change to the
local cache, the runtime emits a single, payload-free `RuntimeEvent::Update`;
the App re-executes every active view. A view's data stays current by
re-reading, not by receiving.

```text
  view render:  data = runtime.execute::<Op>(vars)      (cache read, instant)
  any change:   runtime emits RuntimeEvent::Update       (one signal, unscoped)
  App::apply:   re-execute EVERY active view → re-render
```

- **The typed `Subscription<T>` slot is removed** (`subscription.rs` deleted),
  and `RuntimeEvent::Updated(SubscriptionKey)`
  (`crates/lt-runtime/src/sync/service.rs:9-18`) becomes `RuntimeEvent::Update`
  with no payload. This is the payload-free-wake philosophy
  [[tui-app-event-queue-adr.md]] and [[operation-seam-adr.md]] Decision 4
  established, taken to its floor: the signal says "re-read," and every active
  view does.
- **No `reads`, no per-op dependency map, no subscription registry.** A handful
  of active views re-reading the cache on a change is cheap; the entity-keyed
  fan-out ENG-28 built was precision the app does not need. Correctness is
  last-write-wins of current truth: a redundant re-read is idempotent.
- **Freshness** is likewise simple: a view's operation is refreshed upstream
  while it is active (the delta cycle for issues; the composed view's `Refresh`
  impl when it opens, filled via `Fill`). The on-open refresh skips an issue
  whose optimistic create has not yet synced (`synced_at IS NULL`): its
  fabricated id has no upstream counterpart, so the fetch would be doomed
  (`crates/lt-runtime/src/ops.rs:409-415`). Finer scheduling is deferred, not
  designed here.

Rejected alternatives:

| Option                                         | Why rejected                                                                             |
| ---------------------------------------------- | ---------------------------------------------------------------------------------------- |
| Keep entity-keyed invalidation (ENG-28 Dec. 5) | premature precision; "start simple" — one unscoped signal, re-read all active views      |
| Keep the typed `Subscription<T>` slot          | public subscription surface; ENG-67 hides subscription behind `execute`                  |
| Push `Op::Output` payloads on the event queue  | the per-operation event enum / `Box<dyn Any>` [[operation-seam-adr.md]] already rejected |

## Decision 4: the manual drive stays a control verb

ENG-67 wants "the 'now' part" — driving the op-log drain immediately — as a
separate API. It is not a data operation, so it does not belong on `execute`; it
is a lifecycle control like `run`/`login`/`request_sync`. Today's `drain_now`
(`runtime.rs:535`) becomes the public control verb:

```rust
pub fn drain(&self) -> Result<()>;   // replay the op-log upstream now
```

`execute` on a mutation still nudges the loop to drain promptly (a latency
optimization — the periodic delta cycle drains anyway); `drain` is the
caller-driven, loop-free equivalent the CLI and tests use. The op-log replay
dispatch (the op's `operation` name → concrete mutation,
`crates/lt-runtime/src/sync/drain.rs:40-45`) is a `NAME`-keyed match — the one
runtime-string→type boundary no single generic call spans — each arm replaying
an operation and, on success, signalling `Update`. Hand-written now, generated
by ENG-16 ([[schema-codegen-program-adr.md]]).

## Decision 5: `GraphqlOperation` sheds `extract`; `Op::Output` is the full payload

`GraphqlOperation::extract(self) -> Result<Op::Output>`
(`crates/lt-types/src/graphql.rs:19`) bundles four jobs, and the two that carry
weight do not belong on the wire trait:

| Job                   | Example                                                     | Verdict                                                            |
| --------------------- | ----------------------------------------------------------- | ------------------------------------------------------------------ |
| Trivial projection    | `Ok(self.issues)` (`issues.rs:507`)                         | mechanical; no weight                                              |
| **Lossy narrowing**   | `Ok(self.teams.nodes) -> Vec<Team>` (`teams.rs:26`)         | discards the connection and its `pageInfo`; a `Vec` can't paginate |
| Fallible success-gate | `ensure_success`/`extract_on_success` (`graphql.rs:23-38`)  | real, but a _mutation-result_ concern, not universal               |
| Domain recomposition  | `ViewerEnvelope -> Viewer` (`viewer.rs`); `IssueDetailData` | real wire→domain transform                                         |

The narrowing majority is dead weight, and job 2 is a defect:
[[operation-seam-adr.md]] deliberately kept `Output = IssueConnection` to retain
`pageInfo` for uniform pagination, and `extract` violates that wherever it
returns a `Vec`. So `extract` is removed from `GraphqlOperation`:

- **`Op::Output` is the full-fidelity decoded payload** — connections keep their
  `pageInfo`, nothing is narrowed. `client::execute::<Op>` decodes straight into
  it, and a consumer that wants only `.nodes` projects at the call site.
- **The success-gate moves to the write seam.** It is about a mutation's result;
  the drain/replay path and `Mutation` already own that, and gating there
  propagates the error rather than burying it in a universal method.
- **Recomposition becomes `From`/`TryFrom`** (rust.md: prefer `impl From` over a
  bespoke method), or the cynic fragment selects into the domain shape directly.

This tightens the surface: `execute::<Op>(vars) -> Op::Output` returns the same
full payload whether it came from the wire or the cache.

## The resulting `Runtime` surface

```text
  data:      execute::<Op>(vars) -> Op::Output          (the entire data API)
  control:   new, run, drain, request_sync, login, last_synced_at, seed_sim
```

No entity name, no `Subscription`, no `EntityKey` in any public signature. The
acceptance check is mechanical:

```sh
rg 'pub fn (create|update|delete|load|refresh|subscribe)' crates/lt-runtime/src/runtime.rs  # → empty
```

## Non-goals

- **The read model.** Reads are plain SELECTs over the collapsed fragment type;
  composed-op cursor pagination is unchanged. ENG-67 is a surface refactor over
  that model, not a change to it.
- **Uncached operations.** `execute` requires a `Query` or a `Mutation` impl; an
  operation with neither (e.g. `NotificationsQuery`, no cache table) is out of
  scope until its table exists — consistent with [[operation-seam-adr.md]]'s
  Notifications non-goal.
- **Generating the `Operation`/registry.** Hand-written here; ENG-16 emits them.

## Relationship to ENG-16 and ENG-63

- **ENG-16** ([[schema-codegen-program-adr.md]]) generates the `Operation` impl
  (Decision 2) and the replay registry (Decision 4) from the fragments. ENG-67
  makes the surface generic by hand so ENG-16 has a stable target — the read
  seam (ENG-28) preceded ENG-16 the same way.
- **ENG-63** (generic `Table<'a, T>`/`Form<'a, T>`): once every view renders
  `execute::<Op>(vars) -> Op::Output` and re-executes on `Update`, the view
  widgets can be generic over `Op::Output`. Out of scope here.

## Test migration

- The three write-wrapper tests and the `load`/`refresh`/`subscribe` tests
  (`runtime.rs`, `ops.rs`) rewrite to `execute::<Op>`; mutation tests assert the
  returned optimistic `Op::Output` (a create returns its `Issue`).
- Loop/view tests (`crates/lt-tui/src/loop_tests.rs`) drop the `Subscription`
  slot and assert that any change re-executes every active view; the real
  `Runtime` over `FakeTransport`, no `run()`, stays the harness
  ([[operation-seam-adr.md]] Decision 7).
- `drain_now` tests carry over under `drain`; a replay-registry completeness
  test is added (every shipped mutation resolves).

## Tasks

1. `Query`/`Fill`/`Mutation` traits (`Upsert` becomes `Fill`); the `Operation`
   dispatch trait; `Runtime::execute`; remove `GraphqlOperation::extract`
   (`Op::Output` = full payload, gate → write seam, recomposition → `From`);
   port the read callers off `load`/`refresh` — verify: the `rg` surface check
   is empty for reads.
2. Mutation path through `execute` returning the optimistic `Op::Output`; delete
   the three write wrappers and the `-> String` hack — verify: create returns
   the optimistic `Issue`; loop tests pass rewritten.
3. Delete `EntityKey`; collapse `RuntimeEvent::Updated(SubscriptionKey)` to a
   payload-free `Update`; remove `Subscription<T>`; App re-executes all active
   views on `Update` — verify: `lt-tui` holds no `Subscription`; a write
   re-renders every active view.
4. Replay `NAME`-registry firing `Update` + the `drain` control verb — verify:
   `unknown_op_type` test passes; drain tests carry over.

Ordering: 1 → 2 → 4 are independent; 3 lands after 1–2 (views execute before the
slot can be removed).

## Open questions

None.
