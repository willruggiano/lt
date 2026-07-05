//! The concrete data runtime: today's `LinearSyncService`, renamed and
//! widened (docs/design/operation-seam-adr.md, "Decision 7"). It owns the
//! sync/login loop, the live subscription registry, and every write; it is
//! the only place in the TUI's runtime that touches `HttpTransport`/cynic
//! directly (behind the injected [`TransportSource`]). `lt-cli` constructs it
//! and injects it into `tui::run`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use lt_storage::db;
use lt_storage::db::{Connection, Database, EntityKey, Read, Upsert};
use lt_types::inputs::{CommentCreateInput, IssueCreateInput};
use lt_types::viewer::ViewerQuery;
use lt_types::{types, viewer};
use lt_upstream::auth::login_non_interactive;
use lt_upstream::auth::refresh::load_or_refresh_token;
use lt_upstream::client::{GraphqlTransport, HttpTransport, execute};

use crate::ops::Refresh;
use crate::subscription::{SubId, Subscription};
use crate::sync::service::{IssueEdit, LoginEvent, OnEvent, RuntimeEvent, SyncEvent};

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

/// A local cache-only re-read: fills a subscription's slot and emits
/// `Updated`.
type RereadFn = Arc<dyn Fn(&Connection) + Send + Sync>;

/// An upstream refresh: fetches and upserts, returning the touched keys (or
/// the subscription's own `reads`, on failure -- "the refresh attempt
/// finished; re-read whatever is cached").
type RefreshFn = Arc<dyn Fn(&Connection, &dyn GraphqlTransport) -> Vec<EntityKey> + Send + Sync>;

/// A live subscription's registration: the concrete `reads` set it was
/// subscribed with, and the operation erased into closures over its typed
/// variables (docs/design/operation-seam-adr.md, "Decision 4").
#[derive(Clone)]
struct Entry {
    reads: Vec<EntityKey>,
    reread: RereadFn,
    refresh: RefreshFn,
}

/// A command sent through the runtime's internal channel: the public methods
/// (`subscribe`/`request_sync`/`login`) plus the login worker's private
/// completion signal, which the loop needs so it -- the sole owner of the
/// pause gate -- decides the follow-up. Every variant is `Copy`: nothing here
/// is worth avoiding a cheap duplication for.
#[derive(Clone, Copy)]
enum Command {
    /// Prompts the loop to upstream-refresh this entry if its `reads` extend
    /// beyond the delta cycle's baseline coverage (Decision 6); a no-op
    /// otherwise. Registration and the caller-side initial read already
    /// happened synchronously on `subscribe`'s caller thread.
    Subscribe(SubId),
    RequestSync,
    Login,
    LoginFinished(bool),
}

/// One decision the loop's core makes in response to a command or a tick.
/// `Copy`: nothing here is worth avoiding a cheap duplication for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Cycle { full: bool },
    RefreshEntry(SubId),
    SpawnLogin,
}

/// The loop's pause gate and login-in-flight guard, decided independent of
/// I/O so cadence/pause/login policy is testable without threads. The watch
/// set round 1 kept here is gone: which entries need a freshness refresh is
/// read straight off the registry (`Entry::reads`), not tracked separately.
struct LoopState {
    /// Set on `NotAuthenticated` or a failed login; cleared by a login
    /// success or `request_sync`. While paused, periodic full/delta cycles
    /// are skipped, but a subscription's freshness refresh still runs.
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
            Command::Subscribe(id) => vec![Action::RefreshEntry(id)],
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

