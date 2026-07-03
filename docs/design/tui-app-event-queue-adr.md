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
                              Key: cascades    State: every   Lifecycle:
                              down the stack   live view      sync/auth
                              until consumed   consumes,      typestates,
                                               top-down       then Issues
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

## Decision 2: the view stack — a view exists iff it is displayed, and the base is `views[0]`

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
    /// The live view stack, bottom to top. Never empty: views[0] is the base
    /// view for this CLI invocation — today always the issue list; a future
    /// `lt tui --inbox` or `--projects` seeds a different base (the keymap
    /// design reserves `g i`/`g m`/`g t` for exactly those). The top view is
    /// focused; keys cascade down, StateEvents walk down, everything renders.
    pub views: Vec<View>,

    /// Background-job typestates (Decision 6): what is running, what is
    /// scheduled, who we are.
    pub sync: SyncStatus,
    pub auth: AuthStatus,

    // args, active_filter, initial_args/initial_filter, last_esc_time,
    // footer_msg, viewport_height, quit, session.keyboard_enhanced,
    // db/clock/service/events_tx/events_rx: App-wide, unchanged in kind.
}

/// One view's complete state. A view exists iff it is displayed; there is
/// no separate mode flag to keep consistent.
pub enum View {
    List(ListView),
    Detail(DetailView),
    Popup(PopupView),
    NewIssue(NewIssueModal), // shape unchanged (new_issue.rs:62)
    Search(SearchOverlay),   // shape unchanged (popup.rs:101)
    Help(HelpPopup),         // shape unchanged (popup.rs:61)
}

