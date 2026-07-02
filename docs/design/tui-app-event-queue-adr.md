# TUI AppEvent Queue (ENG-32)

## Status

Proposed

## Context

The TUI event loop drains four independent `Option<mpsc::Receiver<T>>` fields
every frame, each with its own poll function and its own borrow-checker dance
(take/restore, or collect-into-a-`Vec`). The overall
spawn-a-thread-and-drain-per-frame model is described in
[[architecture.md#TUI]]; this ADR changes only how the results travel back.

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
Dropping a receiver doubles as cancellation (`close_detail`, modal drop,
`sync_rx` replacement on login success), and a dropped sender (worker panic)
doubles as a completion signal via the `Disconnected` arm. It works, but the
four mechanisms all differ slightly, the `Option` wrapping leaks into every
consumer, and each new background job adds a fifth copy of the pattern.

This ADR unifies them into one long-lived `mpsc::channel<AppEvent>` drained once
per frame:

```text
  [sync worker] ────┐
  [login worker] ───┤  Sender<AppEvent> (cloned)
  [comment worker] ─┼──────────────┐
  [modal worker] ───┘              v
                          App.events_rx ── drain_app_events ── apply_* fns
                          (one Receiver, non-optional, once per frame)
```

### Prior art divergence

The tracking issue cites gitui's `queue.rs:86-193`. gitui's `Queue` is an
`Rc<RefCell<VecDeque<InternalEvent>>>` for same-thread component-to-component
messaging; its cross-thread async results travel on a separate channel drained
by `update_async`. All four of lt's channels are cross-thread worker results, so
the honest adaptation is the channel half of gitui's split, not the
`Rc<RefCell<VecDeque>>` half. lt has no same-thread component messaging (key
handlers mutate `App` directly), so no in-loop `VecDeque` is needed.

## Decision 1: `AppEvent` wraps the existing payload enums, in `lt-tui`

```rust
// crates/lt-tui/src/lib.rs
/// A message from a background worker to the event loop. All cross-thread
/// results funnel through one channel, drained once per frame.
pub enum AppEvent {
    Sync(SyncEvent),
    Login(LoginEvent),
    /// Comment refresh for the issue `issue_id`; stale if the detail pane
    /// has since closed or moved to another issue.
    Comments {
        issue_id: String,
        event: CommentSyncEvent,
    },
    /// Picker data for the team `team_id`; stale if the modal has since
    /// closed or the selected team changed.
    Modal {
        team_id: String,
        event: ModalEvent,
    },
}
```

- **Wrap, don't flatten.** `SyncEvent`/`LoginEvent` are the `SyncService`
  trait's vocabulary and stay in `lt-runtime/src/sync/service.rs`; flattening
  them would duplicate runtime types in the TUI. Flattening the two TUI-local
  enums (`CommentSyncEvent`, `ModalEvent`) would grow the drain's match by one
  arm per message instead of one arm per subsystem and destroy the per-subsystem
  `apply_*` boundaries (Decision 4). Wrapping keeps each file owning its message
  type.
- **Correlation ids live on the wrapper variant**, not inside the payload enums:
  the payload enums describe what happened; the wrapper describes which
  conversation it belongs to. The `Error` arms get correlation for free. Neither
  payload enum carries an id today, so the ids are added here, not read off
  existing types.
- `AppEvent` lives in `lib.rs` next to `App` and `Mode`. A dedicated `events.rs`
  module for one enum and one small drain function is speculative structure
  ([[posture.md#2. Simplicity First]]).

`App` gains two non-optional fields, created in `App::new`:

```rust
/// Producer end of the app event queue; cloned into every background worker.
pub events_tx: mpsc::Sender<AppEvent>,
/// The single consumer, drained once per frame by `drain_app_events`.
events_rx: mpsc::Receiver<AppEvent>,
```

`SyncState.sync_rx`, `App.login_rx`, `App.detail_comment_rx`, and
`NewIssueModal.modal_rx` are deleted.

## Decision 2: `SyncService::spawn_*` take a completion callback

The trait cannot name `AppEvent` (`lt-runtime` must not depend on `lt-tui`), so
the spawn methods stop returning receivers and instead accept a completion
callback:

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

    // fetch_viewer / fetch_teams / fetch_workflow_states /
    // fetch_team_members / sync_comments: unchanged.
}
```

The TUI passes a closure that wraps and forwards:

```rust
let tx = self.events_tx.clone();
self.service.spawn_sync(
    full,
    fetch_identity,
    Box::new(move |ev| {
        let _ = tx.send(AppEvent::Sync(ev));
    }),
);
```

- The dead `query: IssueQuery` parameter is dropped: the concrete impl binds it
  `_query` (`adapter.rs:43`) and every caller clones `app.args` into it for
  nothing. The signature is being rewritten anyway, and keeping the dead
  parameter would push the new signature past clippy's
  `too-many-arguments-threshold` of 4 once `on_done` lands.
- Both jobs send exactly one event today (`adapter.rs:41-103`), so `FnOnce` is
  the honest type; a `Fn` or a `Sender` parameter would advertise multi-shot
  delivery that doesn't exist.

Rejected alternatives:

| Option                                             | Why rejected                                                                                                       |
| -------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| Keep returning `Receiver`, fan in with TUI threads | one extra thread per job existing purely to move a value between channels                                          |
| Move the trait into `lt-tui`                       | forces `LinearSyncService`, and with it `lt-upstream`/cynic, into `lt-tui` — defeats the seam the trait exists for |
| Make the trait generic over an event mapper        | loses object safety; `Arc<dyn SyncService>` is how `lt-cli` injects it (`lt-cli/src/main.rs:73`)                   |
| Pass `Sender<AppEvent>` into the trait             | `lt-runtime` would depend on a TUI type; the same inversion as moving the trait, in disguise                       |

**Comment-sync and modal-load workers stay TUI-spawned.** They compose existing
trait methods with TUI concerns (a post-sync SQLite re-read producing
`Vec<Comment>`; `PopupItem` construction via `build_assignee_items`). Hoisting
them into the trait for symmetry would push TUI view types into `lt-runtime`,
and the modal worker sends up to two events, which doesn't fit the `FnOnce`
contract. They clone `app.events_tx` and send `AppEvent::Comments { .. }` /
`AppEvent::Modal { .. }` directly.

## Decision 3: staleness is filtered at apply time by correlation id

Cancellation-by-drop dies with the per-job receivers: every event reaches the
drain, so each apply function decides whether the event still matches the UI.

**Ids, not generation counters.** The UI state already carries the id to compare
against (`app.detail.issue.id` for comments; the modal's selected team for
pickers), so ids are the minimal mechanism. The only case a generation counter
would additionally distinguish — two in-flight jobs for the same id — is
idempotent: both deliver a fresh read/fetch of the same data, and
last-write-wins is correct.

A new helper deduplicates the selected-team-id lookup that appears twice today
(`new_issue.rs:154-160`, `219-224`):

```rust
impl NewIssueModal {
    /// The id of the currently selected team, if it has one.
    fn selected_team_id(&self) -> Option<&str> { /* ... */ }
}
```

### Staleness matrix

| #   | Event at drain time                           | UI state                    | Today's mechanism                            | New mechanism                                                                                                                 |
| --- | --------------------------------------------- | --------------------------- | -------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| S1  | `Comments{A}`                                 | detail open on A            | delivered                                    | id match → apply                                                                                                              |
| S2  | `Comments{A}`                                 | detail closed               | rx dropped (`detail.rs:78`)                  | `app.detail` is `None` → drop                                                                                                 |
| S3  | `Comments{A}`                                 | detail open on B            | rx replaced (`detail.rs:33`)                 | `A != B` → drop                                                                                                               |
| S4  | `Comments{A}` twice (close/reopen A fast)     | detail open on A            | older rx dropped                             | both apply; idempotent (each is a fresh post-`sync_comments` DB read)                                                         |
| S5  | `Modal{T}`                                    | modal open, team T selected | delivered                                    | id match → apply                                                                                                              |
| S6  | `Modal{T}`                                    | modal closed                | modal (and rx) dropped                       | `new_issue_modal` is `None` → drop                                                                                            |
| S7  | `Modal{T}`                                    | modal open, team U selected | rx replaced on reload                        | `T != selected` → drop                                                                                                        |
| S8  | `Modal{T}` stale + fresh load for T in flight | modal open, team T          | older rx dropped                             | stale event applies; same-team data, idempotent; `loading` may clear a frame early — accepted                                 |
| S9  | `Sync(_)`                                     | any                         | single `sync_rx`, replaced on login          | at most one sync in flight via `syncing: bool`, once the login-success path gains the missing `!syncing` guard (`sync.rs:68`) |
| S10 | `Login(_)`                                    | any                         | `login_rx` presence gates 'L' (`lib.rs:929`) | `login_in_flight: bool` on `App`; set on 'L', cleared by `apply_login_event`                                                  |

**In-flight indicators:** the existing `sync.syncing` replaces `sync_rx`
presence; a new `login_in_flight: bool` on `App` replaces `login_rx.is_some()`.
Comments need no flag (no in-flight UI today); the modal keeps its existing
`loading` bool, set by the spawner and cleared by `apply_modal_event`.

## Decision 4: one drain, per-subsystem apply functions

```rust
// crates/lt-tui/src/lib.rs
/// Drain all pending background events and apply them to the app.
fn drain_app_events(app: &mut App) {
    while let Ok(event) = app.events_rx.try_recv() {
        match event {
            AppEvent::Sync(ev) => sync::apply_sync_event(app, ev),
            AppEvent::Login(ev) => sync::apply_login_event(app, ev),
            AppEvent::Comments { issue_id, event } => {
                detail::apply_comment_event(app, &issue_id, event);
            }
            AppEvent::Modal { team_id, event } => {
                new_issue::apply_modal_event(app, &team_id, event);
            }
        }
    }
}
```

- `run_app` calls `drain_app_events(app)` once at the top of the loop, replacing
  the four poll calls. The 30s periodic-sync check and `poll_search_debounce`
  remain as-is (Decision 6).
- **Borrow-checker viability:** the borrow of `app.events_rx` in `try_recv()`
  ends when the expression yields its owned `Result`; nothing is borrowed across
  the loop body, so the `apply_*` fns take `&mut App` freely. The `Vec`-collect
  workaround in `poll_modal_events` (`new_issue.rs:258-271`) — needed because
  the receiver lived inside `new_issue_modal` — dies, as do the take/restore
  dances in `poll_sync_events` and `poll_detail_comment_events`.
- Apply functions keep today's file organization and stay under the clippy
  budgets (each is roughly the `Ok(..)` match arms of its poller minus the
  channel plumbing):
  - `sync.rs`: `apply_sync_event` — the three `Ok(..)` arms of
    `poll_sync_events` verbatim, including the `Status::Loading` reset and
    `next_sync_at` scheduling. `apply_login_event` — the two `Ok(..)` arms of
    `poll_login_events`, clearing `login_in_flight`, with the follow-up sync
    spawn newly guarded by `!app.sync.syncing` (S9).
  - `detail.rs`: `apply_comment_event` — S1-S3 filter, then
    `detail.comments = comments` on `Done`, no-op on `Error`.
  - `new_issue.rs`: `apply_modal_event` — S5-S7 filter, then today's three arms.
- The four sync-spawn sites (`lib.rs:657-665`, `lib.rs:749`, `lib.rs:815-822`,
  `sync.rs:68-74`) collapse into one helper,
  `App::start_sync(&mut self, full: bool)`, which sets `syncing`, sets the
  label, and spawns with `fetch_identity = self.viewer_name.is_none()`. In
  `run()` this means constructing `App` before spawning the startup sync (today
  the receiver is threaded through `SyncState`, forcing the spawn first); the
  reorder is behavior-neutral.

## Decision 5: sender lifecycle and worker-panic recovery

`App` holds a `Sender` forever, so the queue never disconnects and every
`Disconnected` arm dies. What those arms did today:

| Poller                       | `Disconnected` behavior today                                                            | Subsumed by                                               |
| ---------------------------- | ---------------------------------------------------------------------------------------- | --------------------------------------------------------- |
| `poll_sync_events`           | clear `syncing`, repair label (`sync.rs:143-150`)                                        | adapter panic guard (below)                               |
| `poll_login_events`          | clear `login_rx` (`sync.rs:82-84`)                                                       | adapter panic guard (below)                               |
| `poll_detail_comment_events` | drop rx; no state change (`detail.rs:228`)                                               | nothing to preserve — a lost comment refresh is invisible |
| `poll_modal_events`          | none — `while let Ok` exits on any `Err`; `loading` already sticks on worker panic today | no regression; accepted as today                          |

The real hazard is sync/login: today a panicking worker drops its `tx`,
`Disconnected` fires, and `syncing`/`login_rx` recover. With a shared sender a
panicked worker sends nothing, `syncing` sticks at `true` forever, and the 30s
periodic sync never reschedules (`next_sync_at` is only set in the event arms).
Panics are denied in workspace code, but dependencies (rusqlite, the HTTP stack,
slice indexing) can still panic. So the trait's "invoked exactly once, even if
the sync body panics" contract is implemented in `LinearSyncService` with
`catch_unwind`:

```rust
// crates/lt-runtime/src/adapter.rs (shape; likewise for spawn_login)
fn spawn_sync(&self, full: bool, fetch_identity: bool, on_done: OnSync) {
    std::thread::spawn(move || {
        let event = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_sync_body(full, fetch_identity) // today's thread body, returning SyncEvent
        }))
        .unwrap_or_else(|_| SyncEvent::Error("sync worker panicked".to_string()));
        on_done(event);
    });
}
```

This is strictly better than today: the user sees `sync error: ...` and the 30s
retry keeps running, instead of a silent label repair. The TUI-spawned
comment/modal workers get no guard — matching today's semantics exactly (see
table) and keeping the change minimal.

`NoopSyncService` (the test double) drops the callback without calling it — the
"nothing happens" a noop should be. One subtle divergence: today its
immediately-disconnected receiver means a test that triggers `refresh` sees
`syncing` cleared a frame later via `Disconnected`; with the queue it stays
`true`. No current test depends on this; tests that need sync completion send
`AppEvent::Sync(..)` explicitly.

## Decision 6: scope boundaries

- **`poll_search_debounce` and the 30s periodic sync stay as inline timer
  checks.** Neither is a channel today; both are "has `Instant` X elapsed"
  predicates over state the loop already owns. Folding them into the queue would
  require a producer thread whose only job is to watch a clock — more machinery,
  not less.
- **Key input stays on `EventSource`.** The blocking `next_key(100ms)` call is
  the loop's tick; routing keys through the queue would need a reader thread
  plus a blocking `recv_timeout` redesign of the loop. Possible future work, out
  of scope here.

## Test migration

All in `crates/lt-tui/src/loop_tests.rs` unless noted
(`#[cfg(all(test, feature = "sim"))]`).

- **Per-poller tests → send + drain.** The pattern
  `let (tx, rx) = mpsc::channel(); tx.send(ev); app.X_rx = Some(rx); poll_X(&mut app)`
  becomes
  `app.events_tx.send(AppEvent::X { .. }).unwrap(); drain_app_events(&mut app)`.
  Affected: the three `poll_sync_events_*` tests,
  `poll_detail_comment_events_done_updates_detail` (asserting the comments
  applied replaces asserting `detail_comment_rx.is_none()`; the test now passes
  a matching `issue_id`), `poll_login_events_error_sets_footer` (assert
  `login_in_flight` cleared instead of `login_rx.is_none()`), and
  `poll_modal_events_applies_loaded_data` (the `NewIssueModal` literal loses
  `modal_rx` and carries a selected team whose id matches the event's
  `team_id`).
- **Disconnected tests die** (`poll_login_events_disconnected_clears_receiver`
  and the disconnect half of
  `poll_detail_comment_events_error_clears_receiver`): the state they exercised
  no longer exists.
- **New staleness tests** for the matrix rows whose mechanism changed: S2, S3,
  S6, S7, S9 (login success spawns no second sync while `syncing`), S10 ('L'
  gated by `login_in_flight`).
- **`App::for_test` / `App::new`:** the channel is created unconditionally in
  `App::new`; `for_test` loses the `sync_rx: None` line in its `SyncState`
  literal.
- **`NoopSyncService`:** `spawn_sync`/`spawn_login` become the callback
  signature with empty bodies.
- **`render_tests.rs`:** the `NewIssueModal` literal loses `modal_rx: None`.

## Delivery: stacked PRs (each green under `make test` + `make check`)

1. **`docs(design)`** — this document.
2. **`refactor(tui): route comment and modal events through one AppEvent queue`**
   — no crate-boundary change; `lt-runtime` untouched.
   - Add `AppEvent` (only `Comments`/`Modal` variants at this stage),
     `events_tx`/`events_rx` on `App`, and `drain_app_events` in the loop,
     replacing `poll_detail_comment_events` and `App::poll_modal_events`.
   - Delete `detail_comment_rx` and `modal_rx`; add `apply_comment_event`,
     `apply_modal_event`, `NewIssueModal::selected_team_id`; producers clone
     `events_tx`.
   - Migrate/add the comment and modal tests and the `render_tests.rs` literal.
   - The drain coexists with the two remaining pollers until PR 3.
3. **`refactor(runtime,tui): sync/login completion callbacks onto the AppEvent queue`**
   - `lt-runtime`: trait change (drop the dead `query` param, add
     `OnSync`/`OnLogin`), `catch_unwind` completion guard in
     `LinearSyncService`.
   - `lt-tui`: add `Sync`/`Login` variants to `AppEvent`; delete
     `SyncState.sync_rx`, `App.login_rx`, `poll_sync_events`,
     `poll_login_events`; add `apply_sync_event`, `apply_login_event`,
     `login_in_flight`, the `!syncing` guard on login success, and
     `App::start_sync` deduplicating the four spawn sites (reordering `run()` to
     build `App` before the startup spawn); update `NoopSyncService`.
   - Migrate/add the sync and login tests; delete the Disconnected tests.
   - Manually verify the login → sync → label flow once (`spawn_login`'s browser
     flow is untestable offline).

Two code PRs is the floor: PR 2's producers and PR 3's trait change touch
disjoint seams, and merging them would put an `lt-runtime` API break and the TUI
queue introduction in one review.

## Open questions

None blocking. Routing key input through the queue (an `AppEvent::Key` variant
fed by a reader thread) is possible future work; it changes the loop's blocking
structure and is not part of ENG-32.