    /// The periodic tick's cycle decision; the caller extends this with a
    /// `RefreshEntry` per registry entry needing a freshness refresh (`run`),
    /// since that needs the registry this pure core does not hold.
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

/// Whether an entry's `reads` extend beyond the delta cycle's baseline
/// coverage (`EntityKey::Issue`), the loop's one piece of freshness policy
/// (docs/design/operation-seam-adr.md, "Decision 6"): a pure-issues
/// subscription is never redundantly re-fetched, since the delta cycle's own
/// upserts feed it through `propagate`.
fn needs_freshness_refresh(reads: &[EntityKey]) -> bool {
    !reads.iter().all(|k| matches!(k, EntityKey::Issue))
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

pub struct Runtime {
    db: Mutex<Database>,
    transports: Box<dyn TransportSource>,
    on_event: Arc<OnEvent>,
    /// The live subscription registry, reachable synchronously from both the
    /// caller thread (registration, write-path propagation, retraction) and
    /// the loop thread (freshness refresh, sync-cycle propagation).
    entries: Arc<Mutex<HashMap<SubId, Entry>>>,
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
            entries: Arc::new(Mutex::new(HashMap::new())),
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
    fn connect(&self) -> Result<Connection> {
        self.db
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .connect()
    }

    /// Best-effort viewer identity via the injected transport source, for the
    /// login worker's direct report (`LoginEvent::Success`, not a cache
    /// read). Ordinary sync cycles persist the viewer through the `Upsert`
    /// seam instead (`sync::persist_viewer`); the header subscribes to
    /// `ViewerQuery` and picks that up through propagation.
    fn viewer_identity(&self) -> Option<viewer::User> {
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
    /// falling back to the wall clock when the read fails or is unparseable
    /// (pre-first-sync, or a corrupt row -- never a panic path).
    fn synced_at(&self) -> chrono::DateTime<chrono::Utc> {
        let raw = self
            .connect()
            .ok()
            .and_then(|conn| db::get_meta(&conn, "last_synced_at").ok().flatten());
        raw.and_then(|ts| chrono::DateTime::parse_from_rfc3339(&ts).ok())
            .map_or_else(chrono::Utc::now, |dt| dt.with_timezone(&chrono::Utc))
    }

    /// Synchronous cache-first subscribe: register the entry (before the
    /// caller-side read, closing the race window between them), read once
    /// for the caller, then prompt the loop to upstream-refresh it if its
    /// `reads` extend beyond the delta cycle's baseline coverage.
    pub fn subscribe<Op>(&self, vars: Op::Variables) -> (Subscription<Op::Output>, Op::Output)
    where
        Op: Read + Upsert + Refresh + 'static,
        Op::Variables: Clone + Send + Sync + 'static,
        Op::Output: Default + Send + 'static,
    {
        let id = SubId::next();
        let reads = Op::reads(&vars);
        let slot: Arc<Mutex<Option<Op::Output>>> = Arc::new(Mutex::new(None));

        let on_event = Arc::clone(&self.on_event);
        let slot_for_reread = Arc::clone(&slot);
        let vars_for_reread = vars.clone();
        let reread: RereadFn =
            Arc::new(
                move |conn: &Connection| match Op::read(conn, &vars_for_reread) {
                    Ok(out) => {
                        *slot_for_reread
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner) = Some(out);
                        on_event(RuntimeEvent::Updated(id));
                    }
                    Err(e) => tracing::warn!(error = %e, "subscription re-read failed"),
                },
            );

        let vars_for_refresh = vars.clone();
        let refresh: RefreshFn = Arc::new(
            move |conn: &Connection, transport: &dyn GraphqlTransport| match Op::refresh(
                conn,
                transport,
                vars_for_refresh.clone(),
            ) {
                Ok(touched) => touched,
                Err(e) => {
                    tracing::warn!(error = %e, "subscription refresh failed");
                    Op::reads(&vars_for_refresh)
                }
            },
        );

        {
            let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            entries.insert(
                id,
                Entry {
                    reads,
                    reread,
                    refresh,
                },
            );
        }

        let initial = self
            .connect()
            .and_then(|conn| Op::read(&conn, &vars))
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "subscription initial read failed");
                Op::Output::default()
            });

        if self.commands_tx.send(Command::Subscribe(id)).is_err() {
            tracing::debug!("subscribe: runtime loop is gone");
        }