/// The issue-list view: today's loose base-list fields on App
/// (lib.rs:317-321), owned. `status` comes too: its only render site is the
/// base table's Loading/Error overlay (ui/table.rs:12-25), and its writers
/// are do_fetch (moving here) and the sync lifecycle's Loading->Idle repair
/// (which reaches the base through `base_list_mut`).
pub struct ListView {
    pub issues: Vec<Issue>,
    pub table_state: TableState,
    pub pagination: Pagination,
    pub status: Status,
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
```

- **A stack, not a slot.** Today's topology is a star — every overlay opens from
  and returns to the list — so `Option<View>` would suffice today. But the
  second layer is not speculative: the keymap design
  ([PR #43](https://github.com/willruggiano/lt/pull/43)) names "popup
  return-mode" as its phase-4 blocker (`popup_confirm`/`popup_cancel` hardcode
  `Mode::List`, `popup.rs:341,346`, so s/p/a popups cannot open from Detail).
  `Option<View>` plus a return-view field would recreate exactly the
  parallel-state smell this decision deletes. Pop restores whatever is beneath
  with its state intact.
- **The base is a view like any other.** Keeping the list outside the stack as
  loose `App` fields hardcodes _which_ view is the base; a future
  `lt tui --inbox` makes the base a parameter of the invocation. And the
  enforcement burden of "the loose fields are the base" (every renderer and
  consumer special-cases them) is heavier than the burden of "the stack is never
  empty" — one constructor and one pop helper:
  - `App::new`/`App::for_test` seed
    `views: vec![View::List(ListView::new(issues, pagination))]`.
  - Exactly one removal path exists:

  ```rust
  impl App {
      /// Pop the focused view. The stack is never empty: popping the base
      /// resets it to the default base view for this CLI invocation instead
      /// (today: the issue list rebuilt from initial_args/initial_filter —
      /// the same reset double-esc performs). No Back path reaches this
      /// branch today (the list's Esc is the double-esc reset,
      /// lib.rs:862-882, and never pops); the branch defines the semantics
      /// rather than defending against a bug.
      fn pop_view(&mut self) {
          if self.views.len() > 1 {
              self.views.pop();
          } else {
              self.reset_base_view();
          }
      }
  }
  ```

- **What stays on `App`, by ownership**: `args` (renderer reads the sort marker
  for the base table and the search overlay, `ui/table.rs:27` and
  `ui/mod.rs:146-149`; modal open reads the preset team, `new_issue.rs:98`),
  `active_filter`/`initial_*` (written by Search confirm, read by the header and
  both resets), `last_esc_time` (its reset mutates App-level filter state; cf.
  the keymap's `pending_key`), `footer_msg` (rendered in every status-row
  branch, written by lifecycle and submit paths), `viewport_height` (written by
  the renderer, read by three views). Everything the list alone reads and writes
  moves into `ListView`.
- **`do_fetch` moves onto `ListView` and gains a context.** Its dependency set
  is `db` + `args.limit` + `active_filter` + the viewer name for `resolve_me`
  (`lib.rs:571-624`) — so the previously deferred ctx struct arrives now
  (Decision 3). `cycle_sort`/`toggle_desc`/`refresh`/the double-esc reset stay
  `App` methods (they mutate `args`/`active_filter` or spawn syncs) that end by
  calling the base's `do_fetch`; `next_page`/`prev_page` and the selection
  helpers move onto `ListView`. Non-list writers reach the base through
  `fn base_list_mut(&mut self) -> Option<&mut ListView>`; `None` (a future
  non-list base) degrades those writes to no-ops, which is correct.
- **Entry is push, exit is `pop_view`.** `popup_confirm` becomes: pop the
  `PopupView`, `enqueue_edit(&p.issue_id, ...)`, route `StateEvent::Issues`
  (Decision 3). `confirm_search` pops the `Search` view first — taking ownership
  of the overlay, so the flush and the writes into `active_filter` and the
  base's issues/selection proceed with no overlapping borrows (strictly simpler
  than today's `mem::take` dance, `popup.rs:559-579`).
- **Latent smells die structurally**: `popup_items`/`popup_selected` are never
  cleared on close today (`popup.rs:341-348`) — now the whole `PopupView` drops;
  the dead `input_mode`/`input_buf` pair (`lib.rs:324-325`, unreachable render
  branch `ui/mod.rs:94-95`) is deleted; `submit_comment` stops reading the list
  selection (`detail.rs:140`) because `DetailView` owns its issue.
- **Popup anchoring**: the base-table renderer computes the anchor from column
  geometry (`table.rs:43-59`). It writes into the popup only when the popup sits
  directly on the base (`[View::List(_), View::Popup(p)]` slice pattern); for a
  future popup over Detail the anchor stays `None` and `render_popup` already
  centers.
- The renderer draws the stack bottom-up — `views[0]` is the full-frame base
  layer, replacing both the unconditional table render (`ui/mod.rs:59`) and the
  six mode-checked blocks in `render_overlays` (`ui/mod.rs:104-152`).
  `render_status_row`'s Detail branch and the header's Search branch match on
  the stack top instead of `Mode`.

## Decision 3: routing — keys cascade, state walks the stack top-down

```rust
// crates/lt-tui/src/lib.rs
/// What a key handler did with a key. Pass hands it to the next view down; a
/// handler that returns Pass must not have mutated anything (in particular
/// the stack), so the walk's indices stay valid.
pub enum KeyFlow {
    Consumed,
    Pass,
}

type KeyHandler = fn(&mut App, usize, KeyEvent) -> KeyFlow;

impl App {
    pub fn apply(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.dispatch_key(key),
            AppEvent::State(ev) => self.route_state_event(&ev),
            AppEvent::Lifecycle(ev) => self.consume_lifecycle(ev),
        }
    }

    /// Keys go to the focused view and cascade toward the base: an unbound
    /// key falls through to the view beneath, with views[0] as the floor.
    /// This is what makes list-level bindings reachable from overlays with
    /// no "global" layer. The handler is picked by discriminant and
    /// re-fetches its view by index — it takes &mut App, so no view borrow
    /// may be held across the call.
    fn dispatch_key(&mut self, key: KeyEvent) {
        for i in (0..self.views.len()).rev() {
            let handler: KeyHandler = match &self.views[i] {
                View::List(_) => handle_list_key,
                View::Detail(_) => detail::handle_key,
                View::Popup(_) => popup::handle_key,
                View::NewIssue(_) => new_issue::handle_key,
                View::Search(_) => popup::handle_search_key,
                View::Help(_) => popup::handle_help_key,
            };
            if matches!(handler(self, i, key), KeyFlow::Consumed) {
                return;
            }
        }
    }

