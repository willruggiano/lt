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
{ /* serialize vars → query → decode into Op::Output via Op::extract */ }
```

Each operation is statically a query or a mutation: `IssuesQuery` selects
`#[cynic(graphql_type = "Query")]` (`crates/lt-types/src/issues.rs:491`),
`IssueCreateMutation` selects `"Mutation"` (`issues.rs:559`). The _kind_ is a
property of the type, and it is the only classification the local seam needs.

The intent of ENG-67 is to expose one verb over all of it:

```rust
pub fn execute<Op>(&self, vars: Op::Variables) -> Result<Op::Output>;
```

Everything else — optimistic writes, the outbox, live refresh and invalidation,
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

   Subscription<T>, the outbox, scoped invalidation  ── all internal, derived from Op
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
  outbox enqueue (atomically), then returns the **optimistic `Op::Output` read
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

## Decision 2: exactly two seam traits — `Query` and `Mutation`

The local seam is exactly two operation-kind traits, named for what they are:

```rust
// lt-storage — the two local-cache seams.
pub trait Query: GraphqlOperation {
    /// Read the operation's result out of the local replica.
    fn query(conn: &Connection, vars: &Self::Variables) -> Result<Self::Output>;
}

pub trait Mutation: GraphqlOperation {
    /// Any write into the local replica: the optimistic user write plus its
    /// outbox command, and (subsuming the old `Upsert`) the application of a
    /// fetched upstream response. Every local write is a mutation.
    fn apply(conn: &Connection, vars: &Self::Variables, /* … */) -> Result<()>;
}
```

- `Query` replaces `Read` (and drops the `reads` predicate — Decision 3 removes
  scoped invalidation, so nothing consumes it).
- `Mutation` replaces `Mutate` and **subsumes `Upsert`**: applying a fetched
  query response into the cache and applying an optimistic user edit are both
  writes to the local replica, so they are one seam, not two.

There is **no `Refresh` trait and no `Upsert` trait**. Pulling an operation from
upstream is already generic —
`client::execute::<Op>(transport, vars) -> Op::Output`
(`crates/lt-upstream/src/client.rs:74`) — so the runtime's internal freshness
path fetches with that and writes the result through `Mutation::apply`. A
per-operation fetch trait would add nothing over the generic transport call.

`execute<Op: Operation>` dispatches through a thin `Operation` supertrait
implemented per operation — a query op wires to `Query`, a mutation op to the
optimistic-write path over `Mutation`. Blanket impls can't decide
query-vs-mutation (Rust has no negative bounds), so the per-op impl is written
by hand and later generated by ENG-16 from the fragment's `graphql_type`
([[schema-codegen-program-adr.md]]).

Rejected alternatives:

| Option                                             | Why rejected                                                                                                                           |
| -------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| Keep `Read`/`Upsert`/`Mutate`/`Refresh` (4 traits) | `Read`/`Mutate` are opaque, `Upsert` is just a local write (fold into `Mutation`), and `Refresh` is just the generic `client::execute` |
| A distinct `Refresh`/fetch trait per operation     | fetching is already `client::execute::<Op>`; the only local seams are read (`Query`) and write (`Mutation`)                            |

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
  while it is active (the delta cycle for issues; a background
  `client::execute::<Op>` applied via `Mutation` for a composed view when it
  opens). Finer scheduling is deferred, not designed here.

Rejected alternatives:

| Option                                         | Why rejected                                                                             |
| ---------------------------------------------- | ---------------------------------------------------------------------------------------- |
| Keep entity-keyed invalidation (ENG-28 Dec. 5) | premature precision; "start simple" — one unscoped signal, re-read all active views      |
| Keep the typed `Subscription<T>` slot          | public subscription surface; ENG-67 hides subscription behind `execute`                  |
| Push `Op::Output` payloads on the event queue  | the per-operation event enum / `Box<dyn Any>` [[operation-seam-adr.md]] already rejected |

## Decision 4: the manual drive stays a control verb

ENG-67 wants "the 'now' part" — driving the outbox drain immediately — as a
separate API. It is not a data operation, so it does not belong on `execute`; it
is a lifecycle control like `run`/`login`/`request_sync`. Today's `drain_now`
(`runtime.rs:535`) becomes the public control verb:

```rust
pub fn drain(&self) -> Result<()>;   // replay the outbox upstream now
```

`execute` on a mutation still nudges the loop to drain promptly (a latency
optimization — the periodic delta cycle drains anyway); `drain` is the
caller-driven, loop-free equivalent the CLI and tests use. The outbox replay
dispatch (`op_type` string → concrete mutation,
`crates/lt-runtime/src/sync/drain.rs:40-46`) becomes a `NAME`-keyed registry —
the one runtime-string→type boundary no single generic call spans — each entry
replaying an operation and, on success, firing `Update`. Hand-written now,
generated by ENG-16 ([[schema-codegen-program-adr.md]]).

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

- **The read model.** Overlay merge, coalescing, temp-id rewrite, composed-op
  cursor pagination are unchanged; ENG-67 is a surface refactor.
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

1. `Query`/`Mutation` rename (folding `Upsert` into `Mutation`); the `Operation`
   dispatch trait; `Runtime::execute`; port the read callers off
   `load`/`refresh` — verify: the `rg` surface check is empty for reads.
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
