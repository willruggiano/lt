//! The concrete [`SyncService`], backed by `lt-upstream`.
//!
//! This is the only place in the TUI's runtime that touches
//! `HttpTransport`/cynic. `lt-cli` constructs it and injects it into
//! `tui::run`, which lets the TUI drive sync/login and modal reads without
//! depending on `lt-upstream`.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use lt_storage::db;
use lt_storage::db::{Connection, Database};
use lt_types::inputs::{CommentCreateInput, IssueCreateInput};
use lt_types::viewer::ViewerQuery;
use lt_types::{types, viewer};
use lt_upstream::auth::login_non_interactive;
use lt_upstream::auth::refresh::load_or_refresh_token;
use lt_upstream::client::{HttpTransport, execute};

use crate::sync::service::{
    IssueEdit, LoginEvent, OnEvent, RuntimeEvent, Scope, StateEvent, SyncEvent, SyncService,
};

/// The loop's periodic delta-sync cadence.
const SYNC_INTERVAL: Duration = Duration::from_secs(30);

pub struct LinearSyncService {
    db: Mutex<Database>,
    on_event: OnEvent,
    commands_tx: mpsc::Sender<Command>,
    /// `run` takes this once, at the start of its loop; `None` after that
    /// signals a second call, which is a programming error (the trait
    /// documents `run` as called at most once, by `lt-cli`).
    commands_rx: Mutex<Option<mpsc::Receiver<Command>>>,
}

/// A command sent through the service's internal channel: the public trait
/// methods (`watch`/`unwatch`/`request_sync`/`login`) plus the login
/// worker's private completion signal, which the loop needs so it -- the
/// sole owner of the watch set and the pause gate -- decides the follow-up.
enum Command {
    Watch(Scope),
    Unwatch(Scope),
    RequestSync,
    Login,
    LoginFinished(bool),
}

/// One decision the loop's core makes in response to a command or a tick.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Cycle { full: bool },
    Refresh(Scope),
    SpawnLogin,
}

/// The loop's watch set, pause gate, and login-in-flight guard, decided
/// independent of I/O so cadence/pause/watch policy is testable without
/// threads.
struct LoopState {
    /// A counted set: two views may watch the same scope; unwatch decrements.
    watched: HashMap<Scope, u32>,
    /// Set on `NotAuthenticated` or a failed login; cleared by a login
    /// success or `request_sync`. While paused, periodic full/delta cycles
    /// are skipped, but watched-scope refreshes still run.
    paused: bool,
    login_in_flight: bool,
}

impl LoopState {
    fn new() -> Self {
        Self {
            watched: HashMap::new(),
            paused: false,
            login_in_flight: false,
        }
    }

