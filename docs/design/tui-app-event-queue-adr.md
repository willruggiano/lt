# TUI AppEvent Queue and State Propagation (ENG-32)

## Status

Accepted. Delivery items 1–7 are landed; items 8–11 (the second review round)
are open.

## Context

The first round of this design replaced the TUI's four per-job
`Option<mpsc::Receiver<T>>` pollers with one long-lived
`mpsc::channel<AppEvent>`, one propagation rule — **writes land in SQLite; a
payload-free invalidation names the scope that changed; the views that display
that scope re-read it** — and a **stack of live views** that makes routing
precise: a view exists iff it is displayed, so events route to what exists. Key
input joined the same queue, making the loop a single blocking wait. That design
is delivered (Delivery items 2–7) and is the codebase this document now
describes:

```text
  [input thread] ─────── Key ──────────┐
  [sync worker] ──── Lifecycle(Sync) ──┤  Sender<AppEvent> (cloned)
  [login worker] ─── Lifecycle(Login) ─┼───────────┐
  [state refreshers] ─── State(..) ────┘           v
                                            App.events_rx ── App::apply
  optimistic writers (same thread,                 |
  via route_state_event directly) ─────────────────┘
```

The second review round found the remaining architectural flaw: **the TUI is
still the producer**. It schedules sync (`App::start_sync` and the periodic
gate, `crates/lt-tui/src/lib.rs:948-958` and `:974-986`), it directs targeted
refreshes (`spawn_state_refresh` called from `open_detail`,
`open_state_popup`/`open_assignee_popup`, and the new-issue modal), it writes to
the database directly and routes its own invalidations (`submit_comment`,
`popup_confirm`), and `new_issue_submit` even bypasses `app.db` via `db_path()`
(`crates/lt-tui/src/new_issue.rs:281-285`). The `SyncService` seam exists, but
the TUI drives it imperatively, job by job.

This revision inverts that. The runtime produces every non-key event and owns
all sync scheduling behind a single blocking entry point; `lt-cli` does the
wiring and owns the background sync thread; the TUI is pure presentation — it
consumes events and re-fetches state accordingly, declares what it displays
(Decision 3), issues user-initiated commands (`r`, `L`), and writes through the
runtime (Decision 4). The propagation rule is unchanged; what changes is who may
produce.

```text
                 lt-cli (wiring)
  (tx, rx) = mpsc::channel::<AppEvent>()
  service  = LinearSyncService::new(db, on_event)
               where on_event = move |ev| tx.send(AppEvent::Runtime(ev))
  thread::spawn(move || service.run())          // detached, process lifetime
  lt_tui::run(args, service, tx, rx)            // tui spawns the input thread

  [input thread] ──────────── Key ─────────────┐
  [sync loop] ── Sync(..) + State(..) ─────────┼──> rx ── App::apply
  [login worker] ── Login(..) ─────────────────┤          |      |       |
  [write methods] ── State(..) (same frame) ───┘         Key   State  Sync/Login
                                                       cascade  stack  typestate
       ^                                               + floor  walk   transitions
       |                                                  |
       └── watch / unwatch / request_sync / login ────────┘
           create_comment / edit_issue / create_issue
```

### Prior art divergence

The tracking issue cites gitui's `queue.rs:86-193`. gitui's `Queue` is an
`Rc<RefCell<VecDeque<InternalEvent>>>` for same-thread component-to-component
messaging; its cross-thread async results travel on a separate channel drained
by `update_async`. lt's producers are the runtime's threads plus one input
thread, so the honest adaptation is the channel half of gitui's split, not the
`Rc<RefCell<VecDeque>>` half. Same-frame optimistic propagation also rides the
channel: the write methods emit through the runtime's callback and the loop's
post-apply drain applies the event before the next draw (Decision 4), so no
same-thread queue is needed. The view stack is gitu's shape (`Vec<Screen>`), not
gitui's.

## Decision 1: event taxonomy — the runtime produces, the TUI consumes

The runtime defines the full vocabulary of everything it tells its consumer; the
TUI's queue type shrinks to "a key, or something the runtime said":

```rust
// crates/lt-runtime/src/sync/service.rs
/// Everything the runtime reports, delivered through the `OnEvent` callback
/// the service is constructed with.
pub enum RuntimeEvent {
    /// The named slice of local state changed; re-read it if displayed.
    State(StateEvent),
    /// Sync-cycle progress and outcome — scheduling and error text, not an
    /// invalidation.
    Sync(SyncEvent),
    /// Login outcome: identity or error text.
    Login(LoginEvent),
}

/// A payload-free invalidation. Variants carry only the scope id a consumer
/// needs to decide relevance and which query to re-run. Moves here from
/// `lt-tui` unchanged: the producer owns the vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateEvent {
    /// The issues read model changed (a write, or a sync upsert).
    Issues,
    /// One issue's comment thread changed.
    Comments { issue_id: String },
    /// The team list changed.
    Teams,
    /// One team's workflow states and memberships changed.
    Team { team_id: String },
}

pub enum SyncEvent {
    /// A sync cycle began. New variant: the TUI can no longer infer
    /// "in flight" from its own spawn, so the producer announces it.
    Started,
    /// Sync succeeded; carries a freshly-fetched identity when the loop
    /// decided one was needed (Decision 2).
    Done(Option<viewer::User>),
    Error(String),
    NotAuthenticated,
}

pub enum LoginEvent {
    /// Login succeeded. `viewer` is not optional: either you log in as a
    /// user or you don't — a post-login identity-fetch failure is `Error`.
    Success { viewer: viewer::User },
    Error(String),
}

/// Invoked once per event, from the service's threads.
pub type OnEvent = Box<dyn Fn(RuntimeEvent) + Send + Sync + 'static>;
```

```rust
// crates/lt-tui/src/lib.rs
/// A message to the event loop. One channel, one drain.
pub enum AppEvent {
    /// A key press (`KeyEventKind::Press` only), raw from crossterm;
    /// normalized at apply time.
    Key(crossterm::event::KeyEvent),
    /// Anything the runtime reported.
    Runtime(RuntimeEvent),
}
```

