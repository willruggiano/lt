//! The concrete data runtime. It owns the sync/login loop, the live
//! subscription registry, and every write; it is the only place in the
//! TUI's runtime that touches `HttpTransport`/cynic directly (behind the
//! injected [`TransportSource`]). `lt-cli` constructs it and injects it into
//! `tui::run`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use lt_storage::db;
use lt_storage::db::{Connection, Database, EntityKey, Mutate, Read, Upsert};
use lt_types::comments::{CommentCreateMutation, CommentCreateVariables};
use lt_types::graphql::GraphqlOperation;
use lt_types::inputs::{CommentCreateInput, IssueCreateInput};
use lt_types::issues::{
    IssueCreateMutation, IssueCreateVariables, IssueUpdateMutation, IssueUpdateVariables,
};
use lt_types::viewer::ViewerQuery;
use lt_types::{types, viewer};
use lt_upstream::auth::login_non_interactive;
use lt_upstream::auth::refresh::load_or_refresh_token;
use lt_upstream::client::{GraphqlTransport, HttpTransport, execute};

use crate::ops::Refresh;
use crate::subscription::{Subscription, SubscriptionKey};
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
    Subscribe(SubscriptionKey),
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
    RefreshEntry(SubscriptionKey),
    SpawnLogin,
    Drain,
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

/// [`Runtime::search`]'s outcome: distinguishes an entirely empty cache from
/// a stale FTS shadow index whose results are an approximate
/// title-substring fallback rather than a ranked FTS5 match.
pub enum SearchOutcome {
    /// No issues are cached at all.
    NoIndex,
    Results {
        issues: Vec<lt_types::types::Issue>,
        approximate: bool,
    },
}

/// [`Runtime::create_issue_now`]'s outcome: the create was acked
/// synchronously, or the transport was unreachable and the command stays
/// queued for the next sync (the CLI's offline case).
pub enum CreateIssueOutcome {
    Created(Box<types::Issue>),
    Queued(String),
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
    /// The live subscription registry, reachable synchronously from both the
    /// caller thread (registration, write-path propagation, retraction) and
    /// the loop thread (freshness refresh, sync-cycle propagation).
    entries: Arc<Mutex<HashMap<SubscriptionKey, Entry>>>,
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
        let key = SubscriptionKey::next();
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
                        on_event(RuntimeEvent::Updated(key));
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
                key,
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

        if self.commands_tx.send(Command::Subscribe(key)).is_err() {
            tracing::debug!("subscribe: runtime loop is gone");
        }

