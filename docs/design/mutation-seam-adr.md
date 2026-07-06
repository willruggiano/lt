# The Mutation Seam: a generic `execute` (ENG-67)

## Status

Proposed. Design target for ENG-67. Builds directly on the operation seam
([[operation-seam-adr.md]]), which made the read side generic and left the write
side as its explicit non-goal ("Mutations").

## Context

After [[operation-seam-adr.md]], the read side of `Runtime` is fully generic
over the operation type: `subscribe::<Op>`, `load::<Op>`, and the background
`refresh` are parameterized over `Read + Upsert + Refresh`, and no entity name
appears in their signatures (`crates/lt-runtime/src/runtime.rs:334-418`). The
write side did not follow. `Runtime` exposes three entity-specific public
methods:

```rust
// crates/lt-runtime/src/runtime.rs:754-772
pub fn create_comment(&self, input: &CommentCreateInput) -> Result<()>;
pub fn update_issue(&self, vars: IssueUpdateVariables) -> Result<()>;
pub fn create_issue(&self, input: &IssueCreateInput) -> Result<String>;
```

This is the asymmetry ENG-67 targets: the read path is a single generic verb;
the write path is a hand-written method per mutation.

Crucially, the _internal_ write plumbing is already generic. All three methods
are thin wrappers over one generic tail:

```rust
// crates/lt-runtime/src/runtime.rs:742-750
fn enqueue_and_propagate<M: Mutate>(&self, vars: M::Variables) -> Result<Vec<EntityKey>> {
    let conn = self.connect()?;
    let touched = M::enqueue(&conn, vars)?;   // optimistic local effect + outbox row
    self.propagate(&touched);                 // same propagation the read side uses
    self.commands_tx.send(Command::Drain);    // nudge the loop to drain now
    Ok(touched)
}
```

`Mutate` (`crates/lt-storage/src/db/ops.rs:59-73`) already mirrors
`Read`/`Upsert` on the write side: `enqueue` writes the optimistic overlay and
the outbox command atomically, `ack` reconciles the base once the drainer has
the wire response. The mutation's own `GraphqlOperation::NAME` is the outbox
`op_type` discriminator, so no parallel constant exists.

```text
   READ side (generic — done)              WRITE side (ENG-67)
   ───────────────────────────            ───────────────────────────
   Runtime::load::<Op>                     create_comment()  ┐  3 hand-written
   Runtime::subscribe::<Op>                update_issue()    ├─ public wrappers
   (internally: Read/Upsert/Refresh)       create_issue()    ┘  runtime.rs:754
                                                   │
                                                   └─ enqueue_and_propagate::<M: Mutate>  ← already generic
                                                          │
   drain: replay match on op_type.as_str() ────────────────┘   drain.rs:40-46
          (the one runtime-string → static-type dispatch that is NOT generic)
```

One dispatch resists a single generic call. The outbox drainer replays a _stored
string_ `op_type` back into a concrete mutation type:

```rust
// crates/lt-runtime/src/sync/drain.rs:40-46
match op.op_type.as_str() {
    IssueUpdateMutation::NAME  => replay_op::<IssueUpdateMutation>(conn, transport, op),
    IssueCreateMutation::NAME  => replay_op::<IssueCreateMutation>(conn, transport, op),
    CommentCreateMutation::NAME => replay_op::<CommentCreateMutation>(conn, transport, op),
    other => bail!("unknown outbox op_type: {other}"),
}
```

This is a runtime value → static type boundary: a persisted outbox row names its
operation by string, and the drainer must recover the type to call
`execute::<M>` and `M::ack`. No single generic function spans it; something must
map `NAME → replay`.

The thesis: the write path is already generic internally. ENG-67 finishes the
job by (1) promoting the generic tail to the sole public write verb, (2)
replacing the hand-maintained `op_type` match with a mutation registry keyed by
`NAME`, and (3) naming the manual upstream-drive API explicitly. After this,
`Runtime` carries zero entity-specific methods.

## Decision 1: `Runtime::execute<M: Mutate>` — the sole write verb

Delete `create_issue`, `update_issue`, `create_comment`. Promote the generic
tail to public:

```rust
impl Runtime {
    /// Apply a mutation against the local replica: write its optimistic
    /// effect and outbox command (`M::enqueue`), propagate to live
    /// subscriptions, and prompt the loop to drain. Returns the touched keys.
    pub fn execute<M: Mutate>(&self, vars: M::Variables) -> Result<Vec<EntityKey>>;
}
```

Callers pass typed variables directly, matching ENG-67's target shape:

```rust
let input = IssueCreateInput { .. };
runtime.execute::<IssueCreateMutation>(IssueCreateVariables { input })?;
```