- **`LifecycleEvent` dies** (`crates/lt-tui/src/lib.rs:96-99`), subsumed by
  `RuntimeEvent`: the sync/login wrapper existed only because the TUI mapped two
  per-job callbacks into its own taxonomy. `App::apply` routes
  `Runtime(State(..))` to `route_state_event`, `Runtime(Sync(..))` and
  `Runtime(Login(..))` to the typestate consumers (Decision 7).
- **`StateEvent` granularity is unchanged** — per scope (table + owning id), not
  per table and not per row; payload-free, so a late or duplicate event is an
  idempotent re-read of current truth.
- **Sync completion emits `State(Issues)` from the producer.** Round 1 unified
  "sync done ⇒ issues changed" inside the TUI's lifecycle consumer (a direct
  `route_state_event` call); that made the TUI derive an invalidation from a
  lifecycle outcome — consumer-side production. The loop now emits `Sync(Done)`
  and then `State(Issues)` itself. The round-1 objection to two ordered events
  is moot: both consumers are idempotent and the drain applies them in the same
  frame.
- `lt-cli` owns the channel ends: it wraps the `Sender` into the `OnEvent`
  callback at service construction and passes the sender (for the input thread)
  and receiver into `tui::run`. `App` keeps only `events_rx`; its `events_tx`
  field and every TUI-side producer clone die.

Rejected alternatives:

| Option                                         | Why rejected                                                                                                 |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| Payload-carrying events                        | payloads are what make staleness a problem; a payload-free event can only trigger a re-read of current truth |
| Events carry rowids / entity diffs             | speculative granularity; every consumer re-reads whole scopes anyway                                         |
| One `StateEvent::Any`                          | forces every view to re-read everything on every event; loses scope relevance                                |
| `StateEvent` stays in `lt-tui`, mapped per job | the producer would emit a type it cannot name; per-job callbacks are the round-1 shape this revision retires |
| TUI keeps deriving `Issues` from `Sync(Done)`  | consumer-side production: the TUI must know which lifecycle outcomes imply which scopes — the producer knows |
| Pass `Sender<AppEvent>` into the trait         | `lt-runtime` would depend on a TUI type; `OnEvent` over `RuntimeEvent` keeps the dependency pointing inward  |

## Decision 2: the sync service — one blocking `run`, loop-owned scheduling

The per-job spawn methods die. The service is a persistent background loop with
one public entry point, constructed with its database and event callback:

```rust
// crates/lt-runtime/src/sync/service.rs
pub trait SyncService: Send + Sync {
    /// The sync loop: blocks for the life of the process. `lt-cli` spawns it
    /// on a detached background thread before the TUI starts. Owns all
    /// scheduling: the startup sync, the 30s delta cadence, prompt and
    /// periodic refreshes of watched scopes, and full syncs on request.
    fn run(&self);

    /// Declare/retract interest in a scope's freshness (Decision 3).
    fn watch(&self, scope: Scope);
    fn unwatch(&self, scope: Scope);

    /// User-initiated commands — deliberate acts, distinct from data-driven
    /// scheduling: `request_sync` nudges the loop into an immediate full
    /// sync (the `r` key); `login` runs the OAuth flow (the `L` key).
    fn request_sync(&self);
    fn login(&self);

    /// Startup header identity (see Open questions).
    fn fetch_viewer(&self) -> Option<viewer::User>;

    /// Writes (Decision 4): transactional local enqueue, then the matching
    /// `State` event emitted through the callback.
    fn create_comment(&self, input: &CommentCreateInput) -> Result<()>;
    fn edit_issue(&self, issue_id: &str, edit: IssueEdit) -> Result<()>;
    /// Returns the optimistic identifier so the caller can seek to it.
    fn create_issue(&self, input: &IssueCreateInput) -> Result<String>;
}
```

`LinearSyncService` becomes a struct: `new(db: Database, on_event: OnEvent)`.
Explicit construction replaces the per-call `db_path()` resolution in
`sync_with` (`crates/lt-runtime/src/adapter.rs:36-39`), so tests can hand the
service the same in-memory database the app reads. The trait methods send on an
internal command channel; `run` owns the receiver, the watch set, and the tick
deadline:

```text
run() loop:
  block on commands_rx.recv_timeout(until next tick):
    Watch(s)    -> watched.add(s); refresh(s)          // prompt refresh
    Unwatch(s)  -> watched.remove(s)
    RequestSync -> cycle(full: true)
    Login       -> spawn the login worker (ignored while one is in flight)
    timeout     -> cycle(full: false); refresh each watched scope

  cycle(full): emit Sync(Started)
    catch_unwind(full ? sync::full::run : sync::delta::run):
      Ok      -> emit Sync(Done(viewer?)); emit State(Issues); tick += 30s
      Err/panic -> emit Sync(Error(brief)); tick += 30s
      no token  -> emit Sync(NotAuthenticated); pause the cadence

  refresh(scope): catch_unwind(the scope's sync helper)
    failures -> tracing; always emit State(scope)      // Decision 3
```

- **`catch_unwind` moves from per-spawn to per-iteration** (replacing
  `crates/lt-runtime/src/adapter.rs:87-104`): a panicking sync body surfaces as
  `Sync(Error)` and the loop and its cadence survive. Strictly better than the
  per-spawn guard, which protected the completion callback but had no loop to
  keep alive. Panics are denied in workspace code; dependencies can still panic.
- **`sync_comments`/`sync_teams`/`sync_team_data` leave the public trait** and
  become private helpers of the loop
  (`crates/lt-runtime/src/{comments,teams}.rs` are unchanged; only the adapter's
  public wrappers die). There is no public "sync issues" vs "sync teams" vs
  "sync users": the one public API is `run`, and what gets refreshed is decided
  by the loop from its cadence and watch set.
- **The cadence pauses on `NotAuthenticated` or a failed login** and resumes on
  a login success or `request_sync` — the same gate the TUI's
  `periodic_sync_due` implements today (`crates/lt-tui/src/lib.rs:974-986`),
  relocated to the scheduler that owns it. Watch-triggered refreshes still run
  while paused; they fail, trace, and emit their `State` event (the cache
  re-read is still correct).
