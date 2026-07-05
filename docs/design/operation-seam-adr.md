# Views, Layouts, and the Operation Seam (ENG-28)

## Status

Accepted. Delivery has not started; the Task decomposition is at the end.

## Context

The local cache is a replica of the Linear API, but the codebase treats the
two sides with different vocabularies. Upstream already has the seam this
design generalizes: `GraphqlOperation` (`crates/lt-types/src/graphql.rs:11`)
binds each operation type to its variables, its cynic-built wire query, and
its extraction, and one generic driver runs every operation —
`execute::<Op>(transport, variables)` (`crates/lt-upstream/src/client.rs:74`).
The local side has no equivalent. It is a flat set of per-entity functions
(`query_issues`, `crates/lt-storage/src/db/issues.rs:285`; `query_teams`,
`query_comments`, …), two parallel filter-to-SQL builders for the same issues
query (`filters::build_sql_filter`, `crates/lt-storage/src/db/filters.rs:31`;
`search_query::build_conditions`), per-entity sync procedures
(`teams::sync_teams`, `teams::sync_team_data`, `comments::sync`), and a
hand-rolled freshness vocabulary: the `Scope` enum
(`crates/lt-runtime/src/sync/service.rs:67`) routed through a `match` in
`LinearSyncService::refresh_body` (`crates/lt-runtime/src/adapter.rs:284-292`)
and hand-mapped onto `StateEvent` (`adapter.rs:276-281`).

The TUI consumes the local side directly. After ENG-42, each view owns its
data, query, and scroll (`ListView`/`ListQuery`, `crates/lt-tui/src/list.rs`),
but it loads that data by calling database functions on a raw connection at
eleven sites (`list.rs:67,90`, `detail.rs:30,40,81,119,126`,
`popup.rs:239,281,297`, `new_issue.rs:102,159,166,213`, `lib.rs:759`), reached
through `lt-runtime`'s re-export of `lt_storage::db`
(`crates/lt-runtime/src/lib.rs:23`). Rendering is split between view modules
(state and behavior) and `ui/*` (free render functions), and the boundary
leaks: the renderer writes layout state onto a view (`popup.anchor`,
`crates/lt-tui/src/ui/mod.rs:131`), hoists base-table geometry across match
arms for the popup and search arms (`ui/mod.rs:108-154`), and `HelpPopup`
caches column widths on view state (`crates/lt-tui/src/popup.rs:116-118`).

[[tui-app-event-queue-adr.md]] deferred the layout-component split as
direction; ENG-27 (the detail-view redesign) is its consumer. ENG-16 will
generate operation types and CLI commands from the GraphQL schema; every
hand-written artifact in this design is one of its codegen targets.

The thesis: the operation type becomes the sole vocabulary of the local side
too. Every read, every refresh, and every view's data contract is an
operation plus its variables.

```text
      one operation type per view (lt-types; ENG-16 generates these later)
  IssuesQuery{filter,sort,first,after}   IssueDetailQuery{id}   NewIssueQuery{team_id?}
        │
        ├─ execute::<Op>(transport, vars) ─► Linear GraphQL      (exists, unchanged)
        ├─ impl Read for Op   ─────────────► SQL over the replica  (new, lt-storage)
        └─ impl Upsert for Op ─────────────► Output → cache tables (new, lt-storage)

  lt-runtime, three generic drivers — the entire data API:
        load::<Op>(vars)      = read                     one-shot local read
        refresh::<Op>(vars)   = upsert ∘ execute         upstream → cache
        subscribe::<Op>(vars) = read + register          live view data
```

## Decision 1: `Read` and `Upsert` over `GraphqlOperation`

```rust
// lt-storage — impls live beside the statement registry; SQL text stays
// crate-private per [[type-safe-sql-adr.md]].
pub trait Read: GraphqlOperation {
    fn read(conn: &Connection, vars: &Self::Variables) -> Result<Self::Output>;
    /// Does this operation's result depend on `key`? Derivable from the
    /// document's fragment set (an ENG-16 codegen target); hand-written
    /// until then. Over-approximation is safe: a spurious re-read is an
    /// idempotent projection of current truth.
    fn reads(vars: &Self::Variables, key: &EntityKey) -> bool;
}

pub trait Upsert: GraphqlOperation {
    /// Write the response into the cache and report every node slice
    /// touched (Decision 5).
    fn upsert(conn: &Connection, out: &Self::Output) -> Result<Vec<EntityKey>>;
}
```

