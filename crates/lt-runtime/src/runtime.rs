//! The concrete data runtime. It owns the sync/login loop, one-shot
//! background upstream refreshes for a composed view opening
//! (docs/design/unified-execute-adr.md, "Decision 3"), and every write; it is
//! the only place in the TUI's runtime that touches `HttpTransport`/cynic
//! directly (behind the injected [`TransportSource`]). `lt-cli` constructs it
//! and injects it into `tui::run`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use lt_storage::db;
use lt_storage::db::{Connection, Database};
use lt_types::viewer::{self, ViewerQuery};
use lt_upstream::auth::login_non_interactive;
use lt_upstream::auth::refresh::load_or_refresh_token;
use lt_upstream::client::{GraphqlTransport, HttpTransport, execute};

use crate::ops::{Operation, Refresh};
use crate::sync::service::{LoginEvent, OnEvent, RuntimeEvent, SyncEvent};

/// The loop's periodic delta-sync cadence.
const SYNC_INTERVAL: Duration = Duration::from_secs(30);

/// Where `Runtime` acquires a live transport for a refresh. Production
/// construction (`load_or_refresh_token` + `HttpTransport::new`) moves out of
/// per-call sites into one injected source, built fresh on every acquisition
/// so a refreshed token is always current; tests inject one that hands out
/// `FakeTransport` responses.
pub trait TransportSource: Send + Sync {
    fn acquire(&self) -> Result<Box<dyn GraphqlTransport>>;
}

/// The production transport source.
pub struct HttpTransportSource;

impl TransportSource for HttpTransportSource {
    fn acquire(&self) -> Result<Box<dyn GraphqlTransport>> {
        let token = load_or_refresh_token()?;
        Ok(Box::new(HttpTransport::new(token.access_token)))
    }
}

/// A one-shot upstream refresh, erased by [`Runtime::refresh`] into a thunk
/// the loop runs once (docs/design/unified-execute-adr.md, "Decision 3"):
/// fetch via `client::execute`, apply via `Fill`.
type RefreshThunk = Box<dyn FnOnce(&Connection, &dyn GraphqlTransport) -> Result<()> + Send>;

/// A command sent through the runtime's internal channel: the public methods
/// (`refresh`/`request_sync`/`login`) plus the login worker's private
/// completion signal, which the loop needs so it -- the sole owner of the
/// pause gate -- decides the follow-up. Every variant is `Copy`: nothing here
/// is worth avoiding a cheap duplication for.
#[derive(Clone, Copy)]
enum Command {
    /// Run the registered one-shot refresh thunk under this id
    /// (`Runtime::refresh`).
    Refresh(u64),
    RequestSync,
    Login,
    LoginFinished(bool),
    /// Prompts the loop to immediately drain the outbox after a caller-side
    /// mutation, instead of waiting for the next sync cycle.
    Drain,
}

/// One decision the loop's core makes in response to a command or a tick.
/// `Copy`: nothing here is worth avoiding a cheap duplication for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Cycle { full: bool },
    Refresh(u64),
    SpawnLogin,
    Drain,
}

/// Every command already buffered on `rx` for this wake, starting with
/// `first`: drained non-blockingly so a burst of buffered commands (e.g.
/// several `Command::Drain`s from rapid mutations) is processed together
/// instead of one wake per command.
fn drain_buffered_commands(rx: &mpsc::Receiver<Command>, first: Command) -> Vec<Command> {
    let mut cmds = vec![first];
    while let Ok(next) = rx.try_recv() {
        cmds.push(next);
    }
    cmds
}

/// Collapse every `Action::Drain` after the first into nothing, preserving
/// the order and count of every other action: one drain already replays the
/// whole outbox, so a burst of buffered drains is fully covered by one.
fn coalesce_drains(actions: Vec<Action>) -> Vec<Action> {
    let mut seen_drain = false;
    actions
        .into_iter()
        .filter(|action| {
            if matches!(action, Action::Drain) {
                let first = !seen_drain;
                seen_drain = true;
                first
            } else {
                true
            }
        })
        .collect()
}

/// The loop's pause gate and login-in-flight guard, decided independent of
/// I/O so cadence/pause/login policy is testable without threads.
struct LoopState {
    /// Set on `NotAuthenticated` or a failed login; cleared by a login
    /// success or `request_sync`. While paused, periodic full/delta cycles
    /// are skipped, but a requested refresh still runs.
    paused: bool,
    login_in_flight: bool,
}

impl LoopState {
    fn new() -> Self {
        Self {
            paused: false,
            login_in_flight: false,
        }
    }

    fn on_command(&mut self, cmd: Command) -> Vec<Action> {
        match cmd {
            Command::Refresh(id) => vec![Action::Refresh(id)],
            Command::Drain => vec![Action::Drain],
            Command::RequestSync => {
                self.paused = false;
                vec![Action::Cycle { full: true }]
            }
            Command::Login => {
                if self.login_in_flight {
                    Vec::new()
                } else {
                    self.login_in_flight = true;
                    vec![Action::SpawnLogin]
                }
            }
            Command::LoginFinished(success) => {
                self.login_in_flight = false;
                if success {
                    self.paused = false;
                    vec![Action::Cycle { full: false }]
                } else {
                    self.paused = true;
                    Vec::new()
                }
            }
        }
    }