- **Login runs on its own worker thread**, spawned by the loop: the OAuth flow
  blocks on a browser redirect for arbitrarily long, and inlining it would
  starve the cadence. On success the worker emits `Login(Success { viewer })`
  and nudges the loop into a delta sync — the follow-up sync moves out of the
  TUI's login consumer (`crates/lt-tui/src/lib.rs:1046-1055`) into the producer.
  A token exchange that succeeds but whose identity fetch fails emits
  `Login(Error)`.
- **Identity policy**: the loop fetches identity with a successful cycle until
  it has delivered one (at process start, and again after `NotAuthenticated`),
  replacing the TUI-threaded `fetch_identity` parameter. When the TUI's startup
  `fetch_viewer` already succeeded this costs one redundant viewer query on the
  first cycle — accepted; it doubles as a token check.
- `fetch_viewer` stays: startup header identity before the first `Done`.

Rejected alternatives:

| Option                                             | Why rejected                                                                                                   |
| -------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| Per-job spawn methods (`spawn_sync`/`spawn_login`) | the TUI becomes the scheduler; every new refresh adds a TUI policy site — the round-1 shape this revision ends |
| Public `sync_issues`/`sync_teams`/`sync_users`     | granular imperative API invites the TUI to direct sync; one `run` keeps scheduling where the schedule lives    |
| The TUI owns the sync thread                       | the CLI does the wiring; the TUI is presentation and should not manage runtime lifetimes                       |
| Async runtime for the loop                         | the workspace has none; one thread blocking on `recv_timeout` is the same shape as the TUI loop itself         |
| Trait generic over the event callback              | loses object safety; `Arc<dyn SyncService>` is how `lt-cli` injects it (`crates/lt-cli/src/main.rs:72-74`)     |

## Decision 3: declarative interest — watch/unwatch replace TUI-directed refreshes

`App::spawn_state_refresh` (`crates/lt-tui/src/lib.rs:930-943`) and every call
to it die. In their place the TUI declares what it displays, and the loop
decides when and how to refresh it:

```rust
// crates/lt-runtime/src/sync/service.rs
/// A freshness interest. `StateEvent` minus `Issues`: the issue list's
/// freshness is the loop's own baseline cadence, not an interest a view
/// declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    Comments { issue_id: String },
    Teams,
    Team { team_id: String },
}
```

```rust
// crates/lt-tui/src/lib.rs
impl View {
    /// The scopes this view displays, derived from its current state.
    fn scopes(&self) -> Vec<Scope> {
        match self {
            View::Detail(d) => vec![Scope::Comments {
                issue_id: d.issue.id.inner().to_string(),
            }],
            View::Popup(p) => p
                .team_id
                .iter()
                .map(|t| Scope::Team { team_id: t.clone() })
                .collect(),
            View::NewIssue(m) => {
                let mut scopes = vec![Scope::Teams];
                if let Some(team_id) = m.selected_team_id() {
                    scopes.push(Scope::Team { team_id });
                }
                scopes
            }
            View::List(_) | View::Search(_) | View::Help(_) => Vec::new(),
        }
    }
}
```

- **Push watches, pop unwatches.** A `push_view` helper watches `view.scopes()`
  before pushing; `pop_view` unwatches the popped view's scopes. Because
  `scopes()` derives from current view state, pop retracts exactly what the view
  declares at that moment. The one mid-life scope change — the new-issue modal's
  team switch (`crates/lt-tui/src/new_issue.rs:232-256`) — unwatches the old
  `Team { .. }` and watches the new one in the same handler.
- **This is declaration, not direction.** The TUI states what is on screen; the
  loop owns policy: refresh a scope promptly on `watch`, and include watched
  scopes in the periodic tick. The service keeps a counted set (two views may
  display the same scope; `unwatch` decrements).