"Cannot diverge" is structural: the same `Variables` in and the same `Output`
out, on the same type the wire uses. The sync engine becomes an instance of
the design rather than a sibling: delta sync is
`refresh::<IssuesQuery>({filter: updated_after, ..})`, full sync is the same
with no filter, paginated through the existing page helper (`sync_pages`,
`crates/lt-runtime/src/sync/mod.rs:27`). `sync/full.rs` and `sync/delta.rs`
become variables presets, not procedures. `teams::sync_teams`,
`teams::sync_team_data`, and `comments::sync`
(`crates/lt-runtime/src/{teams,comments}.rs`) dissolve into `Upsert` impls
invoked by the generic driver.

Rejected alternatives:

| Option                                          | Why rejected                                                                                                     |
| ----------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| A separate `Query` trait as the local anchor    | duplicates what `GraphqlOperation` already carries (variables, output); two vocabularies for one operation      |
| Keep per-entity read/sync functions as the API  | the `fetch_issues` vs `fetch_teams` shape ENG-28 exists to end; every new entity adds a hand-wired method family |
| `Read` impls in `lt-runtime`                    | the SQL registry (`Sql`, `Frag`, `ComposedSql`) is deliberately crate-private per [[type-safe-sql-adr.md]]       |

## Decision 2: typed variables