    /// The periodic tick's cycle decision.
    fn on_timeout(&self) -> Vec<Action> {
        if self.paused {
            Vec::new()
        } else {
            vec![Action::Cycle { full: false }]
        }
    }

    fn mark_not_authenticated(&mut self) {
        self.paused = true;
    }
}

/// A panicking closure's payload as text, for propagation into the emitted
/// error event rather than a generic "panicked" string: `panic!("...")` and
/// `.unwrap()`/`.expect("...")` payloads are `&str` or `String`; anything else
/// falls back to a generic message.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panicked with a non-string payload".to_string()
    }
}

/// A sync cycle's outcome, for the loop to update its pause bookkeeping.
/// Distinct from [`SyncEvent`]: this is loop-internal, not the wire
/// vocabulary `on_event` reports.
enum CycleOutcome {
    Done,
    Error,
    NotAuthenticated,
}

/// `run`'s mutable loop state, bundled so the per-action handlers stay under
/// the argument-count lint.
struct RunLoop {
    state: LoopState,
    deadline: Instant,
}

impl RunLoop {
    fn new() -> Self {
        Self {
            state: LoopState::new(),
            // Timing out immediately performs the startup sync.
            deadline: Instant::now(),
        }
    }
}

/// [`Runtime::seed_sim`]'s summary, for the caller's report line.
#[cfg(feature = "sim")]
pub struct SimSeed {
    pub issues: usize,
    pub comments: usize,
}

pub struct Runtime {
    db: Mutex<Database>,
    transports: Box<dyn TransportSource>,
    on_event: Arc<OnEvent>,
    /// One-shot refresh thunks awaiting the loop (`Runtime::refresh`), keyed
    /// by a fresh id so `Command`/`Action` stay plain `Copy` data; removed
    /// once run.
    pending_refreshes: Mutex<HashMap<u64, RefreshThunk>>,
    next_refresh_id: AtomicU64,
    commands_tx: mpsc::Sender<Command>,
    /// `run` takes this once, at the start of its loop; `None` after that
    /// signals a second call, which is a programming error (`run` is
    /// documented as called at most once, by `lt-cli`).
    commands_rx: Mutex<Option<mpsc::Receiver<Command>>>,
}

impl Runtime {
    pub fn new(db: Database, transports: Box<dyn TransportSource>, on_event: OnEvent) -> Self {
        let (commands_tx, commands_rx) = mpsc::channel();
        Self {
            db: Mutex::new(db),
            transports,
            on_event: Arc::new(on_event),
            pending_refreshes: Mutex::new(HashMap::new()),
            next_refresh_id: AtomicU64::new(0),
            commands_tx,
            commands_rx: Mutex::new(Some(commands_rx)),
        }
    }