The one wrinkle is `create_issue`'s `-> Result<String>` return: the caller
(`crates/lt-tui/src/new_issue.rs:281-287`) uses the returned identifier for
`list.pending_select`, to seek to the new row. That identifier is not derived
state — it is the constant `OPTIMISTIC_ISSUE_IDENTIFIER` (`"NEW"`,
`crates/lt-storage/src/db/outbox.rs:31`), the sentinel every optimistic create
carries until the drainer's ack rewrites it. Since it is a constant, the caller
references `outbox::OPTIMISTIC_ISSUE_IDENTIFIER` directly and `execute` returns
the uniform `Vec<EntityKey>`. The bespoke return type dissolves; it was never
carrying per-call information.

### Why `execute` is the write verb, not a query/mutation super-verb

ENG-67's prose asks for "a single public api, `execute`, that is generic over
the query/mutation type." Taken literally, one verb would span both reads and
writes. It should not, and the issue's own examples already split them: the read
example uses `refresh` + `load`, the write example uses `execute`.

The reason is the local-first invariant ([[architecture.md]]): a read is
cache-first and never touches the network (`load`/`subscribe`), while a mutation
is optimistic-enqueue-then-async-drain. These are different lifecycles with
different failure modes. Folding them into one `execute<Op>` would either make
reads hit the network (destroying the cache-first property
[[operation-seam-adr.md]] rests on) or overload `execute` with a branch on
operation kind. `execute` is the mutation verb; reads keep `load`/`subscribe`,
and `refresh` stays the internal upstream-pull driver.

Rejected alternatives:

| Option                                                         | Why rejected                                                                                                         |
| -------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| One `execute<Op>` over both reads and writes                   | reads are cache-first and writes are optimistic+outbox+drain; one verb either networks reads or branches on op kind  |
| Keep `create_issue -> String`, add generic `execute` alongside | leaves an entity-specific method; the return value is a constant, not per-call state                                 |
| `execute` returns `M::Output`                                  | the output is not available synchronously — the mutation drains later; the synchronous result is the touched key set |

## Decision 2: a mutation registry replaces the `op_type` match

The drainer's `NAME → replay` dispatch (`drain.rs:40-46`) becomes a registry: a
table from `op_type` string to the monomorphized replay function that today's
`replay_op::<M>` already is.

```rust
// crates/lt-runtime/src/sync/drain.rs
type ReplayFn = fn(&Connection, &dyn GraphqlTransport, &PendingOp) -> Result<Vec<EntityKey>>;

/// Every mutation the outbox can replay, keyed by its wire NAME. Hand-written
/// now; an ENG-16 codegen target (docs/design/operation-seam-adr.md, Non-goals).
const REPLAY_REGISTRY: &[(&str, ReplayFn)] = &[
    (IssueUpdateMutation::NAME,  replay_op::<IssueUpdateMutation>),
    (IssueCreateMutation::NAME,  replay_op::<IssueCreateMutation>),
    (CommentCreateMutation::NAME, replay_op::<CommentCreateMutation>),
];

fn replay(conn: &Connection, transport: &dyn GraphqlTransport, op: &PendingOp) -> Result<Vec<EntityKey>> {
    let entry = REPLAY_REGISTRY.iter().find(|(name, _)| *name == op.op_type);
    match entry {
        Some((_, replay_fn)) => replay_fn(conn, transport, op),
        None => bail!("unknown outbox op_type: {}", op.op_type),
    }
}
```

The dispatch behavior is identical (an unknown `op_type` still errors and is
recorded per row, `drain.rs:29`), but the _set of replayable mutations_ is now
data, not control flow — the shape ENG-16 generates. Adding a mutation becomes
one registry row instead of a new match arm.

Rejected alternatives:

| Option                                                   | Why rejected                                                                                                                                                                                        |
| -------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Keep the `match op_type.as_str()`                        | a hand-maintained arm per mutation; not the data shape ENG-16 emits                                                                                                                                 |
| `inventory`/`linkme` distributed-slice auto-registration | link-time magic and a proc-macro/registration dependency for three entries; the explicit slice is greppable and is exactly what codegen will produce ([[posture.md]]: explicit wiring over globals) |
| Store the typed variables' TypeId in the outbox          | the outbox persists across restarts; a process-local `TypeId` is not a stable discriminator, unlike the wire `NAME` already stored                                                                  |

## Decision 3: the manual-drive API is explicit and separate

ENG-67 wants "the 'now' part" — manually driving the runtime to drain pending
mutations — as a distinct API. It already exists internally as `drain_now`
(`crates/lt-runtime/src/runtime.rs:535-541`): connect, acquire a transport,
replay the whole outbox, propagate. Promote it to the public manual-drive verb
(`flush`), and keep the two concerns cleanly split:

```text
  execute::<M>(vars)   local + optimistic: enqueue, propagate, return.
                       Nudges the loop (Command::Drain) so the drain happens
                       promptly, but correctness never depends on it — the
                       periodic delta cycle drains anyway (drain.rs runs before
                       every fetch).
  flush() -> touched   explicit synchronous drive: replay the outbox now and
                       report what it touched. The CLI and tests use this to
                       drive the runtime without the background loop.
```