- **A refresh always ends in its `State` event, even on failure** — "the refresh
  attempt finished; re-read whatever is cached." Failures are expected offline
  and go to `tracing`; the cache is kept. This carries over the round-1 property
  that clears the modal's `loading` flag deterministically and is why no
  per-fetch error variant exists (the global sync label already covers "not
  authenticated" / "sync error").
- **The round-1 rejection of syncing all team metadata in the loop stands**:
  states + members for every team is an N+1 fan-out whose only consumers are the
  pickers. Watching bounds the fan-out to displayed scopes — the loop refreshes
  what someone is looking at, plus the issues baseline.
- Cache-first opens are unchanged: `open_detail`, the popups, and the modal
  populate instantly from the database and let the watch-triggered refresh land
  as a `State` event (`DetailView::consume`, `PopupView::consume`,
  `NewIssueModal::consume` are untouched by this decision).

Rejected alternatives:

| Option                                       | Why rejected                                                                                          |
| -------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| TUI-triggered targeted syncs (round-1 shape) | imperative direction of the sync service from presentation code, one policy site per open/change path |
| Sync all team metadata in the periodic cycle | N+1 query fan-out for data only the pickers read (round-1 rejection, unchanged)                       |
| Loop introspects the TUI's view stack        | inverts the dependency; the runtime cannot (and must not) name views                                  |
| Watches carry a refresh interval or priority | speculative configurability; the loop's prompt-then-periodic policy covers the two real cases         |

## Decision 4: writes behind the runtime

Today's writers enqueue against the database from the TUI and route their own
invalidation as a direct function call (`submit_comment`,
`crates/lt-tui/src/detail.rs:279-305`; `popup_confirm` + `enqueue_edit`,
`crates/lt-tui/src/popup.rs:431-482`; `new_issue_submit`,
`crates/lt-tui/src/new_issue.rs:258-308`). That is production from the consumer,
and it leaves two event paths. Writes move behind the runtime:

```rust
// crates/lt-runtime/src/sync/service.rs
/// One issue-field edit, mirroring the outbox commands
/// (`lt-storage/src/db/outbox.rs`).
pub enum IssueEdit {
    State { id: String, name: String },
    Priority(u8),
    /// `(id, name)`; `None` clears the assignee.
    Assignee(Option<(String, String)>),
}
```

- **`create_comment`** performs today's transactional enqueue
  (`enqueue_comment_create` with a fresh `temp_id`) and emits
  `State(Comments { issue_id })`.
- **`edit_issue`** maps `IssueEdit` onto
  `enqueue_state_change`/`enqueue_priority_change`/`enqueue_assignee_change` and
  emits `State(Issues)`.
- **`create_issue`** builds the optimistic issue fragment, enqueues it with the
  typed input (`enqueue_issue_create`), emits `State(Issues)`, and returns the
  optimistic identifier. `build_create_request`
  (`crates/lt-tui/src/new_issue.rs:319-389`) moves into the runtime: the
  fragment is a database row, not presentation. Display names are resolved from
  the same lookup tables the pickers read (team, state, user), with the same
  name-keyed fallback the current code uses when no state id is known. The write
  model itself — overlay row plus outbox command, one transaction — is unchanged
  ([[architecture.md#TUI]]).
- **The TUI's direct `route_state_event` calls die.** The single event path is
  the queue; `route_state_event` survives only as the consumer of
  `AppEvent::Runtime(State(..))`. Same-frame optimistic feedback is preserved by
  the loop's existing drain: `run_app` applies every queued event after the
  blocking wait and before the next draw (`crates/lt-tui/src/lib.rs:1245-1253`),
  so a `State` event emitted during a key apply renders in that same frame.
- **`new_issue_submit`'s seek** (`do_fetch_and_select`) is replaced by
  `ListView.pending_select: Option<String>`: the submit handler stores the
  identifier `create_issue` returned; the next `Issues` re-read consumes it and
  seeks the selection. One read path — the event walk — instead of a
  writer-owned re-fetch beside it. This also deletes the submit path's direct
  `db_path()` write, the last read/write in `lt-tui` that bypassed `app.db`.
- Write failures surface: the service methods return `Result`, and the TUI
  reports errors in `footer_msg` (they are silently discarded today — Decision
  11).

Rejected alternatives:

| Option                                             | Why rejected                                                                                         |
| -------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| Writers keep enqueue + direct `route_state_event`  | two event paths; the TUI hand-writes database rows and produces its own invalidations                |
| Write methods return the event for the TUI to send | the consumer routing the producer's output by hand — the same conflation with an extra step          |
| TUI builds the optimistic fragment, passes it in   | a database row hand-assembled in the presentation layer; the runtime has the same data via its cache |
| Writer re-fetches and seeks directly after create  | a second read path outside the event walk (the round-1 shape); `pending_select` keeps one path       |

## Decision 5: the view stack — a view exists iff it is displayed

Delivered in item 2 and unchanged in substance: `App.views: Vec<View>` is the
live stack, bottom to top (`crates/lt-tui/src/lib.rs:575-579`); the base is
`views[0]` (today always the issue list; a future `lt tui --inbox` seeds a
different base); the top view is focused; a view's state lives in its variant
(`View::List/Detail/Popup/NewIssue/Search/Help`,
`crates/lt-tui/src/lib.rs:121-131`), so no mode tag exists to fall out of sync
and no "am I displayed?" check survives. `pop_view` is the single removal path;
popping the base resets it instead — the stack is never empty
(`crates/lt-tui/src/lib.rs:750-756`).

This round changes what the base view owns:

- **`ListView` owns its query.** `args: IssueQuery` and the active filter AST
  move from `App` (`crates/lt-tui/src/lib.rs:581,599`) into `ListView`: they are
  issue-list query inputs, not app state — a future non-list base has neither.
  The methods that touch only query + list state move with them
  (`sync_args_from_filter`, `replace_sort_in_filter`, `cycle_sort`,
  `toggle_desc`, pagination). `ListView` also gains `pending_select` (Decision
  4).
- **`App` keeps what is genuinely app-wide**: the launch seeds
  (`initial_args`/`initial_filter`, read by the double-esc reset, which now
  rebuilds `views[0]` from them), `last_esc_time`, `footer_msg`,
  `viewport_height`, `session`, the `sync`/`auth` typestates, and the wiring
  (`db`, `clock`, `service`, `events_rx`).
- Non-list readers reach the base's query through `base_list()` — the header
  filter context, the table's sort marker
  (`crates/lt-tui/src/ui/table.rs:12-13`), the search overlay's limit, and the
  modal's preset team. `None` (a future non-list base) degrades them to
  empty/default, which is correct.

The stack-not-slot rationale, the popup anchor rule
(`crates/lt-tui/src/ui/table.rs:47-65`), and the round-1 rejected alternatives
(mode tag + parallel `Option` fields; `Option<View>` plus a return-view field)
stand as delivered.

## Decision 6: routing — keys cascade onto a floor, state walks the stack

State routing is delivered and keeps its shape: `route_state_event` walks the
stack top-down; every live view consumes; the only checks are id-relevance and
the base's `focused` don't-clobber policy (`crates/lt-tui/src/lib.rs:912-923`,
`ListView::consume` at `:221-225`). Two things change: the context shrinks, and
the key cascade gains a floor.

### `StateCtx` diet

```rust
// crates/lt-tui/src/lib.rs
/// Read-only context a view's consume/re-query needs. Built inline from
/// disjoint `App` field borrows at each call site.
pub struct StateCtx<'a> {
    pub db: &'a lt_runtime::db::Database,
    pub viewer_name: Option<&'a str>,
}
```

The `args`/`filter` fields were the base list's query inputs riding in an
app-wide context — out of place for every other consumer. With `ListView` owning
its query (Decision 5), `do_fetch` reads `self.args`/`self.filter` and the ctx
carries only what any view might need: the database and the viewer name
(`assignee:me` resolution). Consumer signatures are otherwise unchanged;
`DetailView::consume` (`crates/lt-tui/src/detail.rs:28-47`) remains the
representative subscriber.

### The key cascade gets a floor and scroll defaults

Round 1 landed the cascade mechanism-only: every handler returns `Consumed`
unconditionally (`crates/lt-tui/src/lib.rs:344-350`), so nothing cascades and
`Esc`/`q`/scroll arms are duplicated per view. That stage ends:

```rust
// crates/lt-tui/src/lib.rs (shape)
fn dispatch_key(&mut self, key: KeyEvent) {
    // 1. The focused view's bindings.
    let top = self.views.len() - 1;
    if matches!(self.handle_view_key(top, key), KeyFlow::Consumed) {
        return;
    }
    // 2. Scroll defaults, resolved at the focused view only.
    if let Some(motion) = ScrollMotion::from_key(key) {
        let viewport = self.viewport_height;
        if let Some(view) = self.views.last_mut() {
            view.scroll(motion, viewport);
        }
        return;
    }
    // 3. Cascade: unconsumed keys fall toward the base.
    for i in (0..top).rev() {
        if matches!(self.handle_view_key(i, key), KeyFlow::Consumed) {
            return;
        }
    }
    // 4. The floor.
    match key.code {
        KeyCode::Esc if top > 0 => self.pop_view(),
        KeyCode::Esc => self.handle_list_esc(), // double-esc reset, unchanged
        KeyCode::Char('q') if top > 0 => self.pop_view(),
        KeyCode::Char('q') => self.quit = true,
        _ => {}
    }
}
```

- **Handlers return `Pass` for keys they don't bind.** The per-view `Esc`/`q`
  arms are deleted (`crates/lt-tui/src/detail.rs:201`,
  `crates/lt-tui/src/popup.rs:495,508,543`,
  `crates/lt-tui/src/new_issue.rs:439-442`, and the list's own `q` arm,
  `crates/lt-tui/src/lib.rs:1267`): no view has to handle back/quit — the floor
  does. A narrower `Esc` binding still wins where one exists (the comment
  input's cancel, `crates/lt-tui/src/detail.rs:320-325`; Ctrl-C in search).
- **Scroll is a base-layer interface.** One method over a motion enum (`Down`,
  `Up`, `Top`, `Bottom`, `HalfPageDown`, `HalfPageUp`, `PageDown`, `PageUp` —
  the `j`/`k`/`g`/`G`/Ctrl-d/Ctrl-u/PageDown/PageUp family) with a no-op
  default; `List`, `Detail`, and `Popup` override it with their existing
  movement code, and their per-handler duplicate arms are deleted. Any view gets
  the bindings for free; views override the semantics (selection movement vs
  offset scrolling). Scroll keys resolve at the focused view and do not cascade:
  a scroll motion acting on a view beneath the one you see is the same hostility
  the modal's form policy already names. Disclosed deviation from the review
  sketch's per-motion methods (`scroll_down()` et al.): one
  `scroll(motion, viewport)` avoids eight near-identical trait methods; the seam
  is identical.
- **Text contexts are unchanged**: Search, Help, the comment input, and the
  new-issue fields consume printable/editing keys into their editors, so neither
  the scroll layer nor the cascade ever sees a printable there — their
  in-handler navigation arms (arrows in Search, `j`/`k` in Help) stay.
- **The round-1 q-leak hazard is resolved structurally.** The cascade never
  delivers `q` to the base's Quit from an overlay, because the floor consumes it
  as Back first; `q` means quit only when the base is focused. The keymap
  design's rejection of a global `q` binding is honored — the floor is not a
  binding, it is the cascade's terminal.
- Writers no longer call `route_state_event` (Decision 4); handlers call
  `watch`/`unwatch` and the service's write methods. Nothing in the State path
  performs I/O beyond the re-reads themselves, so the round-1 borrow argument
  (ctx field borrows disjoint from `&mut self.views`) holds unchanged.

Rejected forms (round-1 table, still standing, plus this round's):

| Option                                               | Why rejected                                                                                                     |
| ---------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| Broadcast to every view module                       | every possible view is asked "are you displayed?" — the stack answers by existence; closed views cannot be asked |
| Central scope→consumer match                         | a registry that grows with every scope-consumer pair, away from the components that own the dependency           |
| Runtime subscription registry (`Vec<(pattern, fn)>`) | the subscriber set is the live stack itself; a registry re-encodes it as data and loses compile-time visibility  |
| Relay-proper: dependencies derived from queries      | needs queries-as-data plus dependency tracking over SQLite — a reactive framework; speculative at this size      |
| Per-view `Esc`/`q` arms (round-1 interim)            | N copies of Back/quit policy; a new view can forget them — the floor makes them unforgettable                    |
| Scroll keys cascade past the focused view            | a motion acting on an invisible view; consuming at the focused view keeps scroll where the user is looking       |

## Decision 7: lifecycle typestates — consumers of the loop's events

The `sync`/`auth` typestates are delivered (`crates/lt-tui/src/lib.rs:479-535`)
and stay, but they become pure consumers: the TUI no longer spawns or schedules,
so `SyncStatus` drops its scheduling payload and its transitions are driven
entirely by events and the two user commands.

```rust
// crates/lt-tui/src/lib.rs
pub enum SyncStatus {
    /// Nothing has happened yet, or the loop reported NotAuthenticated.
    Idle,
    /// The loop announced a cycle (`Sync(Started)`).
    Syncing,
    /// `next_sync_at` leaves: scheduling belongs to the loop.
    Synced { synced_at: chrono::DateTime<chrono::Utc> },
    Failed { message: String },
}
```

`AuthStatus` keeps its five variants; the `Login(Success)` arm simplifies
because `viewer` is no longer optional. Every transition:

| Trigger                     | Transition                                                                                                                        |
| --------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| startup `run()`             | `fetch_viewer()` Some → `auth = Authenticated { viewer }`, None → `Unknown`; `sync` stays `Idle` until the loop's first `Started` |
| `Sync(Started)`             | `sync = Syncing`                                                                                                                  |
| `Sync(Done(viewer))`        | Some(v) → `auth = Authenticated { v }` (None leaves auth unchanged); `sync = Synced { synced_at }`                                |
| `Sync(Error(msg))`          | `sync = Failed { message }`; base list Loading→Idle repair                                                                        |
| `Sync(NotAuthenticated)`    | `auth = Unauthenticated`; `sync = Idle`; same repair                                                                              |
| `Login(Success { viewer })` | `auth = Authenticated { viewer }` (the follow-up delta sync is the loop's, Decision 2)                                            |
| `Login(Error(msg))`         | `auth = Failed { message }`; `footer_msg` stays a direct transient write                                                          |
| `r` key                     | immediate cache re-read of the base list, then `service.request_sync()` — no typestate write; `Syncing` arrives via `Started`     |
| `L` key                     | gate `!matches!(auth, Authenticating)`; → `Authenticating`; `service.login()`                                                     |

`App::start_sync`, `App::start_login`, `periodic_sync_due`, and
`maybe_start_periodic_sync` (`crates/lt-tui/src/lib.rs:948-986,1221-1226`) die;
`synced_at` is still read from the DB meta `last_synced_at` at the `Done`
transition, falling back to the clock.

The footer label stays derived at render from `(SyncStatus, AuthStatus, Clock)`
(`crates/lt-tui/src/sync.rs:7-23`). `format_sync_label` collapses its 1-minute
and N-minute arms into one and gains an upper bound:

```rust
match elapsed.num_minutes() {
    ..=0 => "synced just now".to_string(),
    mins @ 1..60 => format!("synced {mins} min ago"),
    _ => "synced over an hour ago".to_string(),
}
```

## Decision 8: rendering — one bottom-up walk, no base special case

`ui::render` today renders the base table unconditionally
(`crates/lt-tui/src/ui/mod.rs:53`) and then walks `views[1..]` in
`render_overlays` (`crates/lt-tui/src/ui/mod.rs:102-130`), with an unreachable
`View::List` arm above the base. Rendering becomes one bottom-up walk over
`views[..]`:

- The `List` arm renders the full-frame table wherever it sits — the base is not
  special to the renderer, only to the stack's never-empty invariant. The dead
  `View::List(_) => {}` overlay arm dies with the split.
- **`Identity` dies** (`crates/lt-tui/src/ui/chrome.rs:12-34`): it was a
  hand-copied projection of `AuthStatus`. The header functions take
  `&AuthStatus` and read `viewer_name()`/`org_name()` themselves.
- **Per-view render data is derived in the arm that needs it**: `SortOrder`
  (`crates/lt-tui/src/ui/mod.rs:104-107`) is built inside the `Search` arm from
  the base list's query, not hoisted above the walk where every other view pays
  for it. Same for the modal's `keyboard_enhanced` read.
- The header's top-of-stack search variant and `render_status_row`'s
  top-of-stack matches are unchanged.

Rejected: keeping the base render + `skip(1)` overlay walk — two render paths
that differ only in where the view happens to sit, plus an unreachable match arm
as permanent residue.

## Decision 9: team-scoped cache — schema and targeted sync

Delivered in item 3: `MIGRATION_2` adds `team_id`/`position` columns to
`workflow_states` (a state belongs to exactly one team; the issue read-model
joins keep working) and the `team_memberships` table; registered statements
cover the scoped upsert (with
`position = COALESCE(excluded.position, workflow_states.position)` so
issue-driven upserts pass `NULL` without clobbering a synced position),
`QUERY_TEAMS`, `QUERY_TEAM_STATES` (position order, `NULL`s last by name),
`QUERY_TEAM_MEMBERS`, and replace-set membership writes. Issue upserts back-fill
`team_id` for free, so the pickers work offline after any ordinary sync;
memberships are written only by the targeted team sync (an assignee is not
provably a member). Team metadata stays out of full/delta sync (Decision 3
bounds the fan-out instead). `lt sim` derives memberships from seeded issues,
keeping the pickers drivable offline per [[dst.md]].

One type change this round:

- **`lt-storage`'s `TeamState` struct dies**
  (`crates/lt-storage/src/db/teams.rs:17-25`). It re-declares the fields of the
  `lt-types` position-carrying fragment (`WorkflowStateWithPosition`,
  `crates/lt-types/src/states.rs:58-62`) purely to group arguments.
  `upsert_team_state` takes `(conn, team_id, &WorkflowStateWithPosition)` — the
  targeted sync (`crates/lt-runtime/src/teams.rs:36-46`) already holds exactly
  that type. The issue-driven back-fill
  (`crates/lt-storage/src/db/issues.rs:146`) has no fragment and no position: it
  binds the same registered statement internally with a SQL `NULL` position,
  needing no public struct at all.

## Decision 10: keys through the queue — input thread, single-wait loop

Delivered in item 7 and unchanged in shape: a detached input thread forwards key
presses onto the queue (`crates/lt-tui/src/lib.rs:1161-1175`); the loop draws,
blocks up to 100ms on `EventPump::next`, then drains `try_recv`
(`crates/lt-tui/src/lib.rs:1228-1255`); `EventPump::Scripted` keeps loop tests
thread-free with exhaustion-as-error. Two updates:

- The loop body loses `maybe_start_periodic_sync` (the loop's clock belongs to
  the service now, Decision 2). `poll_search_debounce` is the one inline timer
  left — a 150ms render debounce over view-local state, not sync scheduling, so
  it stays a clock predicate in the frame loop.
- Channel ends move to `lt-cli` (Decision 1): the same `Sender` feeds the input
  thread and the service's `OnEvent` wrapper. `Disconnected` remains unreachable
  in production — the service and input thread hold senders for the process
  lifetime; the `Channel` arm treats it as an idle tick.

## Decision 11: no silent error drops

Global policy, this PR series: **no error is silently dropped — logging is the
minimum; making the function fallible and propagating is the ideal.** Per
[[rust.md]], the class is closed mechanically rather than instance by instance:

```toml
# Cargo.toml [workspace.lints.clippy]
let_underscore_must_use = "deny"
let_underscore_untyped = "deny"
```

`let_underscore_must_use` catches every discarded `Result`;
`let_underscore_untyped` catches the rest of the `let _ =` idiom (an untyped
underscore binding is either a discarded value or an obfuscated one). Every
current instance across all crates is fixed in the same change:

- **Fallible-caller sites propagate**: the TUI's enqueue paths become the
  fallible service write methods surfaced in `footer_msg` (Decision 4).
- **Terminal-edge and cleanup sites trace**: `open::that`, the crossterm
  keyboard-enhancement push/pop, `lt-cli`'s log/tempfile removal,
  `lt-upstream`'s OAuth `http_reply` responses — best-effort by design, now
  `tracing::warn` (or `debug` where failure is the normal shutdown path, e.g. a
  channel `send` to a receiver that quit).
- **Non-`Result` underscore bindings** (the borrow-release `let _ = modal;`
  pairs, `crates/lt-tui/src/new_issue.rs:472,506`) are restructured or become
  `drop(..)`.

## Direction: layout components

Non-normative. The current views conflate a layout component with the one entity
it renders: a detail pane could apply to an issue, a project, a team, or a user
and still be the same detail component; the same holds for the picker popup and
the create modal. The expected future split is layout components parameterized
by entity data. It is deliberately not designed here: the TUI renders exactly
one entity type today, and abstracting for a single consumer is the speculative
flexibility [[posture.md]] forbids. This section exists so the split is
recognized as direction, not discovered as a rewrite.

## Scope relevance

With payload-free events, stale data cannot be applied — events carry none. With
the view stack, display checks cannot be forgotten — a closed view does not
exist. Drops happen in exactly three ways: no consumer exists, an id-relevance
guard falls through inside a live consumer, or the base's `focused` policy
declines. Duplicate or late events are idempotent re-reads of current truth.

| #   | Event at apply time                            | Stack contents                            | Handling                                                                                                                             |
| --- | ---------------------------------------------- | ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| N1  | `State(Comments{A})`                           | `Detail(A)` anywhere in the stack         | consume re-reads `query_comments(A)` — whether the producer was the watch refresh or `create_comment`                                |
| N2  | `State(Comments{A})`                           | no `Detail` / `Detail(B)`                 | no consumer exists / id mismatch falls through — including a refresh that was in flight when its view popped                         |
| N3  | `State(Comments{A})` twice (fast close/reopen) | `Detail(A)`                               | both re-read; idempotent                                                                                                             |
| N4  | `State(Teams)`                                 | `NewIssue` in the stack                   | re-read teams; re-anchor selected team by id                                                                                         |
| N5  | `State(Teams)`                                 | no `NewIssue`                             | no consumer exists (the scope is unwatched; the loop stops refreshing it)                                                            |
| N6  | `State(Team{T})`                               | `NewIssue`, team T selected               | re-read states+members; preserve picks by id; clear `loading`                                                                        |
| N7  | `State(Team{T})`                               | `NewIssue` on team U / no consumer        | id mismatch falls through / no consumer (U is the watched scope now; its refresh follows)                                            |
| N8  | `State(Team{T})`                               | `Popup { team_id: Some(T) }` in the stack | rebuild `items`; re-anchor selection                                                                                                 |
| N9  | `State(Issues)`                                | `[List]` — base focused                   | `ListView::consume` re-reads offset-preserving, then seeks `pending_select` if set                                                   |
| N10 | `State(Issues)`                                | overlay(s) above the base                 | base's `focused` guard drops it; a live `Detail` re-reads its issue (`query_issue_by_id`)                                            |
| N11 | `Runtime(Sync(_))`                             | any                                       | typestate transitions only: `Started` → `Syncing`, `Done` → `Synced` — the `State(Issues)` the loop emits alongside drives N9/N10    |
| N12 | `Runtime(Login(_))`                            | any                                       | `Authenticating` gates `L`; `Success { viewer }` → `Authenticated`; the follow-up delta sync is the loop's                           |
| N13 | `Key(k)`, unbound anywhere                     | overlay(s) atop base                      | cascades toward `views[0]`, then the floor: `Esc`/`q` = Back (pop); at the base, reset/quit — `q` never reaches Quit from an overlay |
| N14 | `Key(k)` in a text context                     | any                                       | forwarded to the editor widget and `Consumed` — printable input never cascades                                                       |
| N15 | scroll key, unbound in the focused view        | any                                       | resolved by the focused view's `scroll` (no-op default); never cascades to views beneath                                             |

## Keymap design reconciliation

The keymap redesign ([PR #43](https://github.com/willruggiano/lt/pull/43)) and
this ADR are open concurrently; whichever lands second rebases its dispatch
seam. Its keymap core — `Key`/`Action`/`Binding`, contexts, tables, help
generation, no-timer chords — is entirely unaffected. What holds and what
changes:

- The dispatch site is the `AppEvent::Key` arm (delivered):
  `AppEvent::Key(ev) => dispatch_key(app, Key::from_event(ev))`. The queue's
  wire type stays the raw crossterm `KeyEvent`; normalization happens exactly
  once, at the boundary between transport and keymap. Chords need no timer: the
  pending prefix is `App` state and survives idle frames of the `recv_timeout`
  loop.
- `key_context` is a stack walk: resolve against the focused view's context
  first (sub-focus rules unchanged), and `Resolved::Unbound` in a pass-through
  context continues to the next view down. **Beneath every table now sit the
  scroll defaults and the `Esc`/`q` floor** (Decision 6): resolution reaches
  them only when no table binds the key. The tables may still name Back/quit
  bindings for help generation — mechanically redundant with the floor, an
  editorial choice for the keymap PR.
- **GLOBAL and the scroll interface must merge on rebase**: both deliver
  per-view semantics for the same key (`j` scrolls in Detail, moves the
  selection in List) — GLOBAL as a resolution layer, `View::scroll` as a
  dispatch layer. One of them wins; the seam is the same either way.
- Its "popup return-mode" risk entry is resolved structurally (delivered):
  confirm/cancel pop, restoring whatever is beneath; phase 4's "s/p/a from
  Detail" pushes a `PopupView` built from the detail's own issue.
- Its loop-test harness reference is `EventPump::Scripted` with
  `AppEvent::Key(...)` entries (delivered).

## User-visible behavior changes

Relative to the pre-ADR baseline; 1–11 shipped with items 2–7, the rest land
with items 8–10.

1. Modal open and the state/assignee popups no longer block the UI thread on the
   network — instant reads.
2. Picker data may be one refresh stale; mitigated by the prompt refresh on
   watch. Cold start offline shows empty pickers instead of an error string;
   per-fetch error text is replaced by `tracing` + the global sync label.
3. The state picker sorts by Linear's stored `position` (states known only from
   issue upserts sort last by name until a targeted refresh records positions).
4. Optimistic edits re-read through the active filter: an edit that no longer
   matches disappears immediately instead of lingering until the next refresh.
5. Sync completion refreshes the list on any page when the list has focus (was
   page-1 only); the re-read preserves the offset.
6. An open detail pane re-reads its issue when the issues scope changes — a
   popup edit or sync upsert is visible in the pane immediately.
7. The optimistic comment author comes from the persisted viewer; it is absent
   before the first successful sync.
8. Worker panics surface as `sync error: ...` instead of a silent label repair.
9. "full sync..." folded into "syncing..." — a hardcoded string with no
   behavioral difference.
10. Identity is state, not residue: on `NotAuthenticated` after a session had
    identity, the header shows "(not authenticated)" instead of stale names;
    `assignee:me` requires a live `Authenticated`. Pressing `L` while
    authenticated blanks the header for the duration of the login.
11. After a failed login, periodic sync pauses until re-auth.
12. The sync label caps its age: past an hour it reads "synced over an hour ago"
    instead of an unbounded minute count.
13. `r` requests a full sync from the persistent loop: pressed mid-cycle it
    coalesces into a follow-up sync instead of being ignored.
14. The state/assignee/priority popups accept `q` as Back and the shared scroll
    motions (`g`/`G`/Ctrl-d/Ctrl-u/PageUp/PageDown); those keys were ignored
    there before.
15. Comment/edit/create failures surface in the footer; they were silently
    discarded.
16. The failed-login and identity semantics of 10–11 are unchanged by the loop
    move: the cadence pauses on `NotAuthenticated` or a failed login until
    re-auth or an explicit `r`.

## Test migration

Round 1's migrations are delivered (mode-tag setups became stack pushes; loop
tests script typed `AppEvent`s through `EventPump::Scripted`; `Disconnected`
tests died with the state they exercised). This round:

- **A recording fake `SyncService` replaces `NoopSyncService`**
  (`crates/lt-tui/src/lib.rs:548-573`): constructed over the same in-memory
  `Database` the app reads, its write methods perform the real enqueues and emit
  through the same callback (synchronously, so tests stay thread-free), and it
  records `watch`/`unwatch`/`request_sync`/`login` calls for assertions —
  push/pop watch wiring, the modal's team-change swap, and the `r`/`L` commands
  become direct assertions on the recording.
- **Loop tests** script `AppEvent::Runtime(..)` entries (a variant rename from
  `Lifecycle`); the typestate transition table and the label tests (including
  the over-an-hour branch) drive `consume` directly.
- **Cascade tests** cover the floor and scroll layers: `Esc`/`q` pop from each
  overlay and reset/quit at the base; a scroll key moves the focused view and
  never a view beneath; a printable key in a text context never cascades.
- **Service-loop tests** (`lt-runtime`): the loop's decision core (command +
  deadline + watch set → actions) is factored so cadence, pause, and watch
  policies are testable without threads; the API edge keeps its `FakeTransport`
  tests (`comments.rs`/`teams.rs`, unchanged).
- **The lint gate is the error-policy test** (Decision 11), plus footer
  assertions for surfaced write failures.

## Delivery: stacked PRs (each green under `make test` + `make check`)

Landed:

1. `docs(design)` — the round-1 document.
2. `refactor(tui): view stack` (fe0049e).
3. `feat(storage,runtime): team-scoped cache and team metadata sync` (432b98c).
4. `refactor(tui): AppEvent queue and StateEvent routing` (1765a3e).
5. `refactor(tui,runtime): cache-first pickers` (3f4633c).
6. `refactor(runtime,tui): sync/login completion callbacks and lifecycle typestates`
   (a5b8059).
7. `refactor(tui): keys through the queue` (f6a5e02).

Open (this revision):

8. **`refactor(runtime,cli,tui): runtime-owned sync service`** —
   `RuntimeEvent`/`StateEvent`/`OnEvent` in `lt-runtime`; `SyncEvent::Started`;
   `LoginEvent::Success { viewer }` non-optional; the trait rewritten
   (`run`/`watch`/`unwatch`/`request_sync`/`login`/`fetch_viewer` + the write
   methods and `IssueEdit`); `LinearSyncService::new(db, on_event)` with the
   command channel, loop, login worker, per-iteration `catch_unwind`, and
   private sync helpers; `build_create_request` moves in; `lt-storage`'s
   `TeamState` dies. `lt-cli` creates the channel, wraps the callback, spawns
   the loop thread, and passes sender + receiver into `tui::run`. `lt-tui`:
   `AppEvent::Runtime`; `LifecycleEvent`, `spawn_state_refresh`, `start_sync`,
   `start_login`, and the periodic gate die; push/pop watch wiring; writers call
   the service and surface errors; `pending_select`; `r`/`L` become commands;
   typestates per Decision 7. The seam flip is atomic — the old spawn methods
   and the new loop cannot meaningfully coexist — so this is one PR, staged as
   add-new/migrate/remove-old commits.
9. **`refactor(tui): presentation diet`** — `StateCtx` shrinks to
   `{ db, viewer_name }`; `ListView` owns `args`/`filter` and the query-only
   methods; the one-walk renderer with the `List` arm; `Identity` dies (header
   takes `&AuthStatus`); `SortOrder` derived in the `Search` arm;
   `impl From<Team/WorkflowState/User> for PopupItem` replaces the hand-maps;
   the sync label collapse + over-an-hour branch; `lt-tui` comments scrubbed of
   cache-vs-live sourcing language; `Cargo.toml` dependencies alphabetized and
   the stray section comment removed. Requires item 8.
10. **`refactor(tui): dispatch floor and scroll defaults`** — handlers return
    `Pass` for unbound keys; per-view `Esc`/`q` arms deleted; the floor;
    `ScrollMotion` + `View::scroll` with the `List`/`Detail`/`Popup` overrides;
    cascade/floor/scroll tests. Independent of items 8–9 (builds on the
    delivered dispatch walk).
11. **`chore(workspace): no silent error drops`** —
    `let_underscore_must_use`/`let_underscore_untyped` at deny plus every
    instance fixed across all crates. Last, so the gate lands on the finished
    seam instead of churning code items 8–9 delete.

Ordering: 8 before 9; 10 independent; 11 last.

## Open questions

None blocking.

- Startup's synchronous `fetch_viewer` (`crates/lt-tui/src/lib.rs:1106`) still
  blocks briefly before the TUI starts and is the one remaining synchronous
  service call in the TUI; reading the persisted `db::synced_viewer` instead is
  a natural follow-up, deliberately out of scope here.
- Whether `AuthStatus::Failed { message }` earns its keep over `Unauthenticated`
  plus the footer message (identical label and gates) — collapse later if it
  stays inert.