    fn take_commands_rx(&self) -> Option<mpsc::Receiver<Command>> {
        self.commands_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// A fresh connection to the injected database.
    pub(crate) fn connect(&self) -> Result<Connection> {
        self.db
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .connect()
    }

    /// Best-effort viewer identity via the injected transport source, for the
    /// login worker's direct report (`LoginEvent::Success`, not a cache
    /// read). Ordinary sync cycles persist the viewer through the `Fill`
    /// seam instead (`sync::persist_viewer`); the header re-executes
    /// `ViewerQuery` on every `Update` and picks that up.
    fn viewer_identity(&self) -> Option<viewer::Viewer> {
        let transport = match self.transports.acquire() {
            Ok(t) => t,
            Err(e) => {
                tracing::debug!(error = %e, "viewer_identity: failed to acquire transport");
                return None;
            }
        };
        match execute::<ViewerQuery>(transport.as_ref(), ()) {
            Ok(viewer) => viewer,
            Err(e) => {
                tracing::debug!(error = %e, "viewer_identity: viewer query failed");
                None
            }
        }
    }

    /// `last_synced_at` for a `Sync(Done)` timestamp: the DB's own meta,
    /// `None` if it is absent, unreadable, or unparseable (pre-first-sync, or
    /// a corrupt row -- never a panic path).
    pub fn last_synced_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        let raw = self
            .connect()
            .ok()
            .and_then(|conn| db::get_meta(&conn, "last_synced_at").ok().flatten())?;
        chrono::DateTime::parse_from_rfc3339(&raw)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc))
    }

    /// The entire data surface (docs/design/unified-execute-adr.md, "Decision
    /// 1"): a query op reads the cache projection, instant, no network. A
    /// view holds its `vars` and re-executes them on every `Update`
    /// (docs/design/unified-execute-adr.md, "Decision 3") rather than holding
    /// a live slot.
    pub fn execute<Op: Operation>(&self, vars: Op::Variables) -> Result<Op::Output> {
        Op::execute(self, vars)
    }

    /// Trigger a one-shot background upstream refresh of `Op`, applied into
    /// the cache via its `Fill` impl, then emit `Update` on success --
    /// the freshness a composed view (Detail, `NewIssue`, a state/assignee
    /// picker) needs when it opens (docs/design/unified-execute-adr.md,
    /// "Decision 3"). The issues list stays covered by the periodic delta
    /// cycle. Never touches the network on the caller's thread: `Op` is
    /// erased into a thunk the loop runs on its own thread scope.
    pub fn refresh<Op>(&self, vars: Op::Variables)
    where
        Op: Refresh,
        Op::Variables: Send + 'static,
    {
        let id = self.next_refresh_id.fetch_add(1, Ordering::Relaxed);
        let thunk: RefreshThunk =
            Box::new(move |conn, transport| Op::refresh(conn, transport, vars));
        self.pending_refreshes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, thunk);
        if self.commands_tx.send(Command::Refresh(id)).is_err() {
            tracing::debug!("refresh: runtime loop is gone");
        }
    }

    /// The one unscoped signal every cache change emits
    /// (docs/design/unified-execute-adr.md, "Decision 3"): every active view
    /// re-executes its own operation in response, rather than the runtime
    /// tracking which view needs which entity.
    pub(crate) fn emit_update(&self) {
        (self.on_event)(RuntimeEvent::Update);
    }

    /// Run one registered refresh thunk and emit `Update` on success; a
    /// missing id (already run, or retracted) is a no-op. `catch_unwind`-
    /// guarded like a sync cycle, since it shares the same DB/network I/O on
    /// the loop thread.
    fn perform_refresh(&self, id: u64) {
        let Some(thunk) = self
            .pending_refreshes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id)
        else {
            return;
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.run_refresh_thunk(thunk)
        }));
        match outcome {
            Ok(Ok(())) => self.emit_update(),
            Ok(Err(e)) => tracing::warn!(error = %e, "background refresh failed"),
            Err(_) => tracing::warn!("background refresh panicked"),
        }
    }

    fn run_refresh_thunk(&self, thunk: RefreshThunk) -> Result<()> {
        let conn = self.connect()?;
        let transport = self.transports.acquire()?;
        thunk(&conn, transport.as_ref())
    }

    /// One full or delta sync cycle: emits `Sync(Started)`, then the cycle's
    /// outcome, then emits `Update` on success (the viewer identity flows
    /// through this same signal -- `sync::persist_viewer` touches `Viewer`
    /// every cycle, so the header's re-executed `ViewerQuery` picks it up
    /// without this loop separately fetching or reporting it).
    /// `catch_unwind`-guarded so a panicking sync body surfaces as
    /// `Sync(Error)` and the loop survives.
    fn cycle(&self, full: bool) -> CycleOutcome {
        (self.on_event)(RuntimeEvent::Sync(SyncEvent::Started));

        if matches!(lt_config::load_token(), Ok(None) | Err(_)) {
            (self.on_event)(RuntimeEvent::Sync(SyncEvent::NotAuthenticated));
            return CycleOutcome::NotAuthenticated;
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.sync_now(full)));

        match result {
            Ok(Ok(())) => {
                (self.on_event)(RuntimeEvent::Sync(SyncEvent::Done(self.last_synced_at())));
                CycleOutcome::Done
            }
            Ok(Err(e)) => {
                let msg = e.to_string();
                let brief = msg.lines().next().unwrap_or(&msg).to_string();
                (self.on_event)(RuntimeEvent::Sync(SyncEvent::Error(brief)));
                CycleOutcome::Error
            }
            Err(payload) => {
                (self.on_event)(RuntimeEvent::Sync(SyncEvent::Error(panic_message(
                    &*payload,
                ))));
                CycleOutcome::Error
            }
        }
    }

    /// Immediately replay every pending outbox command upstream, then emit
    /// `Update` if at least one command was successfully replayed (a fully
    /// failed drain -- e.g. offline -- changed nothing, so there is nothing
    /// to signal). Runs on the loop thread (`Action::Drain`) so it shares the
    /// loop's serialization of all base writes.
    pub fn drain_now(&self) -> Result<()> {
        let conn = self.connect()?;
        let transport = self.transports.acquire()?;
        if crate::sync::drain::drain(&conn, transport.as_ref())? {
            self.emit_update();
        }
        Ok(())
    }

    /// Connect, acquire a transport, run the requested full or delta sync
    /// body, then emit `Update`.
    fn sync_now(&self, full: bool) -> Result<()> {
        let conn = self.connect()?;
        let transport = self.transports.acquire()?;
        if full {
            crate::sync::full::run(&conn, transport.as_ref())?;
        } else {
            crate::sync::delta::run(&conn, transport.as_ref())?;
        }
        self.emit_update();
        Ok(())
    }

    pub fn sync_full(&self) -> Result<()> {
        self.sync_now(true)
    }

    /// The delta counterpart of [`Runtime::sync_full`].
    pub fn sync_delta(&self) -> Result<()> {
        self.sync_now(false)
    }

    /// Seed the local database from the deterministic `sim` generator: no
    /// sync cycle to establish workflow states offline, so they are derived
    /// from the seeded issues' own state fragments (ADR "Sim compatibility"),
    /// as is team membership (from the issues' team/assignee and
    /// team/creator pairs). Marks the cache fresh and records a viewer
    /// identity (a real assignee from the dataset) so the `--assignee=me`
    /// filter resolves offline.
    #[cfg(feature = "sim")]
    pub fn seed_sim(&self, seed: u64, size: usize) -> Result<SimSeed> {
        let dataset = crate::sim::generate(seed, size);
        let conn = self.connect()?;
        for (team_id, state) in crate::sim::derive_workflow_states(&dataset.issues) {
            db::upsert_team_state(&conn, &team_id, &state)?;
        }
        db::upsert_issues(&conn, &dataset.issues)?;
        db::upsert_comments(&conn, &dataset.comments)?;
        db::derive_team_memberships_from_issues(&conn)?;
        db::set_meta(&conn, "last_synced_at", &chrono::Utc::now().to_rfc3339())?;
        if let Some(assignee) = dataset.issues.iter().find_map(|i| i.assignee.clone()) {
            db::set_viewer(
                &conn,
                &viewer::Viewer {
                    user: lt_types::types::User {
                        id: assignee.id,
                        name: assignee.name,
                    },
                    organization: viewer::Organization {
                        id: String::new().into(),
                        name: String::new(),
                        url_key: String::new(),
                    },
                },
            )?;
        }
        Ok(SimSeed {
            issues: dataset.issues.len(),
            comments: dataset.comments.len(),
        })
    }

    /// The login worker's body, run on its own thread. `Success` requires a
    /// fresh identity: a token exchange that succeeds but whose identity
    /// fetch fails is reported as `Error`.
    fn run_login_body(&self) -> LoginEvent {
        match login_non_interactive() {
            Ok(()) => match self.viewer_identity() {
                Some(viewer) => LoginEvent::Success { viewer },
                None => LoginEvent::Error("login succeeded but identity fetch failed".to_string()),
            },
            Err(e) => LoginEvent::Error(e.to_string()),
        }
    }
}