`execute` keeps the `Command::Drain` nudge (it is a latency optimization for the
TUI, not a correctness requirement), and `flush` is the caller-driven, loop-free
equivalent. Loop-free drive is what makes the runtime testable without starting
`run()` — the property [[operation-seam-adr.md]] Decision 7 established for
reads, now named for writes.

Rejected alternatives:

| Option                                                                     | Why rejected                                                                                                                                                 |
| -------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `execute` drains synchronously                                             | reintroduces a network round-trip on the write path; the TUI's optimistic write must return in-frame ([[tui-app-event-queue-adr.md]])                        |
| Drop the `Command::Drain` nudge from `execute`, require callers to `flush` | every TUI edit would wait up to one delta cycle to reach the server, or every call site must remember to `flush`; the nudge is free and preserves current UX |

## Decision 4: zero entity-specific methods — the acceptance check

After Decisions 1–3, `Runtime`'s public surface is: `new`, `run`, `subscribe`,
`load`, `execute`, `flush`, `request_sync`, `login`, `sync_full`, `sync_delta`,
`last_synced_at`, `seed_sim`. None names an entity. The check is mechanical:

```sh
rg 'pub fn (create|update|delete)_\w+' crates/lt-runtime/src/runtime.rs   # → no matches
```

`sync_full`/`sync_delta` remain: they are `refresh::<IssuesQuery>` presets plus
reference-data orchestration (viewer, teams, states before issue pages,
[[architecture.md#Sync]]), not single operations, so they are not `execute`
calls in disguise. They carry no entity-specific _input_ — the acceptance bar is
"no per-mutation method," which they clear.

## Relationship to ENG-16 and ENG-63

- **ENG-16** generates the operation types and, per this ADR, the
  `REPLAY_REGISTRY` — the last hand-maintained `NAME → replay` table.
  [[schema-codegen-program-adr.md]] enumerates it as a codegen target. ENG-67
  makes the write seam generic _by hand_ so ENG-16 has a stable target, exactly
  as [[operation-seam-adr.md]] (ENG-28) preceded ENG-16 on the read side.
- **ENG-63** (generic `Table<'a, T>`/`Form<'a, T>`) is the TUI-side counterpart:
  once every view's data is an `Op::Output` and every write is `execute::<M>`,
  the view widgets can be generic over the operation too. Out of scope here;
  ENG-67 stops at the `Runtime` boundary.

## Non-goals

- **Generating the registry.** The `REPLAY_REGISTRY` slice is hand-written;
  ENG-16 emits it. This ADR only converts the match into the data shape codegen
  will fill.
- **Read-side changes.** `load`/`subscribe`/`refresh` are untouched.
- **`Mutate` semantics.** The overlay write, command coalescing
  (`replace_pending`, `outbox.rs:87`), temp-id rewrite, and ack reconciliation
  are unchanged; ENG-67 is a `Runtime`-surface refactor, not a write-model one.

## Test migration

- The three wrapper tests in `runtime.rs`
  (`create_issue_propagates_to_a_live_issues_subscription`,
  `create_comment_propagates_to_a_live_issue_detail_subscription`,
  `update_issue_refreshes_an_open_detail_pane_for_a_different_issue`,
  `runtime.rs:969-1088`) rewrite to `execute::<M>` calls; the assertions
  (propagation, optimistic overlay) are unchanged.
- `drain.rs`'s `unknown_op_type_is_recorded_as_an_error` (drain.rs:297) carries
  over verbatim. Add a registry-completeness test: every `Mutate` impl the crate
  ships has a `REPLAY_REGISTRY` row (a table-driven assertion over the known
  mutation NAMEs).
- TUI callers (`new_issue.rs:281`, `detail.rs:195`, `popup.rs:419`) swap to
  `execute::<M>` plus `OPTIMISTIC_ISSUE_IDENTIFIER`; loop tests
  (`crates/lt-tui/src/loop_tests.rs`) update the same way.
- `drain_now` tests (`runtime.rs:1320-1409`) carry over under the `flush` name.

## Tasks

1. Promote `enqueue_and_propagate` to `Runtime::execute<M: Mutate>`; delete the
   three wrappers; update TUI call sites to `execute::<M>` +
   `OPTIMISTIC_ISSUE_IDENTIFIER` — verify: the `rg` check in Decision 4 is
   empty; loop and runtime write tests pass rewritten.
2. Convert `drain::replay`'s match into `REPLAY_REGISTRY` + lookup; add the
   completeness test — verify: `unknown_op_type` test passes; every shipped
   mutation resolves.
3. Rename `drain_now` to `flush` as the public manual-drive verb; confirm
   `execute`'s nudge is retained — verify: `flush` tests carry over; CLI/loop
   drive paths unchanged.

Ordering: 1 → 2 → 3; each is independent and independently shippable.

## Open questions

None. The one interpretive fork — whether `execute` spans queries too (Decision

1. — is resolved against unification, on the local-first invariant and the
   issue's own split read/write examples. If a future need to run a query
   "through `execute`" appears, it is `refresh` (upstream pull), already
   generic, not a new verb.