    fn on_command(&mut self, cmd: Command) -> Vec<Action> {
        match cmd {
            Command::Watch(scope) => {
                *self.watched.entry(scope.clone()).or_insert(0) += 1;
                vec![Action::Refresh(scope)]
            }
            Command::Unwatch(scope) => {
                if let Some(count) = self.watched.get_mut(&scope) {
                    *count -= 1;
                    if *count == 0 {
                        self.watched.remove(&scope);
                    }
                }
                Vec::new()
            }
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

    /// The periodic tick: a delta cycle unless paused, plus every watched
    /// scope -- prompt-triggered refreshes still run while paused.
    fn on_timeout(&self) -> Vec<Action> {
        let mut actions = Vec::new();
        if !self.paused {
            actions.push(Action::Cycle { full: false });
        }
        actions.extend(self.watched.keys().cloned().map(Action::Refresh));
        actions
    }

    fn mark_not_authenticated(&mut self) {
        self.paused = true;
    }
}

/// A sync cycle's outcome, for the loop to update its identity/pause
/// bookkeeping. Distinct from [`SyncEvent`]: this is loop-internal, not the
/// wire vocabulary `on_event` reports.
enum CycleOutcome {
    Done { got_identity: bool },
    Error,
    NotAuthenticated,
}

/// `run`'s mutable loop state, bundled so the per-action handlers stay under
/// the argument-count lint.
struct RunLoop {
    state: LoopState,
    deadline: Instant,
    /// Whether a cycle has delivered the viewer identity since the last
    /// `NotAuthenticated` (or since process start).
    identity_delivered: bool,
}

impl RunLoop {
    fn new() -> Self {
        Self {
            state: LoopState::new(),
            // Timing out immediately performs the startup sync.
            deadline: Instant::now(),
            identity_delivered: false,
        }
    }
}

impl LinearSyncService {
    pub fn new(db: Database, on_event: OnEvent) -> Self {
        let (commands_tx, commands_rx) = mpsc::channel();
        Self {
            db: Mutex::new(db),
            on_event,
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

    /// Best-effort viewer identity from the stored token.
    fn viewer_identity() -> Option<viewer::User> {
        let token = match lt_config::load_token() {
            Ok(Some(token)) => token,
            Ok(None) => return None,
            Err(e) => {
                tracing::debug!(error = %e, "viewer_identity: failed to load stored token");
                return None;
            }
        };
        match execute::<ViewerQuery>(&HttpTransport::new(token.access_token), ()) {
            Ok(viewer) => Some(viewer),
            Err(e) => {
                tracing::debug!(error = %e, "viewer_identity: viewer query failed");
                None
            }
        }
    }

    /// A transport with a fresh (auto-refreshed) token for a live read.
    fn transport() -> Result<HttpTransport> {
        let token = load_or_refresh_token()?;
        Ok(HttpTransport::new(token.access_token))
    }

    /// One full or delta sync cycle: emits `Sync(Started)`, then the cycle's
    /// outcome, then `State(Issues)` on success. `catch_unwind`-guarded so a
    /// panicking sync body surfaces as `Sync(Error)` and the loop survives.
    fn cycle(&self, full: bool, fetch_identity: bool) -> CycleOutcome {
        (self.on_event)(RuntimeEvent::Sync(SyncEvent::Started));

        if matches!(lt_config::load_token(), Ok(None) | Err(_)) {
            (self.on_event)(RuntimeEvent::Sync(SyncEvent::NotAuthenticated));
            return CycleOutcome::NotAuthenticated;
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if full {
                crate::sync::full::run()
            } else {
                crate::sync::delta::run()
            }
        }));

        match result {
            Ok(Ok(())) => {
                let viewer = if fetch_identity {
                    Self::viewer_identity()
                } else {
                    None
                };
                let got_identity = viewer.is_some();
                (self.on_event)(RuntimeEvent::Sync(SyncEvent::Done(viewer)));
                (self.on_event)(RuntimeEvent::State(StateEvent::Issues));
                CycleOutcome::Done { got_identity }
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

    /// Refresh one watched scope: always ends in its `State` event, even on
    /// failure -- "the refresh attempt finished; re-read whatever is
    /// cached." `catch_unwind`-guarded like `cycle`.
    fn refresh(&self, scope: &Scope) {
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.refresh_body(scope)));
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("background refresh failed: {e:#}"),
            Err(_) => tracing::warn!("background refresh panicked"),
        }
        let event = match scope.clone() {
            Scope::Comments { issue_id } => StateEvent::Comments { issue_id },
            Scope::Teams => StateEvent::Teams,
            Scope::Team { team_id } => StateEvent::Team { team_id },
        };
        (self.on_event)(RuntimeEvent::State(event));
    }

    fn refresh_body(&self, scope: &Scope) -> Result<()> {
        let conn = self.connect()?;
        let transport = Self::transport()?;
        match scope {
            Scope::Comments { issue_id } => crate::comments::sync(&conn, &transport, issue_id),
            Scope::Teams => crate::teams::sync_teams(&conn, &transport),
            Scope::Team { team_id } => crate::teams::sync_team_data(&conn, &transport, team_id),
        }
    }

    /// The login worker's body, run on its own thread. `Success` requires a
    /// fresh identity: a token exchange that succeeds but whose identity
    /// fetch fails is reported as `Error`.
    fn run_login_body() -> LoginEvent {
        match login_non_interactive() {
            Ok(()) => match Self::viewer_identity() {
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

impl LinearSyncService {
    /// Execute one action: a full/delta cycle updates `run`'s bookkeeping in
    /// place; a scope refresh and a login spawn are self-contained.
    fn perform<'scope>(
        &'scope self,
        action: Action,
        run: &mut RunLoop,
        scope: &'scope thread::Scope<'scope, '_>,
    ) {
        match action {
            Action::Cycle { full } => self.run_cycle(full, run),
            Action::Refresh(s) => self.refresh(&s),
            Action::SpawnLogin => self.spawn_login(scope),
        }
    }

    /// Run one cycle and fold its outcome into `run`'s identity/pause
    /// bookkeeping, then push the deadline out another interval.
    fn run_cycle(&self, full: bool, run: &mut RunLoop) {
        match self.cycle(full, !run.identity_delivered) {
            CycleOutcome::Done { got_identity } => run.identity_delivered |= got_identity,
            CycleOutcome::NotAuthenticated => {
                run.state.mark_not_authenticated();
                run.identity_delivered = false;
            }
            CycleOutcome::Error => {}
        }
        run.deadline = Instant::now() + SYNC_INTERVAL;
    }

    /// Spawn the login worker on the loop's thread-scope: it runs the OAuth
    /// flow, emits `Login(..)` directly, then nudges the loop with
    /// `LoginFinished` so the loop -- the sole owner of the watch set and
    /// the pause gate -- decides the follow-up.
    fn spawn_login<'scope>(&'scope self, scope: &'scope thread::Scope<'scope, '_>) {
        scope.spawn(move || {
            let event = std::panic::catch_unwind(Self::run_login_body)
                .unwrap_or_else(|_| LoginEvent::Error("login worker panicked".to_string()));
            let success = matches!(event, LoginEvent::Success { .. });
            (self.on_event)(RuntimeEvent::Login(event));
            if self
                .commands_tx
                .send(Command::LoginFinished(success))
                .is_err()
            {
                tracing::debug!("login worker: service loop is gone");
            }
        });
    }
}

impl SyncService for LinearSyncService {
    fn run(&self) {
        let Some(commands_rx) = self.take_commands_rx() else {
            tracing::error!("SyncService::run must be called at most once");
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

    fn watch(&self, scope: Scope) {
        if self.commands_tx.send(Command::Watch(scope)).is_err() {
            tracing::debug!("watch: service loop is gone");
        }
    }

    fn unwatch(&self, scope: Scope) {
        if self.commands_tx.send(Command::Unwatch(scope)).is_err() {
            tracing::debug!("unwatch: service loop is gone");
        }
    }

    fn request_sync(&self) {
        if self.commands_tx.send(Command::RequestSync).is_err() {
            tracing::debug!("request_sync: service loop is gone");
        }
    }

    fn login(&self) {
        if self.commands_tx.send(Command::Login).is_err() {
            tracing::debug!("login: service loop is gone");
        }
    }

    fn fetch_viewer(&self) -> Option<viewer::User> {
        Self::viewer_identity()
    }

    fn create_comment(&self, input: &CommentCreateInput) -> Result<()> {
        let conn = self.connect()?;
        db::outbox::enqueue_comment_create(&conn, &db::outbox::temp_id(), input)?;
        (self.on_event)(RuntimeEvent::State(StateEvent::Comments {
            issue_id: input.issue_id.clone(),
        }));
        Ok(())
    }

    fn edit_issue(&self, issue_id: &str, edit: IssueEdit) -> Result<()> {
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
        (self.on_event)(RuntimeEvent::State(StateEvent::Issues));
        Ok(())
    }

    fn create_issue(&self, input: &IssueCreateInput) -> Result<String> {
        let conn = self.connect()?;
        let optimistic = Self::optimistic_issue(&conn, input)?;
        let identifier = optimistic.identifier.clone();
        db::outbox::enqueue_issue_create(&conn, &optimistic, input)?;
        (self.on_event)(RuntimeEvent::State(StateEvent::Issues));
        Ok(identifier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(id: &str) -> Scope {
        Scope::Comments {
            issue_id: id.to_string(),
        }
    }

    #[test]
    fn watch_then_unwatch_drops_the_scope() {
        let mut state = LoopState::new();
        assert_eq!(
            state.on_command(Command::Watch(scope("a"))),
            vec![Action::Refresh(scope("a"))]
        );
        assert!(state.on_timeout().contains(&Action::Refresh(scope("a"))));

        assert_eq!(state.on_command(Command::Unwatch(scope("a"))), Vec::new());
        assert!(
            state
                .on_timeout()
                .iter()
                .all(|a| !matches!(a, Action::Refresh(_)))
        );
    }

    #[test]
    fn watch_is_a_counted_set() {
        let mut state = LoopState::new();
        state.on_command(Command::Watch(scope("a")));
        state.on_command(Command::Watch(scope("a")));

        // One unwatch is not enough to drop a scope watched twice.
        state.on_command(Command::Unwatch(scope("a")));
        assert!(
            state
                .on_timeout()
                .iter()
                .any(|a| matches!(a, Action::Refresh(_)))
        );

        state.on_command(Command::Unwatch(scope("a")));
        assert!(
            state
                .on_timeout()
                .iter()
                .all(|a| !matches!(a, Action::Refresh(_)))
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

    #[test]
    fn timeout_refreshes_watched_scopes_even_while_paused() {
        let mut state = LoopState::new();
        state.on_command(Command::Watch(scope("a")));
        state.mark_not_authenticated();

        assert_eq!(state.on_timeout(), vec![Action::Refresh(scope("a"))]);
    }
}
