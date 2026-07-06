# One `execute` for Reads and Writes (ENG-67)

## Status

Proposed. Design target for ENG-67. Collapses the runtime's data surface to a
single generic verb and moves everything else behind it. This supersedes the
write-path non-goal of [[operation-seam-adr.md]] and **revises its Decision 4
(typed subscription slots) and Decisions 5–6 (registry-based propagation and
freshness)**: subscription and invalidation stop being a public surface and
become internals derived from the operation type.

## Context

After [[operation-seam-adr.md]], the read side is generic but split across three
verbs (`load`, `subscribe`, `refresh`), and the write side is three
entity-specific methods (`create_issue`, `update_issue`, `create_comment`,
`crates/lt-runtime/src/runtime.rs:754-772`). The generality is real but the
surface is wide, and it leaks implementation: `subscribe` hands the TUI a typed
`Subscription<T>` slot (`crates/lt-runtime/src/subscription.rs`), `load` returns
`Op::Output` but `refresh` returns `Vec<EntityKey>`, and the write methods each
name an entity.

The machinery to unify is already in place. The transport runs _any_ operation
generically and returns its typed output:

```rust
// crates/lt-upstream/src/client.rs:74-84
pub fn execute<Op: GraphqlOperation>(transport: &dyn GraphqlTransport, variables: Op::Variables)
    -> Result<Op::Output>
{ /* serialize vars → query → decode into Op::Output via Op::extract */ }
```

Each operation already carries its local seam: `Read`/`Upsert` for queries,
`Mutate` for the outbox write path, `Refresh` for upstream pull — all keyed off
the same `Op` type (`crates/lt-storage/src/db/ops.rs`,
`crates/lt-runtime/src/ops.rs`). And each operation is statically a query or a
mutation: `IssuesQuery` selects `#[cynic(graphql_type = "Query")]`
(`crates/lt-types/src/issues.rs:491`), `IssueCreateMutation` selects
`"Mutation"` (`issues.rs:559`). The _kind_ is a property of the type.

The intent of ENG-67 is to expose one verb over all of it:

```rust
pub fn execute<Op>(&self, vars: Op::Variables) -> Result<Op::Output>;
```

Everything else — optimistic writes, the outbox, `EntityKey` propagation, live
subscription and invalidation, the SQL — is an implementation detail, hidden
behind that surface and derived from `Op`.