    /// Route a state invalidation down the stack, top first. Applies are
    /// idempotent payload-free re-reads, so the order is semantically
    /// irrelevant; top-down is chosen for coherence with the key cascade —
    /// one direction to reason about. The base list is just views[0]'s
    /// consumer.
    fn route_state_event(&mut self, ev: &StateEvent) {
        let ctx = StateCtx {
            db: &self.db,
            args: &self.args,
            filter: &self.active_filter,
            viewer_name: self.auth.viewer_name(),
        };
        let len = self.views.len();
        for (i, view) in self.views.iter_mut().enumerate().rev() {
            view.consume(&ctx, i + 1 == len, ev);
        }
    }
}

/// Read-only context a view's consume/re-query needs. The base list's
/// re-read has four dependencies, not one — the earlier "&Database until a
/// second dependency appears" deferral ends here (deviation from the prior
/// draft, disclosed): do_fetch reads db, args.limit, the active filter, and
/// the viewer name for `assignee:me` resolution (lib.rs:571-624).
pub struct StateCtx<'a> {
    pub db: &'a lt_runtime::db::Database,
    pub args: &'a IssueQuery,
    pub filter: &'a search_query::QueryAst,
    pub viewer_name: Option<&'a str>,
}

impl ListView {
    /// The base list's subscription: Issues, only while focused — the
    /// don't-clobber policy (sync.rs:109) expressed as `focused` instead of
    /// a mode check: a refresh must not swap the rows a popup is anchored
    /// to or a search overlay was opened over.
    fn consume(&mut self, ctx: &StateCtx, focused: bool, ev: &StateEvent) {
        if matches!(ev, StateEvent::Issues) && focused {
            self.do_fetch(ctx, false); // offset- and selection-preserving
        }
    }
}
```

`StateCtx` is built inline from disjoint field borrows at the call site — an
`App::state_ctx(&self)` accessor would borrow all of `self` and conflict with
`&mut self.views`. Detail/NewIssue/Popup consumers take the same `&StateCtx` and
use only `.db`.

A representative consumer — the per-view `consume` match arms are the view's
declared dependencies; irrelevant scopes fall through:

```rust
impl DetailView {
    fn consume(&mut self, ctx: &StateCtx, _focused: bool, ev: &StateEvent) {
        match ev {
            StateEvent::Comments { issue_id } if issue_id == self.issue.id.inner() => {
                if let Ok(conn) = ctx.db.connect()
                    && let Ok(comments) = lt_runtime::db::query_comments(&conn, issue_id)
                {
                    self.comments = comments;
                }
            }
            StateEvent::Issues => {
                // The displayed issue may be what changed (a popup edit
                // confirmed above this pane, or a sync upsert).
                if let Ok(conn) = ctx.db.connect()
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
  popup's/modal's team) and the base's `focused` policy. On the list screen a
  team update is a no-op because there is no consumer for it, not because a
  consumer said no.
- **All live layers consume state**, not just the top: every stacked view is
  visible (rendered bottom-up), so a `Comments` event must reach a Detail even
  with a popup on top of it, and an `Issues` from a popup confirm lets the
  Detail beneath re-read its displayed issue. Applies are payload-free
  idempotent re-reads, so multi-layer application cannot double-apply.
- **Text contexts terminate the key cascade by construction**: Search, Help, the
  comment input, and the new-issue text fields forward every unbound key to
  their editor and return `Consumed` — printable input can never fall through a
  text field. The new-issue picker fields also consume unbound keys: the modal
  is a form, and a stray letter acting on a view underneath it would be hostile.
  The pass-through contexts are Detail (outside the comment input) and Popup.
- **Disclosed: the q-leak.** With cascade, an unbound overlay key reaches the
  base table — including `q` = Quit (`lib.rs:861`); today `handle_popup_key`
  ignores unbound keys (`popup.rs:441-449`), and the keymap design rejected a
  global `q` for exactly this hazard. The cascade re-creates it as a _table_
  decision rather than a layering one: pass-through overlays bind `q` = Back
  (Detail already does, `detail.rs:258`; Popup gains it). The tables own the
  policy; this ADR owns only the mechanism.
- **Disclosed: the motivating example is shadowed.** "Detail open, press `c` to
  create" collides with the keymap design's Detail `c` = Comment binding. The
  cascade makes list bindings _reachable_ from overlays; whether a specific key
  reaches them is decided by the overlay's table (a key bound above never
  cascades). Deferred to the keymap tables.
- **Same-thread optimistic writers call `route_state_event` directly** — a
  function call, not a channel round-trip; same frame, zero latency, one code
  path with the async completions. `submit_comment` keeps the transactional
  enqueue (`enqueue_comment_create` already writes the optimistic `local:` row)
  and deletes the hand-built in-memory push (`detail.rs:147-160`);
  `popup_confirm` keeps `enqueue_edit` and deletes
  `apply_optimistic_in_memory`/`build_optimistic_issue` — the bespoke second
  read model. `new_issue_submit` keeps `do_fetch_and_select` (a DB re-read plus
  a selection seek — view logic the writer owns).
- **Lifecycle outcomes transition the Decision 6 typestates** via one
  `consume_lifecycle` App method; the sync `Done` arm feeds
  `route_state_event(&StateEvent::Issues)`.
- **Borrow mechanics, verified against the consume bodies**: the `StateCtx`
  field borrows and `for view in &mut self.views` are disjoint in one function
  body. It holds because no `consume` body touches App-level state: Detail
  writes only its own comments/issue, the modal only its picker fields ("Me (…)"
  resolution uses the persisted `db::synced_viewer`, not a service call), the
  popup only its items/selection, the list only its own rows. Nothing in the
  State path spawns work — spawns happen in key handlers, which take `&mut App`.
  For the key cascade, the handler is bound as a fn pointer inside the
  discriminant match and called after it — no view borrow crosses the `&mut App`
  call; handlers re-fetch their view by index.

Two deviations from the review sketch, disclosed:

- **Key handlers stay `fn(&mut App, usize, KeyEvent) -> KeyFlow`**, routed and
  cascaded by discriminant, rather than a disjoint `view.consume(key)`: confirms
  mutate App-wide state (`popup_confirm` writes the DB and refreshes the list;
  `confirm_search` moves results into the base), and handlers that consume their
  view pop it first. The keymap design's `dispatch_key` is `app.keys` when it
  lands; the routing seam is identical.
- **`hooks.consume` is honored as a grouping, not a signature**: lifecycle
  outcomes touch identity and scheduling, so the consumer is an `App` method
  over the `sync`/`auth` fields (Decision 6).

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

## Decision 6: lifecycle typestates and worker-panic recovery

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

### Sync and auth are typestates

`SyncState` (lib.rs:258-267), `login_rx` (lib.rs:395),
`session.not_authenticated` (lib.rs:276), and `viewer_name`/`org_name`
(lib.rs:383-385) are replaced by two typestate enums — two direct `App` fields,
not a wrapper struct: no invariant spans them, and the enums already do the
grouping a `Hooks` struct would only sketch. One `consume_lifecycle` App method
transitions both.

```rust
/// Background-sync typestate. The footer label is derived state and no
/// longer stored: it is formatted at render time from (SyncStatus,
/// AuthStatus, Clock). The pure format_sync_label (sync.rs:24) survives with
/// a narrowed signature — the never-synced and parse-error branches are
/// owned by other variants now.
pub enum SyncStatus {
    /// Nothing running, nothing scheduled. Entered only via NotAuthenticated
    /// today (sync.rs:139 is the sole next_sync_at = None writer); also the
    /// honest "not synced" pre-state.
    Idle,
    /// A sync worker is in flight; gates every spawn site.
    Syncing,
    Synced {
        synced_at: chrono::DateTime<chrono::Utc>,
        next_sync_at: Instant,
    },
    Failed {
        message: String,
        next_sync_at: Instant,
    },
}

/// Authentication typestate. Deviation from the review sketch, disclosed:
/// `Authenticated { token }` becomes `Authenticated { viewer }` — the TUI
/// never holds tokens (they live in lt-config/lt-upstream behind the
/// SyncService seam); its witness of authentication is the viewer identity,
/// which absorbs viewer_name/org_name.
pub enum AuthStatus {
    /// The startup identity fetch failed but a token may exist; the
    /// in-flight startup sync resolves this. Not Unauthenticated: the
    /// periodic-retry gate must not block a token-holding user who is
    /// merely offline (fetch_viewer None + first sync Error).
    Unknown,
    /// The OAuth login flow is in flight; gates 'L'.
    Authenticating,
    Authenticated { viewer: viewer::User },
    /// The sync layer reported no stored token.
    Unauthenticated,
    /// The last login attempt failed.
    Failed { message: String },
}
```

- **`Idle` over the sketch's `Option<Instant>`**: the only producer of an absent
  schedule is the unauthenticated path (`sync.rs:139`), where `synced_at` and
  `message` are also absent — an `Option` would smear one state across two
  variants' optional fields; `Idle` names it and makes `next_sync_at` total in
  the variants that schedule.
- **`synced_at` source**: read from the DB meta `last_synced_at` at the `Done`
  transition (every successful sync writes it, `lt-runtime/src/sync/mod.rs:47`),
  falling back to `clock.now()` — exact, since the sync finished at that
  instant. Startup always enters `Syncing` (`lib.rs:729-738`), so `Synced` is
  only ever displayed after a `Done` this session.
- **The label is derived at render** from `(SyncStatus, AuthStatus, Clock)`:
  `Authenticating` → "logging in…"; `Unauthenticated`/`Failed` auth → "not
  authenticated -- press L to log in"; otherwise `Idle` → "not synced",
  `Syncing` → "syncing...", `Synced` → the elapsed-minutes label, sync `Failed`
  → "sync error: …". `sync_status_label` and `build_sync_status_label`'s
  stored-string protocol die.

Every transition, mapped against today's writers:

| Trigger (today's site)                     | Transition                                                                                                                                                                                 |
| ------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| startup `run()` (`lib.rs:744-772`)         | `fetch_viewer()` Some → `Authenticated { viewer }`, None → `Unknown`; spawn startup sync → `sync = Syncing`                                                                                |
| `Sync(Done(viewer))` (`sync.rs:98-119`)    | Some(v) → `auth = Authenticated { v }` (None leaves auth unchanged — identity wasn't requested); `sync = Synced { synced_at, next_sync_at: now + 30s }`; then `route_state_event(&Issues)` |
| `Sync(Error(msg))` (`sync.rs:121-129`)     | `sync = Failed { message, next_sync_at: now + 30s }`; base list Loading→Idle repair via `base_list_mut`                                                                                    |
| `Sync(NotAuthenticated)` (`sync.rs:131`)   | `auth = Unauthenticated`; `sync = Idle`; same Loading repair                                                                                                                               |
| `Login(Success { viewer })` (`sync.rs:56`) | Some → `Authenticated`, None → `Unknown`; if `!matches!(sync, Syncing)` spawn delta with `fetch_identity = !matches!(auth, Authenticated { .. })` → `sync = Syncing`                       |
| `Login(Error(msg))` (`sync.rs:76-80`)      | `auth = Failed { message }`; `footer_msg` stays a direct transient write (deriving it from `Failed` would pin the message past the actions that clear it today)                            |
| `L` key (`lib.rs:929-933`)                 | gate `!matches!(auth, Authenticating)` (replaces `login_rx.is_none()`); → `Authenticating`, spawn login                                                                                    |
| `r` refresh (`lib.rs:653-665`)             | gate `!matches!(sync, Syncing)`; → `Syncing` (full)                                                                                                                                        |

The periodic gate (`lib.rs:810-823`, today
`!syncing && !not_authenticated && next_sync_at elapsed`) rewrites as: due iff
`sync` is `Synced`/`Failed` with `next_sync_at` elapsed, and `auth` is not
`Unauthenticated`/`Failed`;
`fetch_identity = !matches!(auth, Authenticated { .. })` replaces
`viewer_name.is_none()`.

`LoginEvent::Success` changes to carry `Option<viewer::User>` — the adapter
already has the full identity and discards its id (`adapter.rs:93-98`); the
spawn signatures are rewritten in the same PR anyway.

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
exist. Drops happen in exactly three ways: no consumer exists, an id-relevance
guard falls through inside a live consumer, or the base's `focused` policy
declines. Duplicate or late events are idempotent re-reads of current truth.

| #   | Event at apply time                            | Stack contents                            | Handling                                                                                                                        |
| --- | ---------------------------------------------- | ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| N1  | `State(Comments{A})`                           | `Detail(A)` anywhere in the stack         | consume re-reads `query_comments(A)` — even under a future popup                                                                |
| N2  | `State(Comments{A})`                           | no `Detail` / `Detail(B)`                 | no consumer exists / id mismatch falls through                                                                                  |
| N3  | `State(Comments{A})` twice (fast close/reopen) | `Detail(A)`                               | both re-read; idempotent                                                                                                        |
| N4  | `State(Teams)`                                 | `NewIssue` in the stack                   | re-read teams; re-anchor selected team by id                                                                                    |
| N5  | `State(Teams)`                                 | no `NewIssue`                             | no consumer exists                                                                                                              |
| N6  | `State(Team{T})`                               | `NewIssue`, team T selected               | re-read states+members; preserve picks by id; clear `loading`                                                                   |
| N7  | `State(Team{T})`                               | `NewIssue` on team U / no consumer        | id mismatch falls through / no consumer (U's own refresh is in flight)                                                          |
| N8  | `State(Team{T})`                               | `Popup { team_id: Some(T) }` in the stack | rebuild `items`; re-anchor selection                                                                                            |
| N9  | `State(Issues)`                                | `[List]` — base focused                   | `ListView::consume` runs `do_fetch(ctx, false)` (offset-preserving)                                                             |
| N10 | `State(Issues)`                                | overlay(s) above the base                 | base's `focused` guard drops it; a live `Detail` re-reads its issue (`query_issue_by_id`)                                       |
| N11 | `Lifecycle(Sync(_))`                           | any                                       | `matches!(sync, Syncing)` gates every spawn site; `Done` → `Synced { .. }` and feeds N9/N10                                     |
| N12 | `Lifecycle(Login(_))`                          | any                                       | `matches!(auth, Authenticating)` gates `L`; `consume_lifecycle` transitions `auth`                                              |
| N13 | `Key(k)`, unbound in the focused view          | pass-through overlay atop base            | `Pass` cascades toward `views[0]`; the base's handler is the floor. Overlays bind `q` = Back so `q` never falls through to Quit |
| N14 | `Key(k)` in a text context                     | any                                       | forwarded to the editor widget and `Consumed` — printable input never cascades                                                  |

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
- `key_context` becomes a stack walk, not a single derivation: resolve against
  the focused view's context first (sub-focus rules unchanged: `Detail(d)` =>
  `CommentInput` iff `d.comment_input.is_some()`; `NewIssue(m)` by
  `m.focused_field`); `Resolved::Unbound` in a pass-through context continues to
  the next view down, ending at `views[0]`'s context. `resolve`'s signature is
  unchanged; the cascade is dispatch-loop behavior above it.
- **GLOBAL survives the cascade**: the cascade delivers _keys_ downward, but
  GLOBAL delivers per-context _semantics_ for the same key (`j` scrolls in
  Detail, moves the selection in List) — so it remains a resolution layer within
  each view. Merging it into the tables is a keymap editorial choice with no
  mechanical consequence.
- `Action::Back` = `App::pop_view` in every non-base context; the base's `Back`
  keeps the double-esc reset — the same reset `pop_view` performs at the floor,
  so the two Back semantics converge on one helper.
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
9. "full sync..." (`lib.rs:659`) folds into "syncing..." — the distinction was a
   hardcoded string with no behavioral difference.
10. Identity is state, not residue: on `NotAuthenticated` after a session had
    identity (token revoked mid-session), the header shows "(not authenticated)"
    instead of the stale names; `assignee:me` resolution likewise requires a
    live `Authenticated`. Pressing `L` while authenticated blanks the header for
    the duration of the login — re-auth is a deliberate act.
11. After a failed login, periodic sync pauses until re-auth (today the
    press-L-while-authenticated corner could keep syncing under a label that
    claimed "not authenticated").

The view-stack restructure itself (sprint PR 2) is behavior-neutral: render
snapshots must be pixel-identical, which is that PR's acceptance gate.

## Test migration

- **View-stack migration** (`render_tests.rs`, `loop_tests.rs`): every
  `app.mode = Mode::X; app.<field> = Some(...)` setup pair becomes one
  `app.views.push(View::X(...))`; direct base-field setups (`app.issues`,
  `app.table_state`, `app.pagination`, `app.status`) go through
  `base_list_mut()` (or a test-only infallible `list_mut()`).
  `popup_move`/`popup_cancel` tests construct a `PopupView` and assert
  `views.len() == 1` after cancel; `close_detail_clears_pane_state` collapses to
  the same assertion. During the window between the restructure and the queue
  PR, the comment poller finds its receiver via the stack
  (`views.iter_mut().find_map(...)`). `confirm_search` pops the `Search` view
  before touching the base (the borrow requires it, and it destroys the overlay
  anyway); `poll_search_debounce` copies `viewport_height`/`args.limit` out
  before taking `views.last_mut()`.
- **Loop tests** (`loop_tests.rs`): `drive()` builds `EventPump::Scripted` from
  `AppEvent::Key(...)` entries; the exhaustion-as-error test survives with a
  one-line change. The per-poller channel tests become direct calls:
  `route_state_event` unit tests covering N1–N10 (live and absent consumers,
  matching and mismatched ids), cascade tests for N13/N14 (an unbound key in a
  popup reaches the base handler; a bound key stops at the popup; a printable
  key in Search never cascades), `consume_lifecycle` tests for the transition
  table, and the `L`/refresh/periodic gates against the typestates. The
  `Disconnected` tests die with the state they exercised.
  `app.viewer_name = Some(..)` fixtures become
  `app.auth = AuthStatus::Authenticated { .. }`.
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
2. **`refactor(tui): view stack`** — `View` enum + per-variant structs including
   `ListView` (issues/table*state/pagination/status move; `do_fetch` becomes
   `ListView::do_fetch(&StateCtx, bool)` with the ctx built inline; the viewer
   name is still read from the `App` field until PR 6), `views: Vec<View>`
   seeded in `App::new`/`for_test`, `Mode` deleted, `pop_view` replaces every
   `Mode::List` restoration write, `base_list_mut`/`selected_issue` accessors,
   `submit_comment` reads the detail's own issue, the popup anchor write moves
   behind the `[View::List(*),
   View::Popup(p)]`guard, dead`input_mode`/`input_buf`and their render branch deleted,`detail_comment_rx`moves into`DetailView`for the interim.`KeyFlow`+ the index-walk`dispatch_key`land **mechanism-only**: every existing handler returns`Consumed`unconditionally, so no key cascades yet and behavior is provably unchanged — the pass-through policy (which keys`Pass`; Popup `q`= Back) is a binding-table decision that lands with the keymap phases. The`sync.rs:109`guard becomes`views.len()
   == 1` **keeping the page-1 conditions** so this PR stays behavior-neutral
   (render snapshots pixel-identical). Test literals migrate. Independent of
   PR 3.
3. **`feat(storage,runtime): team-scoped cache and team metadata sync`** —
   `MIGRATION_2`, new statements and query/upsert helpers, `upsert_issue_tx`
   scoping, the `lt-types` position fragment, `lt-runtime/src/teams.rs`, the
   trait gains `sync_teams`/`sync_team_data` (Linear + Noop impls), sim
   membership derivation. `fetch_*` still present; TUI untouched.
4. **`refactor(tui): AppEvent queue and StateEvent routing`** —
   `AppEvent`/`StateEvent`, `events_tx`/`events_rx`, `App::apply`, the top-down
   `route_state_event` + `View::consume` over `&StateCtx` (Detail's `Comments`
   and `Issues` arms, `ListView::consume` with the `focused` guard); the comment
   worker goes payload-free; `CommentSyncEvent` and `DetailView`'s interim
   receiver die; `submit_comment` and `popup_confirm` unify on re-reads
   (`apply_optimistic_in_memory`, `build_optimistic_issue`, and
   `selected_issue_mut` die). Coexists with the remaining pollers and
   `EventSource` keys. **Requires PR 2.**
5. **`refactor(tui,runtime): cache-first pickers`** — modal and popups read
   SQLite, `spawn_state_refresh`, `NewIssueModal::consume` and
   `PopupView::consume`; `ModalEvent`, `modal_rx`, and the three `fetch_*` trait
   methods die. Requires PRs 3 and 4.
6. **`refactor(runtime,tui): sync/login completion callbacks and lifecycle typestates`**
   — `OnSync`/`OnLogin`, the dead `query` param dropped, the `catch_unwind`
   guard, `LifecycleEvent` + `consume_lifecycle`;
   `SyncState`/`login_rx`/`session.not_authenticated`/`viewer_name`/`org_name` →
   `sync: SyncStatus` + `auth: AuthStatus`, the label derived at render
   (`build_sync_status_label` dies, `format_sync_label` narrows),
   `LoginEvent::Success` carries `Option<viewer::User>`, the login-success
   `!Syncing` guard, and `App::start_sync` deduplicating the four spawn sites
   (`lib.rs:657-665`, `lib.rs:749`, `lib.rs:815-822`, `sync.rs:68-74`; `run()`
   reorders to construct `App` before the startup spawn). The typestates land
   here, not in PR 2: the receivers _are_ the in-flight state until the
   callbacks arrive — landing the enums earlier means parallel state (the
   redundant-tag smell this ADR deletes) or rewriting the pollers twice.
   Requires PR 4; independent of PR 5.
7. **`refactor(tui): keys through the queue`** — input thread, `EventPump`, the
   `recv_timeout` loop; `EventSource`/`CrosstermEvents`/`ScriptedEvents` die;
   loop tests migrate. Requires PR 4; last, so the loop rewrite lands on a fully
   queue-fed app.

Ordering: 2 before 4; 3 before 5; 4 before 5, 6, and 7; 2∥3 and 5∥6 are
parallelizable. Keymap PR #43 phase 1 can land before or after PR 2 (a
one-function rebase either way); its phase 4 requires PR 2.

## Open questions

None blocking.

- Startup's synchronous `fetch_viewer` (`lib.rs:744`) still blocks briefly
  before the TUI starts; reading `synced_viewer` from the cache instead is a
  natural follow-up, deliberately out of scope here.
- `SearchOverlay::run_search` opens the profile DB via `db_path()`
  (`popup.rs:176-178`), bypassing `app.db` — the same wart class as the comment
  worker fixed in PR 4; Search has no `consume`, so the fix is threading the ctx
  into `run_search`. Rides PR 4.
- Whether `AuthStatus::Failed { message }` earns its keep over `Unauthenticated`
  plus the footer message (identical label and gates) — adopted per the review
  sketch; collapse later if it stays inert.