A local executor cannot lower `IssueFilterValue(serde_json::Value)`
(`crates/lt-types/src/issues.rs:22-38`) to SQL, so the filter becomes a typed
`IssueFilter` — the allowlisted subset the build already validates against
the schema ([[architecture.md#Search and the codegen seam]]). Hand-written
now; generated by ENG-16 later.

- Wire: `build_filter` (`crates/lt-upstream/src/issues.rs:19`) becomes the
  typed filter's wire serialization; `build_sort` likewise. The
  `IssueFilterValue`/`IssueSortValue` wrappers die.
- SQL: `filters::build_sql_filter` and `search_query::build_conditions` —
  today's two parallel lowerings — merge into one `IssueFilter → Vec<Frag>`
  lowering; a text term selects the FTS join.
- Producers: `IssueArgs` (CLI) and the search `QueryAst` both lower into
  `IssuesQuery` variables; `resolve_me` runs at lowering time. `IssueQuery`
  and `ParsedQuery` as parallel specs die.
- Pagination: `first`/`after` are variables. The local read interprets
  `after` as an offset cursor — today's stringified-offset workaround
  (`crates/lt-tui/src/list.rs:84-86`) promoted to defined semantics.
  `Output = IssueConnection` on both sides, so `has_next_page`/`end_cursor`
  are uniform and the filtered-search path gains the pagination it lacks
  (`list.rs:70-71`).

Rejected alternatives:

| Option                                             | Why rejected                                                                                       |
| -------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| An enum over `{IssueQuery, ParsedQuery}`           | a false split: the page and search cases are the same operation with different arguments           |
| Keep JSON filter blobs, interpret them locally     | stringly-typed dispatch in the SQL lowering; the schema validation the build performs goes unused  |

## Decision 3: one operation per view; composed documents

Each view declares exactly one operation. Multi-entity views compose the
document, not the client — GraphQL's native composition replaces per-entity
client joins:

| View            | Operation                              | Output                              |
| --------------- | -------------------------------------- | ----------------------------------- |
| List            | `IssuesQuery{filter, sort, first, after}` | `IssueConnection`                |
| Detail          | `IssueDetailQuery{id}`                 | `{issue, comments, children}`       |
| State picker    | `TeamStatesQuery{team_id}`             | `Vec<WorkflowStateWithPosition>`    |
| Assignee picker | `TeamMembersQuery{team_id}`            | `Vec<User>`                         |
| Priority picker | — (static items)                       | —                                   |
| New-issue modal | `NewIssueQuery{team_id: Option<..>}`   | `{teams, states, members, viewer}`  |
| Search overlay  | `IssuesQuery` via one-shot `load`      | `IssueConnection`                   |
| Help            | —                                      | —                                   |

A composed operation's `Read` joins its parts locally; its refresh re-fetches
everything the view shows. Refresh of a composed document may paginate nested
connections to exhaustion — multiple wire requests, one operation type — so
`IssueDetailQuery` keeps today's fetch-all comment semantics
(`crates/lt-upstream/src/comments.rs:16`). `IssueDetailQuery` is also
ENG-27's data contract.

Rejected alternatives:

| Option                                    | Why rejected                                                                                        |
| ----------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| Per-entity operations, N subscriptions per view | re-encodes the composed document in every consumer; "a view is a single query" stops being total |
| Cap nested connections at one page        | silently stops syncing long comment threads; today's semantics paginate to exhaustion                |

## Decision 4: subscriptions — typed slots, payload-free wakes

The TUI holds no `Database`, no `Connection`, and no `lt_runtime::db::*`
import; `StateCtx` dies. A view receives data; it never fetches.

```rust
// lt-runtime
pub struct Subscription<T> {
    id: SubId,
    latest: Arc<Mutex<Option<T>>>,
}
impl<T> Subscription<T> {
    /// Consume the latest result, if a newer one has arrived.
    pub fn take(&self) -> Option<T>;
}
impl<T> Drop for Subscription<T> { /* sends Unsubscribe(id) */ }

impl Runtime {
    /// Synchronous initial read (cache-first open, same frame), then live:
    /// after a cache change that the operation reads (Decision 5), the
    /// runtime re-runs the read, fills the slot, and emits a wake.
    pub fn subscribe<Op: Read + Upsert>(
        &self,
        vars: Op::Variables,
    ) -> (Subscription<Op::Output>, Op::Output);

    /// One-shot, no registration: the search overlay's debounced preview
    /// and the CLI's cached reads.
    pub fn load<Op: Read>(&self, vars: &Op::Variables) -> Result<Op::Output>;
}
```

- The event vocabulary shrinks to `RuntimeEvent::Updated(SubId)`; the
  `Sync`/`Login` events are unchanged. The channel stays payload-free —
  [[tui-app-event-queue-adr.md]]'s staleness argument concerned partial
  payloads; a whole-result slot overwritten on every change is
  last-write-wins of current truth. Data crosses in the typed slot, so no
  per-operation event enum and no type erasure on the queue.
- Internally, `subscribe` erases the operation into closures capturing the
  typed variables — re-read (fill slot, emit wake) and refresh
  (`upsert ∘ execute`) — registered with the loop through the existing
  command channel (`Subscribe`/`Unsubscribe(SubId)` replacing
  `Watch`/`Unwatch`). The loop re-reads promptly on registration, which also
  closes the window between the caller-side initial read and registration.
- `Drop` retracts: RAII replaces the push/pop watch bookkeeping and the
  new-issue modal's hand-diffed `watched_team_id`
  (`crates/lt-tui/src/new_issue.rs:256-287`) — a team change is a new
  variables value, so the view drops the old subscription and subscribes
  with the new one.
- Same-frame optimistic writes are preserved: the write methods
  (`edit_issue` et al., `crates/lt-runtime/src/adapter.rs:493-523`) keep
  their synchronous enqueue and end in an inline propagation pass instead of
  one hand-picked `StateEvent`; the loop's post-apply drain renders it in
  the same frame.

Rejected alternatives:

| Option                                   | Why rejected                                                                                              |
| ---------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `Op::Output` payloads on the shared queue | needs a per-operation event enum or `Box<dyn Any>` — the hand-rolled vocabulary this design deletes        |
| Views re-read through a `Store` facade   | the TUI keeps issuing reads; "work through the runtime" satisfied in name only                             |
| Fully async initial load                 | costs the instant cache-first open ([[tui-app-event-queue-adr.md]] Decision 3's cache-first property)       |

## Decision 5: entity-keyed invalidation

The cache is a normalized store — rows are nodes, and the cynic fragments in
`lt-types` are the fragment analog of Relay: every operation's document is
composed from shared entity fragments, so the same entity data lands in the
same tables regardless of which operation fetched it. Propagation follows the
nodes:

```rust
// lt-storage — variants mirror the cache tables plus the owning id where
// one exists. StateEvent's granularity ("table + owning id, not per row"),
// now produced mechanically instead of hand-placed.
pub enum EntityKey {
    Issue,
    Comment { issue_id: String },
    Teams,
    WorkflowStates { team_id: String },
    TeamMemberships { team_id: String },
    Viewer,
}
```

The writer reports what it touched: `Upsert::upsert` returns the touched set,
and the outbox enqueues return theirs (`enqueue_state_change` touches
`Issue`; `enqueue_comment_create` touches `Comment{issue_id}`). The runtime
intersects touched keys with each live subscription's `Read::reads` and
re-runs only the matches. No static write-to-read dependency map exists
anywhere: relevance is computed at write time by the one party that knows
what changed.

```text
  delta sync: refresh::<IssuesQuery> → upsert reports [Issue, Comment{42}]
                                             │
              runtime: for each live sub, Op::reads(vars, key)?
                                             │
    ├─ List(IssuesQuery{filter})     reads(Issue)        → re-read, slot, wake
    ├─ Detail(IssueDetailQuery{42})  reads(Comment{42})  → re-read, slot, wake
    ├─ Detail(IssueDetailQuery{7})   no touched key      → untouched
    └─ Picker(TeamMembersQuery{T})   no touched key      → untouched
```

Cross-operation effects work by construction: when an upsert writes a node
another view displays, it reports that node, and the other view's `reads`
matches — no operation needs to know which other operations can produce its
data. `reads` may over-approximate safely (a spurious re-read is idempotent),
so impls stay coarse where coarse is honest (`IssuesQuery` reads `Issue`, not
per-id) and precise where identity matters (`Comment{issue_id}`).

`EntityKey` is the storage↔runtime wire; it never crosses into `lt-tui`.

Rejected alternatives:

| Option                                        | Why rejected                                                                                                  |
| --------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| Notify all live subscriptions on any change   | correct but wasteful, and it discards the compile-time knowledge of what each operation reads                    |
| A static write→read dependency map            | the `refresh_body` match reborn; every new producer/consumer pair is a hand-maintained edge                      |
| Table-level dependency derivation             | a reactive framework; recorded as the escalation path if live-query fan-out ever outgrows the predicate approach |
| Per-row invalidation keys                     | speculative granularity; every consumer re-reads whole result sets anyway                                        |

## Decision 6: freshness derives from entity dependencies

The per-operation freshness constant this design draft once carried is
deleted as duplicative: an operation's `reads` already says what it depends
on. The loop owns exactly one piece of policy — **the baseline delta cycle
covers `EntityKey::Issue`**. On each tick, and promptly on registration, it
walks live subscriptions and upstream-refreshes each one whose `reads`
extends beyond that coverage: `IssueDetailQuery` (comments), the pickers
(states/members), `NewIssueQuery`. A pure-issues subscription is never
redundantly re-fetched — delta feeds it through the intersection. The
targeted fan-out stays bounded by what is on screen, the property the watch
set had (`adapter.rs:64`), now derived instead of declared.

The cadence, pause gate, login worker, and the thread-free `LoopState` policy
core (`adapter.rs:63-136`) are unchanged in shape; `Watch`/`Unwatch(Scope)`
commands become `Subscribe`/`Unsubscribe(SubId)`.

Rejected alternatives:

| Option                                  | Why rejected                                                                              |
| --------------------------------------- | -------------------------------------------------------------------------------------------- |
| A per-operation freshness constant      | duplicates what `reads` declares; two sources of truth for one fact                          |
| Upstream-refresh every live subscription | re-fetches filtered issue pages the delta cycle already covers; API chatter with no payoff |

## Decision 7: a concrete `Runtime`; the `dyn SyncService` seam retires

`subscribe<Op>` is generic, so the `Arc<dyn SyncService>` seam cannot carry
it. The TUI holds a concrete `Runtime` (today's `LinearSyncService`, renamed:
it is now the whole data runtime, not a sync scheduler). The dyn trait
existed for the recording test fake; the replacement is stronger — tests
construct `Runtime` over an in-memory `Database` with `FakeTransport`
(`crates/lt-upstream/src/client.rs:91`) and never start `run()`. Initial
reads and write propagation are synchronous, so loop tests stay thread-free
and exercise the real service.

Transports are currently built per call (`Self::transport()`,
`adapter.rs:214-217`; `viewer_identity`, `adapter.rs:204`) and inside
`sync::{full,delta}::run` (`adapter.rs:230-236`). The generic `refresh`
threads `(conn, transport, vars)` explicitly, so construction moves to one
transport source injected at `Runtime::new(db, transports, on_event)`. Login
is a browser OAuth flow (`login_non_interactive`, `adapter.rs:298`), not a
`GraphqlTransport` concern; the login worker is untouched.

Rejected alternatives:

| Option                                             | Why rejected                                                                          |
| -------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| Split handles: dyn commands + concrete reads       | two injected objects where one suffices; the trait's only consumer was the test fake      |
| Keep a recording fake implementing a smaller trait | duplicates coverage the real service over `FakeTransport` provides against real behavior |

## Decision 8: Views — the enum stays; a view is one operation

The `View` enum (`crates/lt-tui/src/lib.rs:72-84`) and the stack, keymaps,
scroll motions, and the cascade floor are unchanged. What changes is the data
contract: a variant's struct is
`{ vars, data: Op::Output, sub: Subscription<Op::Output>, ui-state }`.

- `View::scopes()` (`lib.rs:117-136`) and the per-variant relevance guards in
  `consume` die. `App::apply` routes `Updated(id)` to the view holding that
  subscription, which `take`s and re-applies its ui-state policy: selection
  clamp, `pending_select`, and the list's focused-guard — which becomes
  "defer `take` while unfocused; the slot holds the latest for focus return"
  (replacing the drop at `crates/lt-tui/src/list.rs:220-224`).
- `ListQuery`'s two-branch fetch (`list.rs:56-108`) collapses into the one
  `IssuesQuery` subscription; pagination mutates `vars.after` and
  re-subscribes.
- No `View` trait: the enum's dispatch methods are exhaustive and
  compile-checked, and the polymorphism that matters lives in the operation
  traits. Introducing a trait object here would trade that for erasure with
  a single consumer, the flexibility [[posture.md]] forbids.

## Decision 9: Layouts — `Widget` impls and explicit geometry

A Layout is a `Widget` impl. Per ratatui 0.30's documented recommendation —
implement `Widget` on references; that supersedes `WidgetRef` and needs no
unstable feature (ratatui 0.30.2, `src/widgets.rs`, "Evolution and Current
Recommendations"; [widget docs](https://docs.rs/ratatui/0.30.2/ratatui/widgets/index.html))
— the `ui/*` free functions become `impl Widget for &FooView`, or
`StatefulWidget`/`&mut` where render legitimately mutates `TableState`. The
boundary leaks close structurally:

- The renderer writing `popup.anchor` onto view state (`ui/mod.rs:131`,
  `popup.rs:69-70`) dies: the base table's layout yields an explicit
  `TableGeometry { widths, selected_row }` that the popup widget takes as a
  render input. Cross-view geometry becomes a parameter, never stored.
- `HelpPopup`'s cached column widths (`popup.rs:116-118`) are computed in
  render.
- `viewport_height` stays app-threaded: scroll math runs before render, and
  fighting that buys nothing.
- Per-entity presentation: the orphan rule bars `impl From<&Issue> for Row`
  (both types foreign to `lt-tui`), and a ratatui dependency in `lt-types`
  points the wrong way. Thin local wrappers, one module per entity —
  `IssueRow<'a>(&'a Issue)`, `IssueDetail<'a>`, … with `Widget` impls — get
  the same co-location: presentation logic lives with the entity it renders,
  and `render_detail`-style functions dissolve into them. This is
  [[tui-app-event-queue-adr.md]]'s deferred layout-components direction, now
  with its consumer (ENG-27).

Rejected alternatives:

| Option                                  | Why rejected                                                                          |
| --------------------------------------- | ----------------------------------------------------------------------------------------- |
| `WidgetRef` behind `unstable-widget-ref` | only needed for `Box<dyn WidgetRef>` collections; `impl Widget for &T` covers the stack |
| Presentation impls in `lt-types`        | a TUI dependency in the vocabulary crate; every consumer pays for ratatui                 |
| ratatui's `Component` trait pattern     | duplicates the enum's dispatch with dynamic dispatch; the event loop already exists       |

## Amendments to the AppEvent queue design

This design supersedes parts of [[tui-app-event-queue-adr.md]]:

- `StateEvent` and `Scope` retire. The invalidation vocabulary is derived:
  `EntityKey` between storage and runtime (produced by upserts), `SubId` on
  the queue. The propagation rule itself — writes land in SQLite, a
  payload-free signal names what changed, displayed state re-reads — is
  unchanged.
- Its rejection of a "runtime subscription registry" is reversed with cause:
  the watch set already was that registry; subscriptions collapse the two
  parallel encodings into one and views keep their consume policy.
- Its rejection of "relay-proper: dependencies derived from queries" is
  narrowed: full table-level dependency tracking stays rejected; the
  operation-declared `reads` predicate plus writer-reported touched sets is
  the affordable middle it lacked a vocabulary for.
- Decision 3 (declarative interest) is subsumed by subscription RAII.
  Decision 6's `StateCtx` dies. The scope-relevance table's scenarios reduce
  to: `take` yields latest-or-nothing (duplicates and late wakes are
  idempotent); a popped view's subscription is dropped and its slot
  unreachable; the focused-guard defers `take` instead of dropping.
- Its open question on startup's blocking `fetch_viewer` closes for free:
  the header subscribes to `ViewerQuery`, whose `Read` is the persisted
  `synced_viewer` — instant from cache, updated by the first sync's
  propagation.

## Non-goals

- **Notifications.** `lt inbox` fetches upstream directly with no cache
  (`crates/lt-cli/src/inbox/mod.rs:21`); `NotificationsQuery` gets no
  `Read`/`Upsert` until a notifications table exists. Until then the "every
  read is `load`/`subscribe`" claim holds for the TUI and the cached CLI
  paths only.
- **Mutations.** The outbox path already maps 1:1 onto mutation operation
  types (`sync/drain.rs` replays `OP_*` commands through
  `execute::<Mutation>`); "execute the mutation against the replica" is the
  overlay+outbox write, and drain is its upstream execution. Systematizing
  that binding is ENG-16's.
- **CLI argument generation.** `IssueArgs → IssuesQuery` variables stays a
  hand-written `From` until ENG-16 derives clap from variables.

## User-visible behavior changes

1. Filtered/search results paginate like the plain list (they cannot today).
2. The header identity populates instantly from the cache at startup instead
   of blocking on a viewer fetch.
3. Detail panes and pickers update on any sync that touches their entities,
   including changes produced by other views' operations; relevance misses
   (a stale pane after an edit elsewhere) are structurally gone.
4. Comment threads keep fetch-all semantics; no truncation is introduced by
   the composed detail operation.

## Test migration

- Loop and view tests drive the real `Runtime` over an in-memory `Database`
  and `FakeTransport`, without starting `run()`; assertions are behavioral
  (drive a write or a synced fixture, assert the view updated or did not).
  The recording fake and its trait die.
- `LoopState` policy tests (`adapter.rs:526-627`) carry over with the
  command rename.
- `Read` impls get the coverage `query_*` functions have today, driven by
  seeded `sim` datasets per [[dst.md]]; `reads`/touched-set intersection
  gets table-driven tests in `lt-runtime`.
- Render snapshots are unchanged by Tasks 1–4 and re-accepted only where
  Task 5 intentionally changes layout.

## Tasks

1. Typed `IssueFilter`; variables retyping; one SQL lowering — verify: both
   former filter paths' tests pass through the merged builder.
2. `Read`/`Upsert` + `load`/`refresh` drivers; `full`/`delta` as
   `refresh::<IssuesQuery>` presets; CLI cached path via `load` — verify:
   sim-backed CLI tests unchanged.
3. `Runtime` with subscriptions, `EntityKey` propagation, and
   `Updated(SubId)`; transport injection; `Scope`/`StateEvent`/dyn trait
   deleted; TUI consumes via `take` — verify: loop tests run the real
   service over `FakeTransport`.
4. Composed per-view operations; the remaining TUI database call sites
   deleted — verify: `lt-tui` contains no `lt_runtime::db` import.
5. Layout migration: `Widget` impls, `TableGeometry` parameter, per-entity
   presentation modules — verify: snapshots unchanged where layout is
   unchanged; ENG-27 builds on the result.

Ordering: 1 → 2 → 3 → 4; 5 is independent after 3.

## Open questions

None.