```text
   TODAY                                     ENG-67
   ─────────────────────────                ─────────────────────────
   load::<Op>(vars)      -> Op::Output       ┐
   refresh::<Op>(vars)   -> Vec<EntityKey>   │
   subscribe::<Op>(vars) -> (Sub<T>, Output) ├──►  execute::<Op>(vars) -> Op::Output
   create_issue(input)   -> String           │     (query → cache read;
   update_issue(vars)    -> ()               │      mutation → optimistic apply,
   create_comment(input) -> ()               ┘      read the optimistic Output back)

   Subscription<T>, EntityKey, outbox, Vec<EntityKey>  ── all internal, derived from Op
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

- **Query `Op`** — `execute` returns `Read::read(cache, vars)`. Cache-first,
  instant, no network, exactly today's `load`. `execute::<IssuesQuery>(vars)`
  returns the `IssueConnection` from the replica.
- **Mutation `Op`** — `execute` runs `Mutate::enqueue` (the optimistic overlay
  and the outbox command, atomically), propagates, prompts the drain, and
  returns the **optimistic `Op::Output` read back from the cache** — the created
  or updated entity as it now renders. `IssueCreateMutation::Output = Issue`
  (`issues.rs:567`), so `execute::<IssueCreateMutation>(vars)` returns the
  optimistic `Issue`, retiring `create_issue`'s `-> String` temp-id hack
  (`runtime.rs:767-772`; the caller wanted an identifier, and the whole entity
  carries it). `IssueUpdateMutation::Output = Option<Issue>` (`issues.rs:530`)
  returns the updated issue.

The network is never on the synchronous path in either case — a read is cache, a
write is the optimistic cache write. Upstream is reconciled behind the surface
(Decision 3).

`refresh` as a public verb disappears: it was "pull upstream, then the caller
reads." Under `execute`, the pull is internal freshness (Decision 3) and the
read is `execute` itself.

Rejected alternatives:

| Option                                                       | Why rejected                                                                                               |
| ------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------- |
| Keep `load`/`refresh`/`subscribe` + add `execute` for writes | the surface stays wide and still leaks `Subscription`/`EntityKey`; ENG-67 asks for _one_ verb              |
| `execute` returns `Vec<EntityKey>` (touched set) for writes  | `EntityKey` is the internal invalidation currency; the caller wants the entity it just wrote, `Op::Output` |
| `execute` returns `()` for writes                            | discards the optimistic entity the create path needs (its new identifier)                                  |

## Decision 2: the `Operation` trait is the dispatch — and a codegen target

A single generic function cannot branch on query-vs-mutation through two
different trait bounds. One unifying trait carries the per-kind behavior:

```rust
// lt-runtime — the one thing execute dispatches through.
pub trait Operation: GraphqlOperation {
    fn execute(rt: &Runtime, vars: Self::Variables) -> Result<Self::Output>;
}
```

- A query op's impl = `Read::read` + register internal freshness interest
  (Decision 3).
- A mutation op's impl = `Mutate::enqueue` → propagate → drain nudge → read the
  optimistic `Op::Output` back.

Blanket impls (`impl<Q: Read> Operation`, `impl<M: Mutate> Operation`) conflict:
Rust has no negative bounds to prove a type is a query xor a mutation. So the
`Operation` impl is written **per operation** — three lines dispatching to the
right seam — which makes it precisely an ENG-16 codegen target: the fragment's
`graphql_type` already says which kind it is, so the impl is derivable
([[schema-codegen-program-adr.md]]). Hand-written until then.

`Read`/`Upsert`/`Mutate`/`Refresh` remain the derived building blocks;
`Operation` composes them into the one verb. The public bound is `Op: Operation`
— "any operation," the spirit of ENG-67's `Op: GraphqlOperation`.

## Decision 3: subscription and invalidation are internal, derived from `Op`

With no public `subscribe`, live views re-`execute` on a payload-free wake:

```text
  view render:   data = runtime.execute::<Op>(vars)          (cache read, instant)
  cache change:  runtime emits RuntimeEvent::Updated          (payload-free, existing queue)
  App::apply:    re-execute each visible view whose Op is affected → re-render
```

- **The typed `Subscription<T>` slot is removed** (`subscription.rs` deleted).
  Data no longer crosses in a slot; the view re-reads through `execute`. This is
  the same payload-free-wake philosophy [[tui-app-event-queue-adr.md]] and
  [[operation-seam-adr.md]] Decision 4 established, with the typed slot — the
  one piece of public subscription surface — removed: the wake says "re-read,"
  the view re-executes.
- **Freshness is derived, not registered.** A query `execute` whose `Op::reads`
  extend beyond the delta cycle's baseline (`EntityKey::Issue`) schedules a
  one-shot background upstream `Refresh` for that op — the same
  "beyond-baseline" policy the loop applies today (`runtime.rs:201-203`,
  `operation-seam-adr.md`, Decision 6), now triggered by the execute itself
  rather than by a durable watch entry. No `Subscription` registry to maintain,
  no RAII retract: a view that stops rendering stops executing, and its interest
  simply stops being refreshed.
- **`EntityKey`/`reads` become an internal optimization.** They survive only to
  let the runtime wake the right views instead of all visible ones; they never
  appear in the public API. The App re-executes a view when a wake's touched
  keys intersect that view's `Op::reads` — or, as the simplest floor,
  re-executes all visible views on any wake (a handful of cache reads).

Rejected alternatives:

| Option                                          | Why rejected                                                                                                                                    |
| ----------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| Keep the typed `Subscription<T>` slot           | it is public subscription surface; ENG-67 puts subscription behind `execute`                                                                    |
| A durable interest registry with an RAII handle | the handle is a second public type; "execute is the entire surface"                                                                             |
| Push `Op::Output` payloads on the event queue   | the per-operation event enum / `Box<dyn Any>` [[operation-seam-adr.md]] already rejected; the slot's removal doubles down on payload-free wakes |

## Decision 4: the manual drive stays a control verb, not a data verb

ENG-67 wants "the 'now' part" — driving the outbox drain immediately — as a
separate API. It is not a data operation, so it does not belong on `execute`; it
is a lifecycle control like `run`/`login`/`request_sync`. Today's `drain_now`
(`runtime.rs:535`) becomes the public control verb:

```rust
pub fn drain(&self) -> Result<()>;   // replay the outbox upstream now
```

`execute` on a mutation still nudges the loop to drain promptly (a latency
optimization, not correctness — the periodic delta cycle drains anyway); `drain`
is the caller-driven, loop-free equivalent the CLI and tests use. The outbox
replay dispatch (`op_type` string → concrete `M`,
`crates/lt-runtime/src/sync/drain.rs:40-46`) becomes a `NAME`-keyed registry —
the one runtime-string→type boundary no single generic call spans — hand-written
now, generated by ENG-16 ([[schema-codegen-program-adr.md]]).

## The resulting `Runtime` surface

```text
  data:      execute::<Op>(vars) -> Op::Output          (the entire data API)
  control:   new, run, drain, request_sync, login, last_synced_at, seed_sim