        let entries_for_retract = Arc::clone(&self.entries);
        let sub = Subscription {
            id,
            latest: slot,
            retract: Box::new(move |id| {
                entries_for_retract
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&id);
            }),
        };
        (sub, initial)
    }

    /// One-shot local read over a fresh connection: no registration, no live
    /// updates. The search overlay's debounced preview and future CLI use.
    pub fn load<Op: Read>(&self, vars: &Op::Variables) -> Result<Op::Output> {
        let conn = self.connect()?;
        Op::read(&conn, vars)
    }

    /// Re-run every live entry whose `reads` intersects `touched`
    /// (docs/design/operation-seam-adr.md, "Decision 5"). Snapshots the
    /// matching re-read closures before running any of them, so a re-read's
    /// `on_event` callback is never invoked while the registry lock is held
    /// (a callback that drops a subscription would otherwise deadlock on the
    /// same lock).
    fn propagate(&self, touched: &[EntityKey]) {
        let conn = match self.connect() {
            Ok(conn) => conn,
            Err(e) => {
                tracing::warn!(error = %e, "propagate: failed to open db connection");
                return;
            }
        };
        let matches: Vec<RereadFn> = {
            let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            entries
                .values()
                .filter(|e| e.reads.iter().any(|k| touched.contains(k)))
                .map(|e| Arc::clone(&e.reread))
                .collect()
        };
        for reread in matches {
            reread(&conn);
        }
    }

    /// Upstream-refresh one entry, if its `reads` extend beyond the delta
    /// cycle's baseline coverage, then propagate whatever it touched (or its
    /// own `reads`, on failure).
    fn refresh_entry(&self, id: SubId) {
        let entry = {
            let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            entries.get(&id).cloned()
        };
        let Some(entry) = entry else {
            return; // retracted before the loop got to it
        };
        if !needs_freshness_refresh(&entry.reads) {
            return;
        }
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.refresh_entry_body(&entry)
        }));
        match outcome {
            Ok(Ok(touched)) => self.propagate(&touched),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "background refresh failed");
                self.propagate(&entry.reads);
            }
            Err(_) => {
                tracing::warn!("background refresh panicked");
                self.propagate(&entry.reads);
            }
        }
    }

    fn refresh_entry_body(&self, entry: &Entry) -> Result<Vec<EntityKey>> {
        let conn = self.connect()?;
        let transport = self.transports.acquire()?;
        Ok((entry.refresh)(&conn, transport.as_ref()))
    }

    /// Every registered entry whose `reads` extend beyond the delta cycle's
    /// baseline coverage -- the periodic tick's freshness fan-out.
    fn entries_needing_freshness(&self) -> Vec<SubId> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, e)| needs_freshness_refresh(&e.reads))
            .map(|(id, _)| *id)
            .collect()
    }

    /// One full or delta sync cycle: emits `Sync(Started)`, then the cycle's
    /// outcome, then propagates whatever it touched on success (the viewer
    /// identity flows through this same propagation -- `sync::persist_viewer`
    /// touches `Viewer` every cycle, so a live `ViewerQuery` subscription
    /// picks it up without this loop separately fetching or reporting it).
    /// `catch_unwind`-guarded so a panicking sync body surfaces as
    /// `Sync(Error)` and the loop survives.
    fn cycle(&self, full: bool) -> CycleOutcome {
        (self.on_event)(RuntimeEvent::Sync(SyncEvent::Started));

        if matches!(lt_config::load_token(), Ok(None) | Err(_)) {
            (self.on_event)(RuntimeEvent::Sync(SyncEvent::NotAuthenticated));
            return CycleOutcome::NotAuthenticated;
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> Result<Vec<EntityKey>> {
                let conn = self.connect()?;
                let transport = self.transports.acquire()?;
                if full {
                    crate::sync::full::run(&conn, transport.as_ref())
                } else {
                    crate::sync::delta::run(&conn, transport.as_ref())
                }
            },
        ));

        match result {
            Ok(Ok(touched)) => {
                (self.on_event)(RuntimeEvent::Sync(SyncEvent::Done(self.synced_at())));
                self.propagate(&touched);
                CycleOutcome::Done
            }
            Ok(Err(e)) => {
                let msg = e.to_string();
                let brief = msg.lines().next().unwrap_or(&msg).to_string();
                (self.on_event)(RuntimeEvent::Sync(SyncEvent::Error(brief)));
                CycleOutcome::Error
            }
            Err(_) => {
                (self.on_event)(RuntimeEvent::Sync(SyncEvent::Error(
                    "sync worker panicked".to_string(),
                )));
                CycleOutcome::Error
            }
        }
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

    /// Build the optimistic issue fragment for a locally-created issue.
    /// Display names are resolved from the same lookup tables the pickers
    /// read (team, state, member); a `state_id` lookup miss or an absent
    /// `state_id` falls back to a name-keyed id ("Backlog") so the
    /// relational join still resolves a label offline.
    fn optimistic_issue(conn: &Connection, input: &IssueCreateInput) -> Result<types::Issue> {
        let team_name = db::query_teams(conn)?
            .into_iter()
            .find(|t| t.id.inner() == input.team_id)
            .map_or_else(String::new, |t| t.name);

        let (state_id, state_name) = match &input.state_id {
            Some(id) => {
                let name = db::query_team_states(conn, &input.team_id)?
                    .into_iter()
                    .find(|s| s.id.inner() == id)
                    .map_or_else(|| id.clone(), |s| s.name);
                (id.clone(), name)
            }
            None => ("Backlog".to_string(), "Backlog".to_string()),
        };

        let assignee = match &input.assignee_id {
            Some(id) => {
                let name = db::query_team_members(conn, &input.team_id)?
                    .into_iter()
                    .find(|u| u.id.inner() == id)
                    .map_or_else(|| id.clone(), |u| u.name);
                Some(types::User {
                    id: id.clone().into(),
                    name,
                })
            }
            None => None,
        };

        let priority = input
            .priority
            .and_then(|p| u8::try_from(p).ok())
            .unwrap_or(0);
        let now = lt_types::scalars::DateTime(chrono::Utc::now());
        Ok(types::Issue {
            id: db::outbox::temp_id().into(),
            identifier: "NEW".to_string(),
            title: input.title.clone(),
            priority: lt_types::scalars::Priority(priority),
            priority_label: types::priority_u8_to_label(priority).to_string(),
            state: types::WorkflowState {
                id: state_id.into(),
                name: state_name,
            },
            assignee,
            team: types::Team {
                id: input.team_id.clone().into(),
                name: team_name,
            },
            description: input.description.clone(),
            labels: types::IssueLabelConnection { nodes: Vec::new() },
            project: None,
            cycle: None,
            creator: None,
            parent: None,
            created_at: now,
            updated_at: now,
        })
    }
}