        let entries_for_retract = Arc::clone(&self.entries);
        let sub = Subscription {
            key,
            latest: slot,
            retract: Box::new(move |key| {
                entries_for_retract
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&key);
            }),
        };
        (sub, initial)
    }

    /// One-shot local read over a fresh connection: no registration, no live
    /// updates.
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
    fn refresh_entry(&self, id: SubscriptionKey) {
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
    fn entries_needing_freshness(&self) -> Vec<SubscriptionKey> {
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
                (self.on_event)(RuntimeEvent::Sync(SyncEvent::Done(self.last_synced_at())));
                self.propagate(&touched);
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

    /// Immediately replay every pending outbox command upstream, then
    /// propagate whatever it touched. The single drain body: runs on the loop
    /// thread (`Action::Drain`, triggered by a caller-side mutation) so it
    /// shares the loop's serialization of all base writes.
    pub fn drain_now(&self) -> Result<Vec<EntityKey>> {
        let conn = self.connect()?;
        let transport = self.transports.acquire()?;
        let touched = crate::sync::drain::drain(&conn, transport.as_ref())?;
        self.propagate(&touched);
        Ok(touched)
    }

    /// The shared body of [`Runtime::sync_full`]/[`Runtime::sync_delta`]:
    /// connect, acquire a transport, run the requested sync body, then
    /// propagate whatever it touched. Mirrors `cycle` minus its event
    /// emission -- for a caller that never starts `run` (e.g. `lt-cli`'s `lt
    /// sync`).
    fn sync_now(&self, full: bool) -> Result<Vec<EntityKey>> {
        let conn = self.connect()?;
        let transport = self.transports.acquire()?;
        let touched = if full {
            crate::sync::full::run(&conn, transport.as_ref())?
        } else {
            crate::sync::delta::run(&conn, transport.as_ref())?
        };
        self.propagate(&touched);
        Ok(touched)
    }

    pub fn sync_full(&self) -> Result<Vec<EntityKey>> {
        self.sync_now(true)
    }

    /// The delta counterpart of [`Runtime::sync_full`].
    pub fn sync_delta(&self) -> Result<Vec<EntityKey>> {
        self.sync_now(false)
    }

    /// A synchronous, caller-driven upstream refresh of a single operation
    /// (e.g. `lt-cli`'s explicit read commands): fetch and upsert via
    /// `Op::refresh`, then propagate whatever it touched. Unlike a
    /// subscription's background freshness refresh, errors are returned
    /// rather than logged.
    pub fn refresh<Op: Refresh>(&self, vars: Op::Variables) -> Result<Vec<EntityKey>> {
        let conn = self.connect()?;
        let transport = self.transports.acquire()?;
        let touched = Op::refresh(&conn, transport.as_ref(), vars)?;
        self.propagate(&touched);
        Ok(touched)
    }

    /// The local full-text search a caller runs without holding a
    /// `Connection`: an empty cache reports [`SearchOutcome::NoIndex`]; a
    /// stale FTS shadow index (present issues, no FTS rows) falls back to an
    /// approximate title-substring match rather than returning nothing.
    pub fn search(&self, query: &str, limit: usize) -> Result<SearchOutcome> {
        let conn = self.connect()?;
        if db::count_issues(&conn)? == 0 {
            return Ok(SearchOutcome::NoIndex);
        }
        let approximate = db::count_fts_rows(&conn).unwrap_or(0) == 0;
        let issues = if approximate {
            db::search_issues_like(&conn, query, limit)?
        } else {
            db::search_issues(&conn, query, limit)?
        };
        Ok(SearchOutcome::Results {
            issues,
            approximate,
        })
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
            Action::Drain => self.perform_drain(),
        }
    }

    /// `Action::Drain`'s body: run the drain, panic-guarded like a sync cycle
    /// since it shares the same DB/network I/O on the loop thread.
    fn perform_drain(&self) {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.drain_now())) {
            Ok(Ok(_)) => {}
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

    /// Every write's shared tail: transactional local enqueue, propagation of
    /// whatever it touched, then a prompt for the loop to immediately drain
    /// the outbox rather than waiting for the next sync cycle.
    fn enqueue_and_propagate<M: Mutate>(&self, vars: M::Variables) -> Result<Vec<EntityKey>> {
        let conn = self.connect()?;
        let touched = M::enqueue(&conn, vars)?;
        self.propagate(&touched);
        if self.commands_tx.send(Command::Drain).is_err() {
            tracing::debug!("enqueue_and_propagate: runtime loop is gone");
        }
        Ok(touched)
    }

    /// The comment thread only -- creating a comment does not touch the
    /// issues table.
    pub fn create_comment(&self, input: &CommentCreateInput) -> Result<()> {
        self.enqueue_and_propagate::<CommentCreateMutation>(CommentCreateVariables {
            input: input.clone(),
        })?;
        Ok(())
    }

    pub fn update_issue(&self, vars: IssueUpdateVariables) -> Result<()> {
        self.enqueue_and_propagate::<IssueUpdateMutation>(vars)?;
        Ok(())
    }

    /// Returns the optimistic identifier so the caller can seek to it.
    pub fn create_issue(&self, input: &IssueCreateInput) -> Result<String> {
        self.enqueue_and_propagate::<IssueCreateMutation>(IssueCreateVariables {
            input: input.clone(),
        })?;
        Ok(db::outbox::OPTIMISTIC_ISSUE_IDENTIFIER.to_string())
    }

    /// The CLI's synchronous create: enqueue through the same outbox path as
    /// [`Runtime::create_issue`], then immediately replay that command
    /// (instead of handing it to the loop's `Command::Drain`, since `lt
    /// issues new` has no running loop to hand it off to) and report the
    /// server's real issue. A replay failure (offline) leaves the command
    /// pending for the next sync and reports the optimistic identifier
    /// instead of an error.
    pub fn create_issue_now(&self, vars: IssueCreateVariables) -> Result<CreateIssueOutcome> {
        let conn = self.connect()?;
        let touched = IssueCreateMutation::enqueue(&conn, vars)?;
        self.propagate(&touched);

        // `enqueue` never coalesces a create (each mints its own temp id), so
        // the newest pending `issueCreate` command is the one just committed.
        let op = db::outbox::pending_operations(&conn)?
            .into_iter()
            .filter(|op| op.op_type == IssueCreateMutation::NAME)
            .max_by_key(|op| op.seq)
            .context("issue-create command missing immediately after its own enqueue")?;

        let replayed = self.transports.acquire().and_then(|transport| {
            crate::sync::drain::replay_op::<IssueCreateMutation>(&conn, transport.as_ref(), &op)
        });

        match replayed {
            Ok((issue, keys)) => {
                self.propagate(&keys);
                Ok(CreateIssueOutcome::Created(Box::new(issue)))
            }
            Err(e) => {
                db::outbox::record_error(&conn, op.seq, &e.to_string())?;
                Ok(CreateIssueOutcome::Queued(
                    db::outbox::OPTIMISTIC_ISSUE_IDENTIFIER.to_string(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc as std_mpsc;

    use lt_types::issues::{IssuesQuery, IssuesVariables, sample_issue_node};
    use lt_types::members::{TeamMembersQuery, TeamVariables as MembersTeamVariables};
    use lt_types::states::{TeamStatesQuery, TeamVariables as StatesTeamVariables};
    use lt_types::teams::TeamsQuery;
    use lt_types::types;
    use lt_upstream::client::FakeTransport;
    use serde_json::json;

    use super::*;

    fn sub_id() -> SubscriptionKey {
        SubscriptionKey::next()
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
    fn drain_command_prompts_a_drain_action() {
        let mut state = LoopState::new();
        assert_eq!(state.on_command(Command::Drain), vec![Action::Drain]);
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
        let id = sub.key();
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
        {
            let conn = db.connect().unwrap();
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
        assert!(matches!(ev, RuntimeEvent::Updated(id) if id == sub.key()));
        let page = sub.take().unwrap();
        assert_eq!(page.nodes.len(), 1);
        assert_eq!(page.nodes[0].identifier, identifier);
    }

    #[test]
    fn create_comment_propagates_to_a_live_issue_detail_subscription() {
        let db = Database::memory().unwrap();
        {
            let conn = db.connect().unwrap();
            // `sample_base_issue`'s state must already be locally known (sync
            // owns workflow states; issue upserts never write them).
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
        assert!(matches!(ev, RuntimeEvent::Updated(id) if id == sub.key()));
        let data = sub.take().unwrap().unwrap();
        assert_eq!(data.comments.len(), 1);
        assert_eq!(data.comments[0].body, "hello");
    }

    #[test]
    fn update_issue_refreshes_an_open_detail_pane_for_a_different_issue() {
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
            .update_issue(IssueUpdateVariables {
                id: "issue-2".to_string(),
                input: lt_types::inputs::IssueUpdateInput {
                    priority: Some(1),
                    ..Default::default()
                },
            })
            .unwrap();

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Updated(id) if id == sub.key()));
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

    /// A single scripted `team.states` page, shared by every test that drives
    /// a `TeamStatesQuery` refresh (background or explicit).
    fn team_states_page_transport() -> FakeTransport {
        FakeTransport::new(vec![json!({ "team": { "states": { "nodes": [
            { "id": "s1", "name": "Todo", "position": 1.0 }
        ] } } })])
    }

    #[test]
    fn refresh_entry_refreshes_and_propagates_when_reads_extend_beyond_issue() {
        let db = Database::memory().unwrap();
        let (on_event, rx) = on_event_channel();
        let runtime = Runtime::new(
            db,
            Box::new(FakeSource::new(team_states_page_transport())),
            on_event,
        );
        let (sub, _initial) = runtime.subscribe::<TeamStatesQuery>(StatesTeamVariables {
            team_id: "t1".to_string(),
        });

        // Call the loop's private entry point directly rather than starting
        // the (unbounded) `run` loop, so the test stays thread-free.
        runtime.refresh_entry(sub.key());

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Updated(id) if id == sub.key()));
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

        runtime.refresh_entry(sub.key());

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

        runtime.refresh_entry(sub.key());

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Updated(id) if id == sub.key()));
        assert_eq!(sub.take().unwrap()[0].name, "Ada");
    }

    #[test]
    fn viewer_query_subscription_refreshes_and_updates_the_header() {
        // The header's `ViewerQuery` subscription lives at the App level,
        // not on a view; its live-update path is the same beyond-Issue
        // freshness refresh every other composed subscription uses.
        let db = Database::memory().unwrap();
        let fake = FakeTransport::new(vec![json!({
            "viewer": { "id": "u1", "name": "Ada", "organization": { "id": "o1", "name": "Acme", "urlKey": "acme" } }
        })]);
        let (on_event, rx) = on_event_channel();
        let runtime = Runtime::new(db, Box::new(FakeSource::new(fake)), on_event);
        let (sub, initial) = runtime.subscribe::<lt_types::viewer::ViewerQuery>(());
        assert!(initial.is_none());

        runtime.refresh_entry(sub.key());

        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Updated(id) if id == sub.key()));
        assert_eq!(sub.take().unwrap().unwrap().user.name, "Ada");
    }

    // -- refresh: a synchronous, caller-driven upstream refresh ----------

    #[test]
    fn refresh_refreshes_and_propagates_to_a_live_subscription() {
        let db = Database::memory().unwrap();
        let (on_event, rx) = on_event_channel();
        let runtime = Runtime::new(
            db,
            Box::new(FakeSource::new(team_states_page_transport())),
            on_event,
        );
        let vars = StatesTeamVariables {
            team_id: "t1".to_string(),
        };
        let (sub, _initial) = runtime.subscribe::<TeamStatesQuery>(vars.clone());

        let touched = runtime.refresh::<TeamStatesQuery>(vars).unwrap();

        assert_eq!(
            touched,
            vec![EntityKey::WorkflowStates {
                team_id: "t1".to_string()
            }]
        );
        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Updated(id) if id == sub.key()));
        assert_eq!(sub.take().unwrap()[0].name, "Todo");
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

        let touched = runtime.sync_full().unwrap();

        assert!(touched.contains(&EntityKey::Issue));
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

        let touched = runtime.sync_delta().unwrap();

        assert!(touched.contains(&EntityKey::Issue));
        let conn = runtime.connect().unwrap();
        assert!(db::query_issue_by_id(&conn, "1").unwrap().is_some());
        assert!(runtime.last_synced_at().is_some());
    }

    // -- search: the local FTS-vs-LIKE seam --------------------------------

    #[test]
    fn search_finds_a_seeded_issue() {
        let db = Database::memory().unwrap();
        {
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
            db::upsert_issues(&conn, &[db::outbox::sample_base_issue("issue-1")]).unwrap();
        }
        let (runtime, _rx) = runtime_over(db);

        let outcome = runtime.search("issue", 10).unwrap();

        match outcome {
            SearchOutcome::Results {
                issues,
                approximate,
            } => {
                assert!(!approximate);
                assert_eq!(issues.len(), 1);
            }
            SearchOutcome::NoIndex => panic!("expected results, got NoIndex"),
        }
    }

    #[test]
    fn search_reports_no_index_over_an_empty_cache() {
        let (runtime, _rx) = runtime_over(Database::memory().unwrap());

        assert!(matches!(
            runtime.search("anything", 10).unwrap(),
            SearchOutcome::NoIndex
        ));
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
        db::upsert_issues(&conn, &[db::outbox::sample_base_issue(id)]).unwrap();
        db
    }

    fn update_priority_to_urgent(runtime: &Runtime, id: &str) {
        runtime
            .update_issue(IssueUpdateVariables {
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
    fn drain_now_replays_a_pending_update_and_reaches_the_subscription_again() {
        let fake = FakeTransport::new(vec![
            json!({ "issueUpdate": { "success": true, "issue": null } }),
        ]);
        let (on_event, rx) = on_event_channel();
        let runtime = Runtime::new(
            db_with_a_todo_issue("issue-1"),
            Box::new(FakeSource::new(fake)),
            on_event,
        );
        let (sub, _initial) = runtime.subscribe::<IssuesQuery>(IssuesVariables {
            filter: None,
            sort: None,
            first: None,
            after: None,
        });

        update_priority_to_urgent(&runtime, "issue-1");
        // The optimistic overlay's own propagation, from `update_issue` itself.
        let first = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(first, RuntimeEvent::Updated(id) if id == sub.key()));

        let touched = runtime.drain_now().unwrap();
        assert_eq!(touched, vec![EntityKey::Issue]);

        // The ack's own propagation reaches the subscription a second time.
        let second = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(second, RuntimeEvent::Updated(id) if id == sub.key()));

        let conn = runtime.connect().unwrap();
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM outbox WHERE status = 'pending'",
                [],
                |r| r.get(0),
            )
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
        let (sub, _initial) = runtime.subscribe::<IssuesQuery>(IssuesVariables {
            filter: None,
            sort: None,
            first: None,
            after: None,
        });

        update_priority_to_urgent(&runtime, "issue-1");
        // The optimistic overlay's own propagation.
        rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let touched = runtime.drain_now().unwrap();
        assert!(touched.is_empty());
        // The failed drain propagates nothing further.
        assert!(rx.try_recv().is_err());

        // The read model still carries the overlay's optimistic edit.
        let page = sub.take().unwrap();
        assert_eq!(page.nodes[0].priority_label, "Urgent");

        let conn = runtime.connect().unwrap();
        let (attempts, last_error): (i64, Option<String>) = conn
            .query_row(
                "SELECT attempts, last_error FROM outbox WHERE entity_id = 'issue-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(attempts, 1);
        assert!(last_error.is_some());
    }

    // -- create_issue_now: the CLI's synchronous create -------------------

    fn db_with_a_todo_state_for(team_id: &str) -> Database {
        let db = Database::memory().unwrap();
        let conn = db.connect().unwrap();
        db::upsert_team_state(
            &conn,
            team_id,
            &types::WorkflowState {
                id: "s-todo".into(),
                name: "Todo".to_string(),
                position: 1.0,
            },
        )
        .unwrap();
        db
    }

    fn new_issue_input() -> IssueCreateInput {
        IssueCreateInput {
            title: "New issue".to_string(),
            team_id: "ENG".to_string(),
            description: None,
            state_id: None,
            priority: None,
            assignee_id: None,
        }
    }

    #[test]
    fn create_issue_now_returns_the_server_issue_and_clears_the_outbox() {
        let (on_event, _rx) = on_event_channel();
        let fake = FakeTransport::new(vec![
            json!({ "issueCreate": { "success": true, "issue": sample_issue_node("real-1") } }),
        ]);
        let runtime = Runtime::new(
            db_with_a_todo_state_for("ENG"),
            Box::new(FakeSource::new(fake)),
            on_event,
        );

        let outcome = runtime
            .create_issue_now(IssueCreateVariables {
                input: new_issue_input(),
            })
            .unwrap();

        let issue = match outcome {
            CreateIssueOutcome::Created(issue) => issue,
            CreateIssueOutcome::Queued(id) => panic!("expected Created, got Queued({id})"),
        };
        assert_eq!(issue.identifier, "ENG-real-1");

        let conn = runtime.connect().unwrap();
        let ident: String = conn
            .query_row(
                "SELECT identifier FROM issues WHERE id = 'real-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ident, "ENG-real-1");
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM outbox WHERE status = 'pending'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 0);
    }

    #[test]
    fn create_issue_now_offline_queues_and_the_optimistic_row_still_renders() {
        // No scripted responses: the transport errors, simulating offline.
        let fake = FakeTransport::new(vec![]);
        let (on_event, rx) = on_event_channel();
        let runtime = Runtime::new(
            db_with_a_todo_state_for("ENG"),
            Box::new(FakeSource::new(fake)),
            on_event,
        );
        let (sub, _initial) = runtime.subscribe::<IssuesQuery>(IssuesVariables {
            filter: None,
            sort: None,
            first: None,
            after: None,
        });

        let outcome = runtime
            .create_issue_now(IssueCreateVariables {
                input: new_issue_input(),
            })
            .unwrap();

        match outcome {
            CreateIssueOutcome::Queued(identifier) => {
                assert_eq!(identifier, db::outbox::OPTIMISTIC_ISSUE_IDENTIFIER);
            }
            CreateIssueOutcome::Created(issue) => {
                panic!("expected Queued, got Created({})", issue.identifier)
            }
        }

        // The optimistic overlay's own propagation, from `enqueue` itself.
        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(ev, RuntimeEvent::Updated(id) if id == sub.key()));
        let page = sub.take().unwrap();
        assert_eq!(page.nodes.len(), 1);
        assert_eq!(
            page.nodes[0].identifier,
            db::outbox::OPTIMISTIC_ISSUE_IDENTIFIER
        );

        let conn = runtime.connect().unwrap();
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM outbox WHERE status = 'pending'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1);
    }
}