```

No entity name, no `Subscription`, no `EntityKey`, no `Vec<EntityKey>` in any
public signature. The acceptance check is mechanical:

```sh
rg 'pub fn (create|update|delete|load|refresh|subscribe)' crates/lt-runtime/src/runtime.rs  # → empty
```

## Non-goals

- **The read model.** Overlay merge, coalescing, temp-id rewrite, composed-op
  cursor pagination are unchanged; ENG-67 is a surface refactor.
- **Uncached operations.** `execute` requires a `Read` (queries) or `Mutate`
  (mutations); an operation with neither (e.g. `NotificationsQuery`, no cache
  table) is out of scope until its table exists — consistent with
  [[operation-seam-adr.md]]'s Notifications non-goal.
- **Generating the `Operation`/registry.** Hand-written here; ENG-16 emits them.

## Relationship to ENG-16 and ENG-63

- **ENG-16** ([[schema-codegen-program-adr.md]]) generates the `Operation` impl
  (Decision 2) and the replay registry (Decision 4) from the fragments. ENG-67
  makes the surface generic by hand so ENG-16 has a stable target — the read
  seam (ENG-28) preceded ENG-16 the same way.
- **ENG-63** (generic `Table<'a, T>`/`Form<'a, T>`): once every view renders
  `execute::<Op>(vars) -> Op::Output` and re-executes on wake, the view widgets
  can be generic over `Op::Output`. Out of scope here.

## Test migration

- The three write-wrapper tests and the `load`/`refresh`/`subscribe` tests
  (`runtime.rs`, `ops.rs`) rewrite to `execute::<Op>`; mutation tests assert the
  returned optimistic `Op::Output` (a create returns its `Issue`).
- Loop/view tests (`crates/lt-tui/src/loop_tests.rs`) drop the `Subscription`
  slot and assert re-execution on `Updated`; the real `Runtime` over
  `FakeTransport`, no `run()`, stays the harness ([[operation-seam-adr.md]]
  Decision 7).
- `drain_now` tests carry over under `drain`; the replay-registry completeness
  test is added (every shipped mutation resolves).

## Tasks

1. `Operation` trait + `Runtime::execute`; port the read callers off
   `load`/`refresh` — verify: the `rg` surface check is empty for reads.
2. Mutation path through `execute` returning the optimistic `Op::Output`; delete
   the three write wrappers and the `-> String` hack — verify: create returns
   the optimistic `Issue`; loop tests pass rewritten.
3. Remove public `Subscription<T>`; App re-executes visible views on `Updated`;
   internalize `EntityKey` as wake-filtering — verify: `lt-tui` holds no
   `Subscription`; a write re-renders the affected views.
4. Replay `NAME`-registry + `drain` control verb — verify: `unknown_op_type`
   test passes; drain tests carry over.

Ordering: 1 → 2 → 4 are independent; 3 lands after 1–2 (views execute before the
slot can be removed).

## Open questions

1. **Wake granularity** (Decision 3): retain internal `EntityKey` precision to
   wake only affected views, or re-execute all visible views on any wake. This
   revises just-shipped ENG-28; recommend retaining `EntityKey` as a pure
   internal optimization (no public change) and confirming the simplification is
   wanted before deleting the subscription registry.
2. **`Operation` vs the split traits**: whether `Operation` is the sole seam or
   a composition over `Read`/`Upsert`/`Mutate`/`Refresh`. Recommend composition
   — they stay the derived, independently-testable building blocks.
