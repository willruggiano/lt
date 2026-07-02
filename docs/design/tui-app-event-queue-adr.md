# TUI AppEvent Queue and State Propagation (ENG-32)

## Status

Proposed

## Context

The TUI event loop drains four independent `Option<mpsc::Receiver<T>>` fields
every frame, each with its own poll function and its own borrow-checker dance
(take/restore, or collect-into-a-`Vec`). The overall
spawn-a-thread-and-drain-per-frame model is described in
[[architecture.md#TUI]].

```text
run_app loop (crates/lt-tui/src/lib.rs:805-853), each frame:

  poll_sync_events ──────── SyncState.sync_rx          Receiver<SyncEvent>
  [inline 30s timer] ────── spawns delta sync
  poll_modal_events ─────── NewIssueModal.modal_rx     Receiver<ModalEvent>
  poll_detail_comment_events  App.detail_comment_rx    Receiver<CommentSyncEvent>
  poll_search_debounce ──── (timer, not a channel)
  poll_login_events ─────── App.login_rx               Receiver<LoginEvent>
  draw; block <=100ms on EventSource::next_key
```

| Receiver           | Field / declared at                          | Poll fn                                        | Producer                                                |
| ------------------ | -------------------------------------------- | ---------------------------------------------- | ------------------------------------------------------- |
| `SyncEvent`        | `SyncState.sync_rx` (`lib.rs:260`)           | `poll_sync_events` (`sync.rs:89`)              | `SyncService::spawn_sync` returns it (`adapter.rs:41`)  |
| `LoginEvent`       | `App.login_rx` (`lib.rs:395`)                | `poll_login_events` (`sync.rs:51`)             | `SyncService::spawn_login` returns it (`adapter.rs:87`) |
| `CommentSyncEvent` | `App.detail_comment_rx` (`lib.rs:353`)       | `poll_detail_comment_events` (`detail.rs:211`) | thread spawned in `App::open_detail` (`detail.rs:52`)   |
| `ModalEvent`       | `NewIssueModal.modal_rx` (`new_issue.rs:88`) | `App::poll_modal_events` (`new_issue.rs:257`)  | thread in `new_issue_load_states_and_assignees_bg`      |

Every receiver is single-producer, single-message-kind, cross-thread (all
workers are `std::thread::spawn` + `std::sync::mpsc`; no async runtime).
Dropping a receiver doubles as cancellation, and a dropped sender (worker panic)
doubles as a completion signal via the `Disconnected` arm. The four mechanisms
all differ slightly, the `Option` wrapping leaks into every consumer, and each
new background job adds a fifth copy of the pattern.

Three further problems surfaced in review:

- The comment and modal events are **update notifications carrying data
  payloads**: workers re-read the database (or the API) and ship rows through
  the channel, while the optimistic-write paths (`submit_comment`,
  `popup_confirm`) maintain hand-built in-memory copies of the same state
  (`detail.rs:147-160`, `popup.rs:429` `apply_optimistic_in_memory`). There are
  two read models for the same truth, and staleness handling exists only because
  events carry data.
- The pickers are not local-first: the new-issue modal fetches teams
  **synchronously on the UI thread** (`new_issue.rs:118`), and the
  state/assignee popups fetch states/members the same way (`popup.rs:234`,
  `popup.rs:277`). Team-scoped picker data is not in SQLite at all — the lookup
  tables (`lt-storage/src/db/mod.rs:96-98`) are flat `(id, name)` with no team
  scoping.
- Event consumption is imprecise: view state lives in per-view `Option` fields
  on `App` beside a redundant `Mode` tag, so any dispatch of an update must ask
  every possible view "are you displayed?" — when the view's very existence
  should answer that.

This ADR replaces all of it with one long-lived `mpsc::channel<AppEvent>`, one
propagation rule — **writes land in SQLite; a payload-free invalidation names
the scope that changed; the views that display that scope re-read it** — and one
structural change that makes the routing precise: view state moves into a
**stack of live views**, and events route to what exists. Key input joins the
same queue, making the loop a single blocking wait.

```text
  [input thread] ──── Key ───────┐
  [sync worker] ── Lifecycle ────┤  Sender<AppEvent> (cloned)
  [login worker] ── Lifecycle ───┼──────────────┐
  [comment refresh] ─ State(..) ─┤              v
  [team refresh] ──── State(..) ─┘      App.events_rx ── App::apply
                                          |         |            |
                              Key: top of the   State: base    Lifecycle:
                              view stack        list + every   hooks (job
                              (else the list)   live view      gates), then
                                                consumes       State(Issues)
  optimistic writers (same thread) ──────────────┘
```

### Prior art divergence

The tracking issue cites gitui's `queue.rs:86-193`. gitui's `Queue` is an
`Rc<RefCell<VecDeque<InternalEvent>>>` for same-thread component-to-component
messaging; its cross-thread async results travel on a separate channel drained
by `update_async`. lt's producers are worker threads plus one input thread, so
the honest adaptation is the channel half of gitui's split, not the
`Rc<RefCell<VecDeque>>` half. Same-thread propagation needs no queue at all: it
is a direct call to the same routing function the drain uses (Decision 3). The
view stack, however, is gitu's shape (`Vec<Screen>`), not gitui's.

## Decision 1: event taxonomy — keys, state invalidations, lifecycle results

Three kinds of message, three variants:

```rust
// crates/lt-tui/src/lib.rs
/// A message to the event loop: a key press from the input thread, a state
/// invalidation, or a background-job lifecycle outcome. One channel, one drain.
pub enum AppEvent {
    /// A key press (KeyEventKind::Press only), raw from crossterm; normalized
    /// at apply time.
    Key(crossterm::event::KeyEvent),
    /// The named slice of application state changed; re-read it if displayed.
    State(StateEvent),
    /// A background job finished; carries the outcome, not data.
    Lifecycle(LifecycleEvent),
}

/// A payload-free invalidation. Variants carry only the scope id a view needs
/// to decide relevance and which query to re-run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateEvent {
    /// The issues read model changed (optimistic edit/create, or sync upsert).
    Issues,
    /// One issue's comment thread changed.
    Comments { issue_id: String },
    /// The team list changed.
    Teams,
    /// One team's workflow states and memberships changed.
    Team { team_id: String },
}

/// Background-job outcomes: identity, error text, scheduling — not
/// invalidations. Wraps the `SyncService` trait's vocabulary.
pub enum LifecycleEvent {
    Sync(SyncEvent),
    Login(LoginEvent),
}
```

- **`StateEvent` granularity is per scope (table + owning id)**, not per table
  and not per row. A view needs exactly (a) a relevance check and (b) a query to
  re-run; `Comments { issue_id }` and `Team { team_id }` are the minimal keys
  for both. `Team` deliberately does not split states from members: one trait
  call writes both (Decision 4), so one event and one paired re-read is honest.
- **`Lifecycle` events are not invalidations.** They carry identity, error text,
  and scheduling information. Sync completion does not additionally emit
  `State(Issues)`: the `Done` arm of the lifecycle consumer calls
  `route_state_event(&StateEvent::Issues)` directly — the unification happens at
  the function level, without ordering two events that always travel together.
- `SyncEvent`/`LoginEvent` stay in `lt-runtime/src/sync/service.rs` (the trait's
  vocabulary); `AppEvent`/`StateEvent`/`LifecycleEvent` live in `lib.rs` next to
  `App`.

Rejected alternatives:

| Option                                 | Why rejected                                                                                                 |
| -------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| Payload-carrying wrapper events        | payloads are what make staleness a problem; a payload-free event can only trigger a re-read of current truth |
| Events carry rowids / entity diffs     | speculative granularity; every consumer re-reads whole scopes anyway                                         |
| One `StateEvent::Any`                  | forces every view to re-read everything on every event; loses scope relevance                                |
| Emit `State(Issues)` from sync workers | duplicates the sync `Done` outcome across two ordered events                                                 |

`App` gains two non-optional fields, created in `App::new`:

```rust
/// Producer end of the app event queue; cloned into every background worker.
pub events_tx: mpsc::Sender<AppEvent>,
/// The single consumer, drained once per frame.
events_rx: mpsc::Receiver<AppEvent>,
```

`SyncState.sync_rx`, `App.login_rx`, `App.detail_comment_rx`,
`NewIssueModal.modal_rx`, and the enums `CommentSyncEvent` and `ModalEvent` are
deleted.

## Decision 2: the view stack — a view exists iff it is displayed

Today `App` holds a `Mode` tag plus a parallel `Option` field per view
(`detail`, `new_issue_modal`, `help_popup`, `search_overlay`, the
`popup_items`/`popup_selected`/`popup_anchor` triple), and the codebase already
maintains the invariant that each field is populated iff its mode is active —
set on entry, cleared on exit, every poller gated on the `Option`. `Mode` is a
redundant tag over that invariant, and it is what forces every consumer of an
update to ask "am I displayed?".

The invariant becomes structure:

```rust
// crates/lt-tui/src/lib.rs
pub struct App {
    // Base list: always rendered under everything (ui/mod.rs:58-59), never
    // popped. issues, table_state, args, pagination, status, active_filter,
    // the double-esc fields, footer_msg, viewport_height: unchanged.

    /// Views stacked over the base list, bottom to top. Empty = the list has
    /// focus. The top view receives keys; every view consumes StateEvents.
    pub views: Vec<View>,

    /// Background-job gates and scheduling (lifecycle consumers).
    pub hooks: Hooks,

    // identity/session/db/clock/service/events_tx/events_rx: unchanged.
}

/// One overlay's complete state. A view exists iff it is displayed; there is
/// no separate mode flag to keep consistent.
pub enum View {
    Detail(DetailView),
    Popup(PopupView),
    NewIssue(NewIssueModal), // shape unchanged (new_issue.rs:62)
    Search(SearchOverlay),   // shape unchanged (popup.rs:101)
    Help(HelpPopup),         // shape unchanged (popup.rs:61)
}

/// Detail pane: today's IssueDetailView (detail.rs:11) plus the App fields
/// that were only valid in Detail mode.
pub struct DetailView {
    pub issue: Issue, // owned: severs submit_comment's list-selection read
    pub comments: Vec<Comment>,
    pub parent: Option<Issue>,
    pub children: Vec<Issue>,
    pub scroll: u16,                   // was App.detail_scroll
    pub comment_input: Option<String>, // was App.comment_input
}

/// State/priority/assignee picker: today's App.popup_* fields plus the
/// target captured at open.
pub struct PopupView {
    pub kind: PopupKind,
    /// Target issue id, captured at open; confirm no longer depends on the
    /// list selection being unchanged.
    pub issue_id: String,
    /// The issue's team — the scope key for Team{T} relevance (state and
    /// assignee popups; None for the static priority popup).
    pub team_id: Option<String>,
    pub items: Vec<PopupItem>,
    pub selected: usize,
    /// Written by the renderer when this popup sits directly on the base
    /// table; None => render_popup centers.
    pub anchor: Option<Rect>,
}

/// Background-job gates: today's SyncState (lib.rs:258) plus the login gate
/// that replaces `login_rx.is_some()` (lib.rs:929).
pub struct Hooks {
    pub syncing: bool,
    pub sync_status_label: String,
    pub next_sync_at: Option<Instant>,
    pub login_in_flight: bool,
}
```

- **A stack, not a slot.** Today's topology is a star — every overlay opens from
  and returns to List — so `Option<View>` would suffice today. But the second
  layer is not speculative: the keymap design
  ([PR #43](https://github.com/willruggiano/lt/pull/43)) names "popup
  return-mode" as its phase-4 blocker (`popup_confirm`/`popup_cancel` hardcode
  `Mode::List`, `popup.rs:341,346`, so s/p/a popups cannot open from Detail).
  `Option<View>` plus a return-view field would recreate exactly the
  parallel-state smell this decision deletes. The stack also models what the
  renderer already draws — base always, overlays on top — and pop restores
  whatever is beneath with its state intact. Cost over a slot: a `for` loop in
  two places. Depth is ≤1 today, ≤2 after keymap phase 4; open sites, not the
  container, bound the depth.
- **The base list stays out of the stack.** It is rendered every frame under
  everything, never popped, and its state (`issues`, `table_state`,
  `pagination`, `active_filter`) is read by overlays at open time. Putting it in
  the stack would make "empty stack" an unreachable panic state. Empty stack =
  the list has focus.
- **Entry is push, exit is pop.** `Back`/Esc pops everywhere; the list's Esc
  keeps the double-esc reset (`lib.rs:862-882`). `popup_confirm` becomes: pop
  the `PopupView`, `enqueue_edit(&p.issue_id, ...)`, route `StateEvent::Issues`
  (Decision 3).
- **Latent smells die structurally**: `popup_items`/`popup_selected` are never
  cleared on close today (`popup.rs:341-348`) — now the whole `PopupView` drops;
  the dead `input_mode`/`input_buf` pair (`lib.rs:324-325`, unreachable render
  branch `ui/mod.rs:94-95`) is deleted; `submit_comment` stops reading the list
  selection (`detail.rs:140`) because `DetailView` owns its issue.
- **Popup anchoring**: the base-table renderer computes the anchor from column
  geometry (`table.rs:43-59`). It writes into the popup only when the popup is
  the sole stack entry (`[View::Popup(p)]` slice pattern); for a future popup
  over Detail the anchor stays `None` and `render_popup` already centers.
- The renderer draws the base list, then the stack bottom-up — replacing the six
  mode-checked blocks in `render_overlays` (`ui/mod.rs:104-152`).
  `render_status_row`'s Detail branch and the header's Search branch match on
  the stack top instead of `Mode`.

## Decision 3: routing — each event kind has exactly one consumer path

```rust
// crates/lt-tui/src/lib.rs
impl App {
    pub fn apply(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.dispatch_key(key),
            AppEvent::State(ev) => self.route_state_event(&ev),
            AppEvent::Lifecycle(ev) => self.consume_lifecycle(ev),
        }
    }

    /// Keys go to the top view; empty stack means the list has focus. This
    /// is today's Mode router (lib.rs:844-851) keyed on the stack instead;
    /// the keymap design's dispatch_key replaces it wholesale when it lands.
    fn dispatch_key(&mut self, key: KeyEvent) {
        match self.views.last() {
            Some(View::Detail(_)) => detail::handle_key(self, key),
            Some(View::Popup(_)) => popup::handle_key(self, key),
            Some(View::NewIssue(_)) => new_issue::handle_key(self, key),
            Some(View::Search(_)) => popup::handle_search_key(self, key),
            Some(View::Help(_)) => popup::handle_help_key(self, key),
            None => handle_list_key(self, key),
        }
    }

    /// Route a state invalidation: the base list first (its consumer owns
    /// the don't-clobber-under-overlays policy), then every live view. All
    /// stacked views are visible, so all of them consume; a closed view no
    /// longer exists, which is the entire display check.
    fn route_state_event(&mut self, ev: &StateEvent) {
        self.list_consume(ev);
        let db = &self.db;
        for view in &mut self.views {
            view.consume(db, ev);
        }
    }

    /// The base list's subscription: Issues, only while no overlay is up
    /// (preserving sync.rs:109's guard — search-confirm and popup opens read
    /// `self.issues`, so a refresh must not swap it beneath them).
    fn list_consume(&mut self, ev: &StateEvent) {
        if matches!(ev, StateEvent::Issues) && self.views.is_empty() {
            self.do_fetch(false); // offset- and selection-preserving
        }
    }
}

impl View {
    fn consume(&mut self, db: &Database, ev: &StateEvent) {
        match self {
            View::Detail(v) => v.consume(db, ev),
            View::Popup(v) => v.consume(db, ev),
            View::NewIssue(v) => v.consume(db, ev),
            View::Search(_) | View::Help(_) => {} // no DB-backed scopes
        }
    }
}
```

A representative consumer — the per-view `consume` match arms are the view's
declared dependencies; irrelevant scopes fall through:

```rust
impl DetailView {
    fn consume(&mut self, db: &Database, ev: &StateEvent) {
        match ev {
            StateEvent::Comments { issue_id } if issue_id == self.issue.id.inner() => {
                if let Ok(conn) = db.connect()
                    && let Ok(comments) = lt_runtime::db::query_comments(&conn, issue_id)
                {
                    self.comments = comments;
                }
            }
            StateEvent::Issues => {
                // The displayed issue may be what changed (a popup edit
                // confirmed above this pane, or a sync upsert).
                if let Ok(conn) = db.connect()
                    && let Ok(Some(fresh)) =
                        lt_runtime::db::query_issue_by_id(&conn, self.issue.id.inner())
                {
                    self.issue = fresh;
                }
            }
            _ => {}
        }
    }
}
```

- **Precision is structural.** A closed view does not exist, so no "am I
  displayed?" checks survive anywhere; the only remaining checks are
  id-relevance (`Comments{A}` vs the detail's own issue id, `Team{T}` vs the
  popup's/modal's team) — data checks, not mode checks. On the list screen a
  team update is a no-op because there is no consumer for it, not because a
  consumer said no.
- **All live layers consume**, not just the top: every stacked view is visible
  (rendered bottom-up), so a `Comments` event must reach a Detail even with a
  popup on top of it, and an `Issues` from a popup confirm lets the Detail
  beneath re-read its displayed issue. Applies are payload-free idempotent
  re-reads, so multi-layer application cannot double-apply.
- **The base list's policy lives in its consumer**, not the router: today's
  sync-completion guard `matches!(app.mode, Mode::List)` (`sync.rs:109`) becomes
  `views.is_empty()` inside `list_consume`.
- **Same-thread optimistic writers call `route_state_event` directly** — a
  function call, not a channel round-trip; same frame, zero latency, one code
  path with the async completions. `submit_comment` keeps the transactional
  enqueue (`enqueue_comment_create` already writes the optimistic `local:` row)
  and deletes the hand-built in-memory push (`detail.rs:147-160`);
  `popup_confirm` keeps `enqueue_edit` and deletes
  `apply_optimistic_in_memory`/`build_optimistic_issue` — the bespoke second
  read model. `new_issue_submit` keeps `do_fetch_and_select` (a DB re-read plus
  a selection seek — view logic the writer owns).
- **Lifecycle outcomes are consumed by an `App` method over `self.hooks`.** They
  write identity, `session.not_authenticated`, `status`, and scheduling —
  App-wide state — and the sync `Done` arm feeds
  `route_state_event(&StateEvent::Issues)`. `Hooks` groups today's `SyncState`
  with the `login_in_flight` gate.
- **Borrow mechanics, verified against the consume bodies**: `&self.db` (shared)
  and `for view in &mut self.views` are disjoint field borrows in one function
  body. It holds because no `consume` body touches App-level state: Detail
  writes only its own comments/issue, the modal only its picker fields ("Me (…)"
  resolution uses the persisted `db::synced_viewer`, not a service call), the
  popup only its items/selection. Nothing in the State path spawns work — spawns
  happen in key handlers, which take `&mut App`. `&Database` is the entire
  context a `consume` needs; a ctx struct waits until a second dependency
  actually appears.

Two deviations from the review sketch, disclosed:

- **Key handlers stay `fn(&mut App, ...)`, routed by the top view's
  discriminant**, rather than a disjoint `view.consume(key)`: confirms mutate
  App-wide state (`popup_confirm` writes the DB and refreshes the list;
  `confirm_search` moves results into `app.issues`), and handlers that consume
  their view pop it first. The keymap design's `dispatch_key` is `app.keys` when
  it lands; the routing seam is identical.
- **`hooks.consume` is honored as a grouping, not a signature**: lifecycle
  outcomes touch identity and session, so the consumer is an `App` method over
  the `Hooks` field.

Rejected forms:

| Option                                               | Why rejected                                                                                                     |
| ---------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| Broadcast to every view module (previous draft)      | every possible view is asked "are you displayed?" — the stack answers by existence; closed views cannot be asked |
| Central scope→consumer match                         | a registry that grows with every scope-consumer pair, away from the components that own the dependency           |
| Runtime subscription registry (`Vec<(pattern, fn)>`) | the subscriber set is the live stack itself; a registry re-encodes it as data and loses compile-time visibility  |
| Relay-proper: dependencies derived from queries      | needs queries-as-data plus dependency tracking over SQLite — a reactive framework; speculative at this size      |

## Decision 4: team-scoped cache — schema, targeted sync, trait diet

### Schema (`MIGRATION_2`)

Appended to `migrations()` (`lt-storage/src/db/mod.rs:132-133`);
`rusqlite_migration` upgrades existing databases in place.

```sql
ALTER TABLE workflow_states ADD COLUMN team_id TEXT;
ALTER TABLE workflow_states ADD COLUMN position REAL;
CREATE INDEX idx_workflow_states_team_id ON workflow_states (team_id);
CREATE TABLE team_memberships (
    team_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    PRIMARY KEY (team_id, user_id)
);
```

- `workflow_states` gains columns rather than a parallel table: a Linear
  workflow state belongs to exactly one team, the table already has the right
  id/name shape, and the issue read-model joins keep working untouched.
- `position` is stored so the state picker shows Linear's Backlog → Todo → …
  order rather than alphabetical. The richer fetch is a new cynic fragment in
  `lt-types` (id, name, position) used only by the team-states query; the shared
  `WorkflowState { id, name }` in the issue fragment is unchanged.
- `teams` already has the right shape (`mod.rs:96`).

New registered statements in `sql.rs` (covered by the existing `sql_validation`
gate): a scoped workflow-state upsert whose
`position = COALESCE(excluded.position, workflow_states.position)` lets
issue-driven upserts pass `NULL` without clobbering a synced position;
`QUERY_TEAMS`; `QUERY_TEAM_STATES` (ordered `position IS NULL, position, name`);
`QUERY_TEAM_MEMBERS` (join through `team_memberships`); and delete-then-insert
membership statements with replace-set semantics.

**Issue upserts scope states for free**: `upsert_issue_tx`
(`lt-storage/src/db/issues.rs:130-199`) knows the state's team
(`issue.team.id`), so its workflow-state arm switches to the scoped upsert with
`position = NULL`. Every full/delta sync back-fills `team_id` for all states in
use — the pickers work offline after any ordinary sync, before a targeted
refresh ever runs. Memberships are **not** inferred from issues (an assignee is
not provably a member); only `sync_team_data` writes them.

### Targeted sync, mirroring `sync_comments`

New module `lt-runtime/src/teams.rs`, shaped like `comments.rs` and tested the
same way (`FakeTransport`, offline):

```rust
pub fn sync_teams(conn: &Connection, transport: &dyn GraphqlTransport) -> Result<()>;
/// States: scoped upsert (id, name, team_id, position). Memberships:
/// delete-then-insert the fetched set in one transaction. Users are upserted
/// so the membership join resolves names.
pub fn sync_team_data(conn: &Connection, transport: &dyn GraphqlTransport, team_id: &str) -> Result<()>;
```

Team metadata is **not** added to full/delta sync: syncing states + members for
every team is an N+1 query fan-out whose only consumers are the pickers, and the
pickers refresh themselves on open and team-change. (States removed from a team
linger until unreferenced — accepted; deleting them would break the issue
read-model joins for archived states.)

### The trait after this design

```rust
// crates/lt-runtime/src/sync/service.rs
/// Invoked exactly once with the outcome of a spawned background job.
pub type OnSync = Box<dyn FnOnce(SyncEvent) + Send + 'static>;
pub type OnLogin = Box<dyn FnOnce(LoginEvent) + Send + 'static>;

pub trait SyncService: Send + Sync {
    /// Spawn a background sync (full or delta); `on_done` is invoked exactly
    /// once with the outcome, even if the sync body panics.
    fn spawn_sync(&self, full: bool, fetch_identity: bool, on_done: OnSync);
    /// Spawn the background OAuth login flow; same completion contract.
    fn spawn_login(&self, on_done: OnLogin);
    /// Startup header identity. Unchanged.
    fn fetch_viewer(&self) -> Option<viewer::User>;
    /// API -> DB writers; callers re-read via a StateEvent.
    fn sync_comments(&self, issue_id: &str) -> Result<()>;
    fn sync_teams(&self) -> Result<()>;
    fn sync_team_data(&self, team_id: &str) -> Result<()>;
}
```

- `spawn_sync`/`spawn_login` stop returning receivers and take completion
  callbacks — the trait cannot name `AppEvent` (`lt-runtime` must not depend on
  `lt-tui`), so the TUI passes a closure that wraps into
  `AppEvent::Lifecycle(Sync(ev))` / `Lifecycle(Login(ev))` and sends on
  `events_tx`. `FnOnce` is the honest type: both jobs send exactly one event.
- The dead `query: IssueQuery` parameter is dropped: the concrete impl binds it
  `_query` (`adapter.rs:43`) and every caller clones `app.args` into it for
  nothing; keeping it would also push the new signature past clippy's
  `too-many-arguments-threshold` of 4.
- `fetch_teams` / `fetch_workflow_states` / `fetch_team_members` **die** (all
  call sites: `new_issue.rs:118,173,193`, `popup.rs:234,277`, plus
  `NoopSyncService`). The modal's `fetch_viewer` call (`new_issue.rs:170`) dies
  too: "Me (…)" resolution uses the persisted `db::synced_viewer`
  (`lt-storage/src/db/issues.rs:484`) at consume time.

Rejected alternatives for the spawn signatures:

| Option                                             | Why rejected                                                                                                       |
| -------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| Keep returning `Receiver`, fan in with TUI threads | one extra thread per job existing purely to move a value between channels                                          |
| Move the trait into `lt-tui`                       | forces `LinearSyncService`, and with it `lt-upstream`/cynic, into `lt-tui` — defeats the seam the trait exists for |
| Make the trait generic over an event mapper        | loses object safety; `Arc<dyn SyncService>` is how `lt-cli` injects it (`lt-cli/src/main.rs:73`)                   |
| Pass `Sender<AppEvent>` into the trait             | `lt-runtime` would depend on a TUI type; the same inversion as moving the trait, in disguise                       |

### Sim compatibility

`sim`-seeded data flows through the issue upserts, so states get `team_id` for
free. `team_memberships` is populated by a registered derivation statement run
by `lt sim` (distinct team/assignee and team/creator pairs from the seeded
issues), keeping the pickers drivable offline per [[dst.md]].
`NoopSyncService`'s new sync methods return `Ok(())`.

## Decision 5: cache-first pickers

Cross-thread producers use one helper; the invalidation is sent **even on
failure** ("the refresh attempt finished; re-read whatever is cached"), which
clears the modal's `loading` flag deterministically and is why no error variant
exists. Per-fetch error display dies deliberately: offline, every targeted
refresh fails, and per-fetch error text (`ModalEvent::LoadError`, the popup
footer messages at `popup.rs:252,287`) would be constant noise in a local-first
app; failures go to `tracing`, and the global sync label already covers "not
authenticated" / "sync error". (`CommentSyncEvent::Error`'s handling was already
a silent no-op, `detail.rs:223-225`.)

```rust
impl App {
    /// Run `job` on a background thread, then always send `State(ev)`.
    /// Refresh failures are expected offline: logged, cache kept.
    fn spawn_state_refresh(
        &self,
        ev: StateEvent,
        job: impl FnOnce(&dyn SyncService) -> Result<()> + Send + 'static,
    ) {
        let service = Arc::clone(&self.service);
        let tx = self.events_tx.clone();
        std::thread::spawn(move || {
            if let Err(e) = job(service.as_ref()) {
                tracing::warn!("background refresh failed: {e:#}");
            }
            let _ = tx.send(AppEvent::State(ev));
        });
    }
}
```

`open_detail`'s worker becomes `sync_comments(&issue_id)` → send
`State(Comments { issue_id })`. This also fixes a test wart: the worker's
post-sync re-read today opens the real profile DB via `db_path()`
(`detail.rs:57-60`) even when tests install an in-memory database; all reads now
go through `app.db` at consume time.

**New-issue modal** (`new_issue.rs`):

- Open: build `teams` from `query_teams(app.db)` (instant; the synchronous
  network fetch dies), preselect from `args.team` as today, read
  `query_team_states`/`query_team_members` for the preselected team, set
  `loading = true`, then `spawn_state_refresh(Teams, |s| s.sync_teams())` and —
  when a team is selected — `spawn_state_refresh(Team { team_id }, …)`.
- Leaving the Team field: instant cache read for the newly selected team plus
  one `spawn_state_refresh(Team { .. })`. `PopupItem` construction moves from
  the worker thread to consume-time DB reads (`build_assignee_items` survives,
  fed by `synced_viewer` + `query_team_members`).
- `NewIssueModal::consume`: on `Teams`, re-read teams and re-anchor the
  selection by team id (fallback index 0); on `Team { team_id }` — guarded by
  `selected_team_id() == team_id`, a new helper deduplicating the lookup at
  `new_issue.rs:154-160` and `219-224` — re-read states/members, preserve the
  user's picks by item id (today a refresh resets them to 0), and clear
  `loading`. `error` remains for submit validation only.

**State/assignee popups** (`popup.rs:228-303`) migrate to the same pattern —
they are the other two `fetch_*` callers and today block the UI thread on the
network: open reads the cache for the target issue's team, captures it as
`PopupView.team_id`, and spawns `sync_team_data`; `PopupView::consume` on a
matching `Team { team_id }` rebuilds `items` and re-anchors the selection. The
priority popup is static (`team_id: None`) and untouched.

## Decision 6: sender lifecycle and worker-panic recovery

`App` holds `events_tx` forever, so the queue never disconnects and every
`Disconnected` arm dies. What those arms did today:

| Poller                       | `Disconnected` behavior today                         | Subsumed by                                         |
| ---------------------------- | ----------------------------------------------------- | --------------------------------------------------- |
| `poll_sync_events`           | clear `syncing`, repair label (`sync.rs:143-150`)     | adapter panic guard (below)                         |
| `poll_login_events`          | clear `login_rx` (`sync.rs:82-84`)                    | adapter panic guard (below)                         |
| `poll_detail_comment_events` | drop rx; no state change (`detail.rs:228`)            | `spawn_state_refresh` always sends the invalidation |
| `poll_modal_events`          | none — `loading` already sticks on worker panic today | `spawn_state_refresh` always sends the invalidation |

The real hazard is sync/login: today a panicking worker drops its `tx`,
`Disconnected` fires, and `syncing`/`login_rx` recover. With a shared sender a
panicked worker sends nothing, `syncing` sticks at `true` forever, and the 30s
periodic sync never reschedules. Panics are denied in workspace code, but
dependencies can still panic. So the trait's "invoked exactly once, even if the
body panics" contract is implemented in `LinearSyncService` with `catch_unwind`:

```rust
// crates/lt-runtime/src/adapter.rs (shape; likewise for spawn_login)
fn spawn_sync(&self, full: bool, fetch_identity: bool, on_done: OnSync) {
    std::thread::spawn(move || {
        let event = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_sync_body(full, fetch_identity)
        }))
        .unwrap_or_else(|_| SyncEvent::Error("sync worker panicked".to_string()));
        on_done(event);
    });
}
```

Strictly better than today: the user sees `sync error: ...` and the 30s retry
keeps running, instead of a silent label repair. `spawn_state_refresh` workers
need no guard — the invalidation-on-completion already covers the failure path,
and a panic between the API call and the send costs only one refresh.

In-flight gates live in `Hooks`: `syncing` (plus a `!syncing` guard added to the
login-success sync spawn, which today replaces `sync_rx` unguarded at
`sync.rs:68-74`) and `login_in_flight` replacing `login_rx.is_some()` at the `L`
binding (`lib.rs:929`).

## Decision 7: keys through the queue — input thread, single-wait loop, `EventPump`

### Input thread and loop

```rust
// crates/lt-tui/src/lib.rs — called from run() after terminal setup;
// kitty enhancement handling (lib.rs:778-787) is unchanged.
fn spawn_input_thread(tx: mpsc::Sender<AppEvent>) {
    std::thread::spawn(move || loop {
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                if tx.send(AppEvent::Key(key)).is_err() {
                    return;
                }
            }
            Ok(_) => {}       // resize/mouse/release: dropped, as today
            Err(_) => return, // terminal gone
        }
    });
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    pump: &mut EventPump,
    app: &mut App,
) -> Result<()> {
    loop {
        maybe_start_periodic_sync(app); // 30s inline clock check, unchanged
        poll_search_debounce(app);      // 150ms inline clock check, unchanged

        terminal.draw(|frame| ui::render(frame, app))?;
        if app.quit {
            return Ok(());
        }

        // Block up to 100ms for the first event, then drain without blocking.
        if let Some(event) = pump.next(&app.events_rx, Duration::from_millis(100))? {
            app.apply(event);
        }
        while let Ok(event) = app.events_rx.try_recv() {
            app.apply(event);
        }
    }
}
```

- **Draw-then-wait preserves today's frame order exactly**: the first frame
  renders immediately, key latency is unchanged, and the ≤100ms tick keeps the
  debounce/periodic timers and terminal-resize pickup working with no dedicated
  events. The timers stay inline: they are clock predicates over loop-owned
  state, not channels; folding them in would require a producer thread whose
  only job is to watch a clock.
- **Quit / thread lifetime**: the input thread is detached and usually blocked
  in `event::read()`; it exits with the process (`lt-cli` returns immediately
  after `run()`) or on `send` failure once the app drops. Between
  `ratatui::restore()` and process exit it may consume at most one terminal
  event — harmless, accepted.
- **Disconnected is unreachable** in production (`App` owns a sender); the
  `Channel` arm treats it as an idle tick.

### Test seam: `EventPump` replaces `EventSource`

`EventSource`, `CrosstermEvents` (`lib.rs:73-91`), and `ScriptedEvents`
(`loop_tests.rs:19-30`) die. The seam moves up a level and keeps the
load-bearing exhaustion-as-error property:

```rust
/// Where the loop's blocking wait gets its first event each frame. A closed
/// set (cf. Clock and db::Database): the channel in the binary, a script in
/// tests.
enum EventPump {
    Channel,
    /// Scripted events for loop tests; errors when exhausted so a test that
    /// forgot to quit fails fast instead of hanging.
    #[cfg(all(test, feature = "sim"))]
    Scripted(VecDeque<AppEvent>),
}
```

`Scripted` supplies the first event each frame — typed `AppEvent`, so tests
script `Key`, `State`, and `Lifecycle` events interleaved. The unconditional
`try_recv` drain still runs afterwards, so events that consumers push onto the
real channel are seen in the same frame. Loop tests run thread-free and
deterministic.

## Scope relevance

With payload-free events, stale data cannot be applied — events carry none. With
the view stack, display checks cannot be forgotten — a closed view does not
exist. Drops happen in exactly two ways: no consumer exists, or an id-relevance
guard falls through inside a live consumer. Duplicate or late events are
idempotent re-reads of current truth.

| #   | Event at apply time                            | Stack contents                            | Handling                                                                             |
| --- | ---------------------------------------------- | ----------------------------------------- | ------------------------------------------------------------------------------------ |
| N1  | `State(Comments{A})`                           | `Detail(A)` anywhere in the stack         | consume re-reads `query_comments(A)` — even under a future popup                     |
| N2  | `State(Comments{A})`                           | no `Detail` / `Detail(B)`                 | no consumer exists / id mismatch falls through                                       |
| N3  | `State(Comments{A})` twice (fast close/reopen) | `Detail(A)`                               | both re-read; idempotent                                                             |
| N4  | `State(Teams)`                                 | `NewIssue` in the stack                   | re-read teams; re-anchor selected team by id                                         |
| N5  | `State(Teams)`                                 | no `NewIssue`                             | no consumer exists                                                                   |
| N6  | `State(Team{T})`                               | `NewIssue`, team T selected               | re-read states+members; preserve picks by id; clear `loading`                        |
| N7  | `State(Team{T})`                               | `NewIssue` on team U / no consumer        | id mismatch falls through / no consumer (U's own refresh is in flight)               |
| N8  | `State(Team{T})`                               | `Popup { team_id: Some(T) }` in the stack | rebuild `items`; re-anchor selection                                                 |
| N9  | `State(Issues)`                                | stack empty                               | base list `do_fetch(false)` (offset-preserving)                                      |
| N10 | `State(Issues)`                                | stack non-empty                           | base list's guard drops it; a live `Detail` re-reads its issue (`query_issue_by_id`) |
| N11 | `Lifecycle(Sync(_))`                           | any                                       | `hooks.syncing` gates spawns; `Done` feeds N9/N10 via `route_state_event`            |
| N12 | `Lifecycle(Login(_))`                          | any                                       | `hooks.login_in_flight` gates `L`; cleared by `consume_lifecycle`                    |

## Keymap design reconciliation

The keymap redesign ([PR #43](https://github.com/willruggiano/lt/pull/43)) and
this ADR are open concurrently; whichever lands second rebases its dispatch
seam. Its keymap core — `Key`/`Action`/`Binding`, contexts, tables, help
generation, no-timer chords — is entirely unaffected. What changes:

- Its assumption that the 100ms poll loop and `EventSource` are untouched no
  longer holds. Chords still need no timer: the pending prefix is `App` state
  and survives any number of idle frames of the `recv_timeout` loop.
- Its dispatch site becomes the `AppEvent::Key` arm:
  `AppEvent::Key(ev) => dispatch_key(app, Key::from_event(ev))`. The queue's
  wire type is the raw crossterm `KeyEvent`, not `keymap::Key`: normalization
  still happens exactly once, at the boundary between transport and keymap.
- `key_context` derives from the stack top instead of `Mode`, with sub-focus
  read from view-local fields: `None => List`; `Detail(d)` => `CommentInput` if
  `d.comment_input.is_some()` else `Detail`; `NewIssue(m)` => text vs picker by
  `m.focused_field`; `Popup`/`Search`/ `Help` map directly.
- `Action::Back` = `views.pop()` in every non-list context; the list's `Back`
  keeps the double-esc reset.
- **Its "popup return-mode" risk entry is resolved structurally** and should be
  deleted on rebase: confirm/cancel pop instead of writing `Mode::List`,
  restoring whatever is beneath. Phase 4's "s/p/a from Detail" pushes a
  `PopupView` built from the detail's own issue.
- Its loop-test harness reference (`ScriptedEvents`) becomes
  `EventPump::Scripted` with `AppEvent::Key(...)` entries; the same key
  sequences drive the same assertions.

## User-visible behavior changes

1. Modal open and the state/assignee popups no longer block the UI thread on the
   network — instant cache reads.
2. Picker data may be one refresh stale; mitigated by the targeted refresh on
   open and team-change. Cold cache + offline shows empty pickers instead of an
   error string; per-fetch error text is replaced by `tracing` + the global sync
   label.
3. The state picker sorts by Linear's stored `position` (states known only from
   issue upserts sort last by name until a targeted refresh records positions).
4. Optimistic edits re-read through the active filter: an edit that no longer
   matches (e.g. mark Done under `state:todo`) disappears immediately instead of
   lingering until the next refresh.
5. Sync completion refreshes the list on any page when the list has focus (was
   page-1 only, `sync.rs:109-114`); `do_fetch(false)` preserves the offset.
6. An open detail pane re-reads its issue when the issues scope changes (N10) —
   a popup edit or sync upsert is visible in the pane immediately.
7. The optimistic comment author comes from the persisted viewer rather than the
   in-memory `viewer_name`; it is absent before the first successful sync.
8. Worker panics surface as `sync error: ...` instead of a silent label repair.

The view-stack restructure itself (sprint PR 2) is behavior-neutral: render
snapshots must be pixel-identical, which is that PR's acceptance gate.

## Test migration

- **View-stack migration** (`render_tests.rs`, `loop_tests.rs`): every
  `app.mode = Mode::X; app.<field> = Some(...)` setup pair becomes one
  `app.views.push(View::X(...))`. `popup_move`/`popup_cancel` tests construct a
  `PopupView` and assert `views.is_empty()` after cancel;
  `close_detail_clears_pane_state` collapses to the same assertion. During the
  window between the restructure and the queue PR, the comment poller finds its
  receiver via the stack (`views.iter_mut().find_map(...)`). `confirm_search`
  pops the `Search` view before touching `app.issues` (the borrow requires it,
  and it destroys the overlay anyway); `poll_search_debounce` copies
  `viewport_height`/`args.limit` out before taking `views.last_mut()`.
- **Loop tests** (`loop_tests.rs`): `drive()` builds `EventPump::Scripted` from
  `AppEvent::Key(...)` entries; the exhaustion-as-error test survives with a
  one-line change. The per-poller channel tests become direct calls:
  `route_state_event` unit tests covering N1–N10 (live and absent consumers,
  matching and mismatched ids), `consume_lifecycle` tests, `login_in_flight` and
  login-success-guard tests. The `Disconnected` tests die with the state they
  exercised.
- **Storage**: migration validity is already covered by `migrations_are_valid`;
  add ordering/scoping tests for `query_team_states`, replace-set semantics for
  memberships, and "issue upsert back-fills `team_id` without clobbering
  `position`". New statements are covered by the existing `sql_validation` gate.
- **Runtime**: `teams.rs` sync fns tested with `FakeTransport`, mirroring the
  `comments.rs` tests — offline, sim-compatible.
- **`NoopSyncService`** (`lib.rs:282-314`): `spawn_*` become empty-body
  callbacks; `sync_teams`/`sync_team_data` return `Ok(())`; the three `fetch_*`
  methods are deleted.

## Delivery: stacked PRs (each green under `make test` + `make check`)

1. **`docs(design)`** — this document.
2. **`refactor(tui): view stack`** — `View` enum + per-variant structs,
   `views: Vec<View>`, `Mode` deleted, key router on `views.last()`, push/pop
   entry/exit, `submit_comment` reads the detail's own issue, the popup anchor
   write moves behind the sole-entry guard, dead `input_mode`/`input_buf` and
   their render branch deleted, `detail_comment_rx` moves into `DetailView` for
   the interim. The `sync.rs:109` guard becomes `views.is_empty()` **keeping the
   page-1 conditions** so this PR stays behavior-neutral (render snapshots
   pixel-identical). Test literals migrate. Independent of PR 3.
3. **`feat(storage,runtime): team-scoped cache and team metadata sync`** —
   `MIGRATION_2`, new statements and query/upsert helpers, `upsert_issue_tx`
   scoping, the `lt-types` position fragment, `lt-runtime/src/teams.rs`, the
   trait gains `sync_teams`/`sync_team_data` (Linear + Noop impls), sim
   membership derivation. `fetch_*` still present; TUI untouched.
4. **`refactor(tui): AppEvent queue and StateEvent routing`** —
   `AppEvent`/`StateEvent`, `events_tx`/`events_rx`, `App::apply`,
   `route_state_event` + `View::consume` (Detail's `Comments` and `Issues` arms,
   `list_consume`); the comment worker goes payload-free; `CommentSyncEvent` and
   `DetailView`'s interim receiver die; `submit_comment` and `popup_confirm`
   unify on re-reads (`apply_optimistic_in_memory`, `build_optimistic_issue`,
   and `selected_issue_mut` die). Coexists with the remaining pollers and
   `EventSource` keys. **Requires PR 2.**
5. **`refactor(tui,runtime): cache-first pickers`** — modal and popups read
   SQLite, `spawn_state_refresh`, `NewIssueModal::consume` and
   `PopupView::consume`; `ModalEvent`, `modal_rx`, and the three `fetch_*` trait
   methods die. Requires PRs 3 and 4.
6. **`refactor(runtime,tui): sync/login completion callbacks`** —
   `OnSync`/`OnLogin`, the dead `query` param dropped, the `catch_unwind` guard,
   `LifecycleEvent` + `consume_lifecycle`, `SyncState` → `Hooks` with
   `login_in_flight`, the login-success `!syncing` guard, and `App::start_sync`
   deduplicating the four spawn sites (`lib.rs:657-665`, `lib.rs:749`,
   `lib.rs:815-822`, `sync.rs:68-74`; `run()` reorders to construct `App` before
   the startup spawn — behavior-neutral). Requires PR 4; independent of PR 5.
7. **`refactor(tui): keys through the queue`** — input thread, `EventPump`, the
   `recv_timeout` loop; `EventSource`/`CrosstermEvents`/`ScriptedEvents` die;
   loop tests migrate. Requires PR 4; last, so the loop rewrite lands on a fully
   queue-fed app.

Ordering: 2 before 4; 3 before 5; 4 before 5, 6, and 7; 2∥3 and 5∥6 are
parallelizable. Keymap PR #43 phase 1 can land before or after PR 2 (a
one-function rebase either way); its phase 4 requires PR 2.

## Open questions

None blocking. Startup's synchronous `fetch_viewer` (`lib.rs:744`) still blocks
briefly before the TUI starts; reading `synced_viewer` from the cache instead is
a natural follow-up, deliberately out of scope here.