impl Runtime {
    /// Execute one action: a full/delta cycle updates `run`'s bookkeeping in
    /// place; a refresh and a login spawn are self-contained.
    fn perform<'scope>(
        &'scope self,
        action: Action,
        run: &mut RunLoop,
        scope: &'scope thread::Scope<'scope, '_>,
    ) {
        match action {
            Action::Cycle { full } => self.run_cycle(full, run),
            Action::Refresh(id) => self.perform_refresh(id),
            Action::SpawnLogin => self.spawn_login(scope),
            Action::Drain => self.perform_drain(),
        }
    }

    /// `Action::Drain`'s body: run the drain, panic-guarded like a sync cycle
    /// since it shares the same DB/network I/O on the loop thread.
    fn perform_drain(&self) {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.drain_now())) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "immediate outbox drain failed"),
            Err(_) => tracing::warn!("immediate outbox drain panicked"),
        }
    }

    /// Run one cycle and fold its outcome into `run`'s pause bookkeeping,
    /// then push the deadline out another interval.
    fn run_cycle(&self, full: bool, run: &mut RunLoop) {
        match self.cycle(full) {
            CycleOutcome::NotAuthenticated => run.state.mark_not_authenticated(),
            CycleOutcome::Done | CycleOutcome::Error => {}
        }
        run.deadline = Instant::now() + SYNC_INTERVAL;
    }

    /// Spawn the login worker on the loop's thread-scope: it runs the OAuth
    /// flow, emits `Login(..)` directly, then nudges the loop with
    /// `LoginFinished` so the loop -- the sole owner of the pause gate --
    /// decides the follow-up.
    fn spawn_login<'scope>(&'scope self, scope: &'scope thread::Scope<'scope, '_>) {
        scope.spawn(move || {
            let event =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_login_body()))
                    .unwrap_or_else(|payload| LoginEvent::Error(panic_message(&*payload)));
            let success = matches!(event, LoginEvent::Success { .. });
            (self.on_event)(RuntimeEvent::Login(event));
            if self
                .commands_tx
                .send(Command::LoginFinished(success))
                .is_err()
            {
                tracing::debug!("login worker: runtime loop is gone");
            }
        });
    }

    /// The runtime loop: blocks for the life of the process. `lt-cli` spawns
    /// it on a detached background thread before the TUI starts. Owns all
    /// scheduling: the startup sync, the 30s delta cadence, a composed view's
    /// one-shot upstream refresh requested on open, and full syncs on
    /// request.
    pub fn run(&self) {
        let Some(commands_rx) = self.take_commands_rx() else {
            tracing::error!("Runtime::run must be called at most once");
            return;
        };

        let mut run = RunLoop::new();

        thread::scope(|scope| {
            loop {
                let timeout = run.deadline.saturating_duration_since(Instant::now());
                let actions = match commands_rx.recv_timeout(timeout) {
                    Ok(cmd) => {
                        let actions = drain_buffered_commands(&commands_rx, cmd)
                            .into_iter()
                            .flat_map(|c| run.state.on_command(c))
                            .collect();
                        coalesce_drains(actions)
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        run.deadline = Instant::now() + SYNC_INTERVAL;
                        run.state.on_timeout()
                    }
                    // Unreachable in production: `self` holds `commands_tx`
                    // for the lifetime of `run`, so the channel never
                    // disconnects; treated as an idle tick.
                    Err(mpsc::RecvTimeoutError::Disconnected) => Vec::new(),
                };
                for action in actions {
                    self.perform(action, &mut run, scope);
                }
            }
        });
    }

    /// User-initiated: nudges the loop into an immediate full sync (the `r`
    /// key).
    pub fn request_sync(&self) {
        if self.commands_tx.send(Command::RequestSync).is_err() {
            tracing::debug!("request_sync: runtime loop is gone");
        }
    }

    /// User-initiated: runs the OAuth login flow (the `L` key).
    pub fn login(&self) {
        if self.commands_tx.send(Command::Login).is_err() {
            tracing::debug!("login: runtime loop is gone");
        }
    }

    /// Nudge the loop to drain the outbox immediately after a caller-side
    /// mutation, instead of waiting for the next periodic sync cycle.
    pub(crate) fn request_drain(&self) {
        if self.commands_tx.send(Command::Drain).is_err() {
            tracing::debug!("request_drain: runtime loop is gone");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc as std_mpsc;

    use lt_types::comments::{CommentCreateMutation, CommentCreateVariables};
    use lt_types::inputs::{CommentCreateInput, IssueCreateInput};
    use lt_types::issues::{
        IssueCreateMutation, IssueCreateVariables, IssueUpdateMutation, IssueUpdateVariables,
        IssuesQuery, IssuesVariables, sample_issue_node,
    };
    use lt_types::members::{TeamMembersQuery, TeamVariables as MembersTeamVariables};
    use lt_types::states::{TeamStatesQuery, TeamVariables as StatesTeamVariables};
    use lt_types::teams::TeamsQuery;
    use lt_types::types;
    use lt_upstream::client::FakeTransport;
    use serde_json::json;

    use super::*;

    // -- LoopState (pure decisions) ------------------------------------

    #[test]
    fn refresh_command_prompts_a_refresh_action() {
        let mut state = LoopState::new();
        assert_eq!(
            state.on_command(Command::Refresh(7)),
            vec![Action::Refresh(7)]
        );
    }

    #[test]
    fn request_sync_unpauses_and_cycles_full() {
        let mut state = LoopState::new();
        state.mark_not_authenticated();
        assert_eq!(state.on_timeout(), Vec::new()); // paused: no cycle

        let actions = state.on_command(Command::RequestSync);
        assert_eq!(actions, vec![Action::Cycle { full: true }]);
        assert_eq!(state.on_timeout(), vec![Action::Cycle { full: false }]);
    }

    #[test]
    fn login_is_ignored_while_one_is_in_flight() {
        let mut state = LoopState::new();
        assert_eq!(state.on_command(Command::Login), vec![Action::SpawnLogin]);
        assert_eq!(state.on_command(Command::Login), Vec::new());
    }

    #[test]
    fn login_finished_success_unpauses_and_cycles_delta() {
        let mut state = LoopState::new();
        state.on_command(Command::Login);
        state.mark_not_authenticated();

        let actions = state.on_command(Command::LoginFinished(true));
        assert_eq!(actions, vec![Action::Cycle { full: false }]);
        assert_eq!(state.on_timeout(), vec![Action::Cycle { full: false }]);
        // A new login is accepted again.
        assert_eq!(state.on_command(Command::Login), vec![Action::SpawnLogin]);
    }

    #[test]
    fn drain_command_prompts_a_drain_action() {
        let mut state = LoopState::new();
        assert_eq!(state.on_command(Command::Drain), vec![Action::Drain]);
    }

    // -- drain_buffered_commands --------------------------------------------

    #[test]
    fn drain_buffered_commands_collects_everything_already_queued() {
        let (tx, rx) = std_mpsc::channel();
        tx.send(Command::Drain).unwrap();
        tx.send(Command::Drain).unwrap();
        tx.send(Command::RequestSync).unwrap();

        let cmds = drain_buffered_commands(&rx, Command::Drain);

        assert_eq!(cmds.len(), 4);
        assert!(matches!(cmds[0], Command::Drain));
        assert!(rx.try_recv().is_err()); // fully drained
    }

    // -- coalesce_drains --------------------------------------------------

    #[test]
    fn coalesce_drains_collapses_several_buffered_drains_into_one() {
        let actions = vec![Action::Drain, Action::Drain, Action::Drain];
        assert_eq!(coalesce_drains(actions), vec![Action::Drain]);
    }

    #[test]
    fn coalesce_drains_preserves_other_actions_and_their_order() {
        let actions = vec![Action::Drain, Action::Refresh(3), Action::Drain];
        assert_eq!(
            coalesce_drains(actions),
            vec![Action::Drain, Action::Refresh(3)]
        );
    }

    #[test]
    fn login_finished_failure_pauses() {
        let mut state = LoopState::new();
        state.on_command(Command::Login);

        let actions = state.on_command(Command::LoginFinished(false));
        assert_eq!(actions, Vec::new());
        assert_eq!(state.on_timeout(), Vec::new()); // paused: no cycle
    }

    // -- Runtime: execute / write / Update, thread-free -------------------

    fn on_event_channel() -> (OnEvent, std_mpsc::Receiver<RuntimeEvent>) {
        let (tx, rx) = std_mpsc::channel();
        let on_event: OnEvent = Box::new(move |ev| {
            drop(tx.send(ev));
        });
        (on_event, rx)
    }

    fn runtime_over(db: Database) -> (Runtime, std_mpsc::Receiver<RuntimeEvent>) {
        let (on_event, rx) = on_event_channel();
        (
            Runtime::new(db, Box::new(HttpTransportSource), on_event),
            rx,
        )
    }

    fn issues_vars() -> IssuesVariables {
        IssuesVariables {
            filter: None,
            sort: None,
            first: None,
            after: None,
        }
    }

    #[test]
    fn execute_reads_the_cache_synchronously() {
        let db = Database::memory().unwrap();
        {
            let conn = db.connect().unwrap();
            db::upsert_teams(
                &conn,
                &[types::Team {
                    id: "t1".into(),
                    name: "Eng".to_string(),
                }],
            )
            .unwrap();
        }
        let (runtime, _rx) = runtime_over(db);

        let teams = runtime.execute::<TeamsQuery>(()).unwrap();

        assert_eq!(teams.nodes.len(), 1);
        assert_eq!(teams.nodes[0].name, "Eng");
    }

    #[test]
    fn create_issue_emits_update_and_a_reexecute_sees_it() {
        let db = Database::memory().unwrap();
        {
            let conn = db.connect().unwrap();
            // The team itself must already be cached (`enqueue_issue_create`
            // only mints a nameless skeleton row for an uncached team id).
            db::upsert_teams(
                &conn,
                &[types::Team {
                    id: "t1".into(),
                    name: "Eng".to_string(),
                }],
            )
            .unwrap();
            // The optimistic create defaults to the team's first cached state
            // (sync owns workflow states; issue upserts never write them).
            db::upsert_team_state(
                &conn,
                "t1",
                &types::WorkflowState {
                    id: "s-todo".into(),
                    name: "Todo".to_string(),
                    position: 1.0,
                },
            )
            .unwrap();
        }
        let (runtime, rx) = runtime_over(db);
        assert!(
            runtime
                .execute::<IssuesQuery>(issues_vars())
                .unwrap()
                .nodes
                .is_empty()
        );

        let input = IssueCreateInput {
            title: "New issue".to_string(),
            team_id: "t1".to_string(),
            description: None,
            state_id: None,
            priority: None,
            assignee_id: None,
        };
        let issue = runtime
            .execute::<IssueCreateMutation>(IssueCreateVariables { input })
            .unwrap();
        assert_eq!(issue.identifier, db::op_log::OPTIMISTIC_ISSUE_IDENTIFIER);

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Update));

        let page = runtime.execute::<IssuesQuery>(issues_vars()).unwrap();
        assert_eq!(page.nodes.len(), 1);
        assert_eq!(page.nodes[0].identifier, issue.identifier);
    }

    #[test]
    fn create_comment_emits_update_and_a_reexecute_of_the_detail_sees_it() {
        let db = db_with_a_todo_issue("issue-1");
        let (runtime, rx) = runtime_over(db);
        let detail_vars = lt_types::detail::IssueDetailVariables {
            id: "issue-1".to_string(),
        };
        assert!(
            runtime
                .execute::<lt_types::detail::IssueDetailQuery>(detail_vars.clone())
                .unwrap()
                .unwrap()
                .comments
                .is_empty()
        );

        let comment = runtime
            .execute::<CommentCreateMutation>(CommentCreateVariables {
                input: CommentCreateInput {
                    issue_id: "issue-1".to_string(),
                    body: "hello".to_string(),
                },
            })
            .unwrap();
        assert_eq!(comment.body, "hello");

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Update));

        let data = runtime
            .execute::<lt_types::detail::IssueDetailQuery>(detail_vars)
            .unwrap()
            .unwrap();
        assert_eq!(data.comments.len(), 1);
        assert_eq!(data.comments[0].body, "hello");
    }

    #[test]
    fn update_issue_emits_a_single_unscoped_update() {
        // `Update` is unscoped: it carries no entity id, so any write's
        // signal is indistinguishable from any other's.
        let db = db_with_a_todo_issue("issue-1");
        let (runtime, rx) = runtime_over(db);

        let updated = runtime
            .execute::<IssueUpdateMutation>(IssueUpdateVariables {
                id: "issue-1".to_string(),
                input: lt_types::inputs::IssueUpdateInput {
                    priority: Some(1),
                    ..Default::default()
                },
            })
            .unwrap();
        assert_eq!(updated.unwrap().id.inner(), "issue-1");

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Update));
    }

    // -- Runtime::refresh: one-shot upstream freshness --------------------

    struct FakeGraphql(Arc<Mutex<FakeTransport>>);

    impl GraphqlTransport for FakeGraphql {
        fn query(&self, query: &str, variables: serde_json::Value) -> Result<serde_json::Value> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .query(query, variables)
        }
    }

    // `FakeTransport` is `!Sync` (its scripted queue is a `RefCell`); a
    // `Mutex` around the shared transport gives `FakeSource` the
    // `Send + Sync` `TransportSource` requires without changing
    // `FakeTransport` itself.
    struct FakeSource(Arc<Mutex<FakeTransport>>);

    impl FakeSource {
        fn new(transport: FakeTransport) -> Self {
            Self(Arc::new(Mutex::new(transport)))
        }
    }

    impl TransportSource for FakeSource {
        fn acquire(&self) -> Result<Box<dyn GraphqlTransport>> {
            Ok(Box::new(FakeGraphql(Arc::clone(&self.0))))
        }
    }

    /// A single scripted `team.states` page, shared by every test that drives
    /// a `TeamStatesQuery` refresh.
    fn team_states_page_transport() -> FakeTransport {
        FakeTransport::new(vec![json!({ "team": { "states": { "nodes": [
            { "id": "s1", "name": "Todo", "position": 1.0 }
        ] } } })])
    }

    #[test]
    fn refresh_runs_the_thunk_and_emits_update() {
        let db = Database::memory().unwrap();
        let (on_event, rx) = on_event_channel();
        let runtime = Runtime::new(
            db,
            Box::new(FakeSource::new(team_states_page_transport())),
            on_event,
        );
        let commands_rx = runtime.take_commands_rx().unwrap();

        runtime.refresh::<TeamStatesQuery>(StatesTeamVariables {
            team_id: "t1".to_string(),
        });
        let Ok(Command::Refresh(id)) = commands_rx.try_recv() else {
            unreachable!("expected a Refresh command");
        };
        runtime.perform_refresh(id);

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Update));
        let states = runtime
            .execute::<TeamStatesQuery>(StatesTeamVariables {
                team_id: "t1".to_string(),
            })
            .unwrap();
        assert_eq!(states.nodes[0].name, "Todo");
    }

    #[test]
    fn team_members_refresh_runs_the_thunk_and_emits_update() {
        let db = Database::memory().unwrap();
        let fake = FakeTransport::new(vec![json!({ "team": { "members": { "nodes": [
            { "id": "u1", "name": "Ada" }
        ] } } })]);
        let (on_event, rx) = on_event_channel();
        let runtime = Runtime::new(db, Box::new(FakeSource::new(fake)), on_event);
        let commands_rx = runtime.take_commands_rx().unwrap();

        runtime.refresh::<TeamMembersQuery>(MembersTeamVariables {
            team_id: "t1".to_string(),
        });
        let Ok(Command::Refresh(id)) = commands_rx.try_recv() else {
            unreachable!("expected a Refresh command");
        };
        runtime.perform_refresh(id);

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Update));
        let members = runtime
            .execute::<TeamMembersQuery>(MembersTeamVariables {
                team_id: "t1".to_string(),
            })
            .unwrap();
        assert_eq!(members.nodes[0].name, "Ada");
    }

    #[test]
    fn viewer_refresh_runs_the_thunk_and_emits_update() {
        let db = Database::memory().unwrap();
        let fake = FakeTransport::new(vec![json!({
            "viewer": { "id": "u1", "name": "Ada", "organization": { "id": "o1", "name": "Acme", "urlKey": "acme" } }
        })]);
        let (on_event, rx) = on_event_channel();
        let runtime = Runtime::new(db, Box::new(FakeSource::new(fake)), on_event);
        let commands_rx = runtime.take_commands_rx().unwrap();
        assert!(
            runtime
                .execute::<lt_types::viewer::ViewerQuery>(())
                .unwrap()
                .is_none()
        );

        runtime.refresh::<lt_types::viewer::ViewerQuery>(());
        let Ok(Command::Refresh(id)) = commands_rx.try_recv() else {
            unreachable!("expected a Refresh command");
        };
        runtime.perform_refresh(id);

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Update));
        assert_eq!(
            runtime
                .execute::<lt_types::viewer::ViewerQuery>(())
                .unwrap()
                .unwrap()
                .user
                .name,
            "Ada"
        );
    }

    // -- sync_full / sync_delta: a synchronous sync cycle -----------------

    fn full_sync_transport() -> FakeTransport {
        FakeTransport::new(vec![
            json!({ "viewer": { "id": "u1", "name": "Ada", "organization": {
                "id": "o1", "name": "Acme", "urlKey": "acme"
            } } }),
            json!({ "teams": { "nodes": [{ "id": "ENG", "name": "Engineering" }] } }),
            json!({ "workflowStates": { "nodes": [
                { "id": "s", "name": "Todo", "position": 1.0, "team": { "id": "ENG" } }
            ], "pageInfo": { "hasNextPage": false, "endCursor": null } } }),
            json!({ "issues": { "nodes": [sample_issue_node("1")],
                "pageInfo": { "hasNextPage": false, "endCursor": null } } }),
        ])
    }

    #[test]
    fn sync_full_upserts_issues_and_stamps_last_synced_at() {
        let (on_event, _rx) = on_event_channel();
        let runtime = Runtime::new(
            Database::memory().unwrap(),
            Box::new(FakeSource::new(full_sync_transport())),
            on_event,
        );

        runtime.sync_full().unwrap();

        let conn = runtime.connect().unwrap();
        assert!(db::query_issue_by_id(&conn, "1").unwrap().is_some());
        assert!(runtime.last_synced_at().is_some());
    }

    #[test]
    fn sync_delta_falls_back_to_full_before_any_prior_sync() {
        let (on_event, _rx) = on_event_channel();
        let runtime = Runtime::new(
            Database::memory().unwrap(),
            Box::new(FakeSource::new(full_sync_transport())),
            on_event,
        );

        runtime.sync_delta().unwrap();

        let conn = runtime.connect().unwrap();
        assert!(db::query_issue_by_id(&conn, "1").unwrap().is_some());
        assert!(runtime.last_synced_at().is_some());
    }

    // -- seed_sim: the deterministic offline dataset -----------------------

    #[cfg(feature = "sim")]
    #[test]
    fn seed_sim_populates_issues_and_stamps_meta() {
        let (runtime, _rx) = runtime_over(Database::memory().unwrap());

        let summary = runtime.seed_sim(0, 10).unwrap();

        assert_eq!(summary.issues, 10);
        let conn = runtime.connect().unwrap();
        assert_eq!(db::count_issues(&conn).unwrap(), 10);
        assert!(runtime.last_synced_at().is_some());
    }

    // -- drain_now: the immediate write-path flush -----------------------

    /// A single cached issue with its workflow state already known.
    /// `sample_base_issue`'s state must be locally known (sync owns workflow
    /// states; issue upserts never write them), so the read model's join
    /// resolves it.
    fn db_with_a_todo_issue(id: &str) -> Database {
        let db = Database::memory().unwrap();
        let conn = db.connect().unwrap();
        db::upsert_team_state(
            &conn,
            "ENG",
            &types::WorkflowState {
                id: "s-todo".into(),
                name: "Todo".to_string(),
                position: 1.0,
            },
        )
        .unwrap();
        db::upsert_issues(&conn, &[db::op_log::sample_base_issue(id)]).unwrap();
        db
    }

    fn update_priority_to_urgent(runtime: &Runtime, id: &str) {
        runtime
            .execute::<IssueUpdateMutation>(IssueUpdateVariables {
                id: id.to_string(),
                input: lt_types::inputs::IssueUpdateInput {
                    priority: Some(1),
                    ..Default::default()
                },
            })
            .unwrap();
    }

    #[test]
    fn update_issue_sends_a_drain_command() {
        let (runtime, _rx) = runtime_over(db_with_a_todo_issue("issue-1"));
        let commands_rx = runtime.take_commands_rx().unwrap();

        update_priority_to_urgent(&runtime, "issue-1");

        assert!(matches!(commands_rx.try_recv(), Ok(Command::Drain)));
    }

    #[test]
    fn drain_now_replays_a_pending_update_and_emits_update_again() {
        let fake = FakeTransport::new(vec![
            json!({ "issueUpdate": { "success": true, "issue": null } }),
        ]);
        let (on_event, rx) = on_event_channel();
        let runtime = Runtime::new(
            db_with_a_todo_issue("issue-1"),
            Box::new(FakeSource::new(fake)),
            on_event,
        );

        update_priority_to_urgent(&runtime, "issue-1");
        // The optimistic overlay's own `Update`, from `execute` itself.
        let first = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(first, RuntimeEvent::Update));

        runtime.drain_now().unwrap();

        // The ack's own `Update` follows.
        let second = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(second, RuntimeEvent::Update));

        let conn = runtime.connect().unwrap();
        let pending: i64 = conn
            .query_row("SELECT COUNT(*) FROM op_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pending, 0);
        let priority_label: String = conn
            .query_row(
                "SELECT priority_label FROM issues WHERE id = 'issue-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(priority_label, "Urgent");
    }

    #[test]
    fn drain_now_leaves_a_failed_update_pending_and_the_overlay_still_renders() {
        // No scripted responses: the transport errors, simulating offline.
        let fake = FakeTransport::new(vec![]);
        let (on_event, rx) = on_event_channel();
        let runtime = Runtime::new(
            db_with_a_todo_issue("issue-1"),
            Box::new(FakeSource::new(fake)),
            on_event,
        );

        update_priority_to_urgent(&runtime, "issue-1");
        // The optimistic overlay's own `Update`.
        rx.recv_timeout(Duration::from_secs(1)).unwrap();

        runtime.drain_now().unwrap();
        // The failed drain emits nothing further.
        assert!(rx.try_recv().is_err());

        // The read model still carries the overlay's optimistic edit.
        let page = runtime.execute::<IssuesQuery>(issues_vars()).unwrap();
        assert_eq!(page.nodes[0].priority_label, "Urgent");

        let conn = runtime.connect().unwrap();
        let (attempts, last_error): (i64, Option<String>) = conn
            .query_row(
                "SELECT attempts, last_error FROM op_log WHERE id = 'issue-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(attempts, 1);
        assert!(last_error.is_some());
    }
}