impl Runtime {
    /// Execute one action: a full/delta cycle updates `run`'s bookkeeping in
    /// place; an entry refresh and a login spawn are self-contained.
    fn perform<'scope>(
        &'scope self,
        action: Action,
        run: &mut RunLoop,
        scope: &'scope thread::Scope<'scope, '_>,
    ) {
        match action {
            Action::Cycle { full } => self.run_cycle(full, run),
            Action::RefreshEntry(id) => self.refresh_entry(id),
            Action::SpawnLogin => self.spawn_login(scope),
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
                    .unwrap_or_else(|_| LoginEvent::Error("login worker panicked".to_string()));
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
    /// scheduling: the startup sync, the 30s delta cadence, prompt and
    /// periodic freshness refreshes of live subscriptions beyond the delta
    /// cycle's coverage, and full syncs on request.
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
                    Ok(cmd) => run.state.on_command(cmd),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        run.deadline = Instant::now() + SYNC_INTERVAL;
                        let mut actions = run.state.on_timeout();
                        actions.extend(
                            self.entries_needing_freshness()
                                .into_iter()
                                .map(Action::RefreshEntry),
                        );
                        actions
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

    /// Transactional local enqueue, then propagation of the comment's
    /// touched entity (the comment thread only -- creating a comment does
    /// not touch the issues table).
    pub fn create_comment(&self, input: &CommentCreateInput) -> Result<()> {
        let conn = self.connect()?;
        db::outbox::enqueue_comment_create(&conn, &db::outbox::temp_id(), input)?;
        self.propagate(&[EntityKey::Comment {
            issue_id: input.issue_id.clone(),
        }]);
        Ok(())
    }

    /// Transactional local enqueue, then propagation of `Issue`.
    pub fn edit_issue(&self, issue_id: &str, edit: IssueEdit) -> Result<()> {
        let conn = self.connect()?;
        match edit {
            IssueEdit::State { id, name } => {
                db::outbox::enqueue_state_change(&conn, issue_id, &id, &name)?;
            }
            IssueEdit::Priority(p) => {
                db::outbox::enqueue_priority_change(&conn, issue_id, p)?;
            }
            IssueEdit::Assignee(assignee) => {
                db::outbox::enqueue_assignee_change(
                    &conn,
                    issue_id,
                    assignee
                        .as_ref()
                        .map(|(id, name)| (id.as_str(), name.as_str())),
                )?;
            }
        }
        self.propagate(&[EntityKey::Issue]);
        Ok(())
    }

    /// Builds the optimistic fragment, enqueues it, then propagates `Issue`.
    /// Returns the optimistic identifier so the caller can seek to it.
    pub fn create_issue(&self, input: &IssueCreateInput) -> Result<String> {
        let conn = self.connect()?;
        let optimistic = Self::optimistic_issue(&conn, input)?;
        let identifier = optimistic.identifier.clone();
        db::outbox::enqueue_issue_create(&conn, &optimistic, input)?;
        self.propagate(&[EntityKey::Issue]);
        Ok(identifier)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc as std_mpsc;

    use lt_types::issues::{IssuesQuery, IssuesVariables};
    use lt_types::members::{TeamMembersQuery, TeamVariables as MembersTeamVariables};
    use lt_types::states::{TeamStatesQuery, TeamVariables as StatesTeamVariables};
    use lt_types::teams::TeamsQuery;
    use lt_upstream::client::FakeTransport;
    use serde_json::json;

    use super::*;

    fn sub_id() -> SubId {
        SubId::next()
    }

    // -- LoopState (pure decisions) ------------------------------------

    #[test]
    fn subscribe_command_prompts_a_refresh_entry_action() {
        let mut state = LoopState::new();
        let id = sub_id();
        assert_eq!(
            state.on_command(Command::Subscribe(id)),
            vec![Action::RefreshEntry(id)]
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
    fn login_finished_failure_pauses() {
        let mut state = LoopState::new();
        state.on_command(Command::Login);

        let actions = state.on_command(Command::LoginFinished(false));
        assert_eq!(actions, Vec::new());
        assert_eq!(state.on_timeout(), Vec::new()); // paused: no cycle
    }

    // -- needs_freshness_refresh ----------------------------------------

    #[test]
    fn pure_issue_reads_never_need_a_freshness_refresh() {
        assert!(!needs_freshness_refresh(&[EntityKey::Issue]));
    }

    #[test]
    fn reads_beyond_issue_need_a_freshness_refresh() {
        assert!(needs_freshness_refresh(&[EntityKey::Teams]));
        assert!(needs_freshness_refresh(&[
            EntityKey::Issue,
            EntityKey::Teams
        ]));
    }

    // -- Runtime: subscribe / propagate / retract, thread-free -----------

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

    #[test]
    fn subscribe_returns_the_synchronous_initial_read() {
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

        let (_sub, initial) = runtime.subscribe::<TeamsQuery>(());

        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].name, "Eng");
    }

    #[test]
    fn dropping_a_subscription_retracts_its_registry_entry() {
        let db = Database::memory().unwrap();
        let (runtime, _rx) = runtime_over(db);

        let (sub, _initial) = runtime.subscribe::<TeamsQuery>(());
        let id = sub.id();
        assert!(
            runtime
                .entries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains_key(&id)
        );

        drop(sub);

        assert!(
            !runtime
                .entries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains_key(&id)
        );
    }

    #[test]
    fn create_issue_propagates_to_a_live_issues_subscription() {
        let db = Database::memory().unwrap();
        let (runtime, rx) = runtime_over(db);
        let (sub, _initial) = runtime.subscribe::<IssuesQuery>(IssuesVariables {
            filter: None,
            sort: None,
            first: None,
            after: None,
        });

        let input = IssueCreateInput {
            title: "New issue".to_string(),
            team_id: "t1".to_string(),
            description: None,
            state_id: None,
            priority: None,
            assignee_id: None,
        };
        let identifier = runtime.create_issue(&input).unwrap();

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Updated(id) if id == sub.id()));
        let page = sub.take().unwrap();
        assert_eq!(page.nodes.len(), 1);
        assert_eq!(page.nodes[0].identifier, identifier);
    }

    #[test]
    fn create_comment_propagates_to_a_live_issue_detail_subscription() {
        let db = Database::memory().unwrap();
        {
            let conn = db.connect().unwrap();
            db::upsert_issues(&conn, &[db::outbox::sample_base_issue("issue-1")]).unwrap();
        }
        let (runtime, rx) = runtime_over(db);
        let (sub, initial) = runtime.subscribe::<lt_types::detail::IssueDetailQuery>(
            lt_types::detail::IssueDetailVariables {
                id: "issue-1".to_string(),
            },
        );
        assert!(initial.unwrap().comments.is_empty());

        runtime
            .create_comment(&CommentCreateInput {
                issue_id: "issue-1".to_string(),
                body: "hello".to_string(),
            })
            .unwrap();

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Updated(id) if id == sub.id()));
        let data = sub.take().unwrap().unwrap();
        assert_eq!(data.comments.len(), 1);
        assert_eq!(data.comments[0].body, "hello");
    }

    #[test]
    fn edit_issue_refreshes_an_open_detail_pane_for_a_different_issue() {
        // Any `Issue`-touching write refreshes every open detail pane:
        // `IssueDetailQuery` reads `Issue` broadly, not scoped to its own id
        // (docs/design/operation-seam-adr.md, user-visible change 3).
        let db = Database::memory().unwrap();
        {
            let conn = db.connect().unwrap();
            db::upsert_issues(
                &conn,
                &[
                    db::outbox::sample_base_issue("issue-1"),
                    db::outbox::sample_base_issue("issue-2"),
                ],
            )
            .unwrap();
        }
        let (runtime, rx) = runtime_over(db);
        let (sub, _initial) = runtime.subscribe::<lt_types::detail::IssueDetailQuery>(
            lt_types::detail::IssueDetailVariables {
                id: "issue-1".to_string(),
            },
        );

        runtime
            .edit_issue("issue-2", IssueEdit::Priority(1))
            .unwrap();

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Updated(id) if id == sub.id()));
    }

    // -- freshness refresh: beyond-Issue subscriptions refresh upstream ---

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

    #[test]
    fn refresh_entry_refreshes_and_propagates_when_reads_extend_beyond_issue() {
        let db = Database::memory().unwrap();
        let fake = FakeTransport::new(vec![json!({ "team": { "states": { "nodes": [
            { "id": "s1", "name": "Todo", "position": 1.0 }
        ] } } })]);
        let (on_event, rx) = on_event_channel();
        let runtime = Runtime::new(db, Box::new(FakeSource::new(fake)), on_event);
        let (sub, _initial) = runtime.subscribe::<TeamStatesQuery>(StatesTeamVariables {
            team_id: "t1".to_string(),
        });

        // Call the loop's private entry point directly rather than starting
        // the (unbounded) `run` loop, so the test stays thread-free.
        runtime.refresh_entry(sub.id());

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Updated(id) if id == sub.id()));
        let states = sub.take().unwrap();
        assert_eq!(states[0].name, "Todo");
    }

    #[test]
    fn refresh_entry_is_a_noop_for_a_pure_issue_subscription() {
        let db = Database::memory().unwrap();
        let (runtime, rx) = runtime_over(db);
        let (sub, _initial) = runtime.subscribe::<IssuesQuery>(IssuesVariables {
            filter: None,
            sort: None,
            first: None,
            after: None,
        });

        runtime.refresh_entry(sub.id());

        // No upstream refresh attempted (would need a real transport), and
        // no propagate -- only the `Subscribe` command's send happened.
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn team_members_query_can_also_subscribe_and_refresh() {
        let db = Database::memory().unwrap();
        let fake = FakeTransport::new(vec![json!({ "team": { "members": { "nodes": [
            { "id": "u1", "name": "Ada" }
        ] } } })]);
        let (on_event, rx) = on_event_channel();
        let runtime = Runtime::new(db, Box::new(FakeSource::new(fake)), on_event);
        let (sub, _initial) = runtime.subscribe::<TeamMembersQuery>(MembersTeamVariables {
            team_id: "t1".to_string(),
        });

        runtime.refresh_entry(sub.id());

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Updated(id) if id == sub.id()));
        assert_eq!(sub.take().unwrap()[0].name, "Ada");
    }

    #[test]
    fn viewer_query_subscription_refreshes_and_updates_the_header() {
        // The header's `ViewerQuery` subscription lives at the App level,
        // not on a view; its live-update path is the same beyond-Issue
        // freshness refresh every other composed subscription uses.
        let db = Database::memory().unwrap();
        let fake = FakeTransport::new(vec![json!({
            "viewer": { "id": "u1", "name": "Ada", "organization": { "name": "Acme", "urlKey": "acme" } }
        })]);
        let (on_event, rx) = on_event_channel();
        let runtime = Runtime::new(db, Box::new(FakeSource::new(fake)), on_event);
        let (sub, initial) = runtime.subscribe::<lt_types::viewer::ViewerQuery>(());
        assert!(initial.is_none());

        runtime.refresh_entry(sub.id());

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Updated(id) if id == sub.id()));
        assert_eq!(sub.take().unwrap().unwrap().name, "Ada");
    }
}
