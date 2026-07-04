mod detail;
mod keymap;
mod list;
mod markdown;
mod new_issue;
mod popup;
mod search_completer;
mod sync;
mod text_input;
mod ui;

#[cfg(all(test, feature = "sim"))]
mod render_tests;

#[cfg(all(test, feature = "sim"))]
mod loop_tests;

#[cfg(all(test, feature = "sim"))]
use std::collections::VecDeque;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
pub use detail::DetailView;
#[cfg(all(test, feature = "sim"))]
pub(crate) use detail::{build_cached_detail, populate_relations};
pub use list::{ListQuery, ListView};
use lt_runtime::query::IssueQuery;
#[cfg(all(test, feature = "sim"))]
use lt_runtime::sync::service::IssueEdit;
pub use lt_runtime::sync::service::RuntimeEvent;
pub(crate) use lt_runtime::sync::service::StateEvent;
use lt_runtime::sync::service::{LoginEvent, Scope, SyncEvent, SyncService};
use lt_runtime::{Clock, search_query};
use lt_types::types::Issue;
#[cfg(all(test, feature = "sim"))]
pub(crate) use lt_types::types::priority_label_to_u8;
use lt_types::viewer;
#[cfg(all(test, feature = "sim"))]
pub(crate) use new_issue::build_assignee_items;
pub(crate) use new_issue::{NewIssueField, NewIssueModal};
pub(crate) use popup::{
    HelpPopup, PopupItem, PopupKind, PopupView, SearchOverlay, poll_search_debounce,
    priority_popup_items, state_items,
};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::widgets::TableState;
pub(crate) use sync::sync_status_label;
pub(crate) use text_input::TextInput;

// ---------------------------------------------------------------------------
// The app event queue
// ---------------------------------------------------------------------------

/// A message to the event loop. One channel, one drain.
pub enum AppEvent {
    /// A key press (`KeyEventKind::Press` only), raw from crossterm;
    /// normalized at apply time.
    Key(KeyEvent),
    /// Anything the runtime reported.
    Runtime(RuntimeEvent),
}

// ---------------------------------------------------------------------------
// The view stack
// ---------------------------------------------------------------------------

/// One view's complete state. A view exists iff it is displayed; there is no
/// separate mode flag to keep consistent.
pub enum View {
    // Boxed: `ListQuery`'s launch-snapshot fields make `ListView` one of the
    // larger variants, so boxing it keeps every other `View` push/pop from
    // paying for its size.
    List(Box<ListView>),
    // Boxed: `DetailView` is by far the largest variant, so boxing it keeps
    // every other `View` push/pop from paying for its size.
    Detail(Box<DetailView>),
    Popup(PopupView),
    NewIssue(NewIssueModal),
    Search(SearchOverlay),
    Help(HelpPopup),
}

/// A view's declared key handling: its resolution layers, the apply
/// function for non-navigation actions, and the unbound-key policy.
pub(crate) struct Keymap {
    pub(crate) layers: keymap::Layers,
    pub(crate) apply: Option<fn(&mut App, usize, keymap::Action)>,
    pub(crate) unbound: Unbound,
}

/// What a key no layer binds does: cascade, be swallowed, or forward
/// verbatim to the view's editor widget. `esc` is exempt: it always passes
/// to the floor before this policy is consulted.
pub(crate) enum Unbound {
    Cascade,
    Swallow,
    Forward(fn(&mut App, usize, KeyEvent)),
}

impl View {
    /// `focused` is true iff this is the top of the stack; Search/Help have
    /// no `StateEvent` dependencies.
    fn consume(&mut self, ctx: &StateCtx, focused: bool, ev: &StateEvent) {
        match self {
            View::List(list) => list.consume(ctx, focused, ev),
            View::Detail(detail) => detail.consume(ctx, focused, ev),
            View::Popup(popup) => popup.consume(ctx, focused, ev),
            View::NewIssue(modal) => modal.consume(ctx, focused, ev),
            View::Search(_) | View::Help(_) => {}
        }
    }

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

    /// Resolve a scroll motion against this view's own semantics: selection
    /// movement for `List`/`Popup`, offset scrolling for `Detail`.
    fn scroll(&mut self, motion: ScrollMotion, viewport_height: u16) {
        match self {
            View::List(list) => list.scroll(motion, viewport_height),
            View::Detail(detail) => detail.scroll(motion, viewport_height),
            View::Popup(popup) => popup.scroll(motion, viewport_height),
            View::NewIssue(modal) => modal.scroll(motion, viewport_height),
            View::Search(overlay) => overlay.scroll(motion, viewport_height),
            View::Help(help) => help.scroll(motion, viewport_height),
        }
    }

    /// This view's declared keymap, delegating any sub-focus decision to
    /// the view type itself.
    fn keymap(&self) -> &'static Keymap {
        match self {
            View::List(_) => &list::LIST_KEYMAP,
            View::Detail(d) => d.keymap(),
            View::Popup(_) => &popup::POPUP_KEYMAP,
            View::NewIssue(m) => m.keymap(),
            View::Search(_) => &popup::SEARCH_KEYMAP,
            View::Help(_) => &popup::HELP_KEYMAP,
        }
    }
}

/// A scroll/selection motion: one enum over the shared navigation family
/// rather than one method per motion, avoiding eight near-identical trait
/// methods for the same dispatch seam.
#[derive(Clone, Copy)]
pub(crate) enum ScrollMotion {
    Down,
    Up,
    Top,
    Bottom,
    HalfPageDown,
    HalfPageUp,
    PageDown,
    PageUp,
}

impl ScrollMotion {
    /// Selection movement; caller guards `len == 0`.
    fn apply_index(self, selected: usize, len: usize, viewport_height: u16) -> usize {
        let delta: i32 = match self {
            ScrollMotion::Down => 1,
            ScrollMotion::Up => -1,
            ScrollMotion::Top => i32::MIN / 2,
            ScrollMotion::Bottom => i32::MAX / 2,
            ScrollMotion::HalfPageDown => i32::from(viewport_height) / 2,
            ScrollMotion::HalfPageUp => -(i32::from(viewport_height) / 2),
            ScrollMotion::PageDown => i32::from(viewport_height),
            ScrollMotion::PageUp => -i32::from(viewport_height),
        };
        let step = usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX);
        if delta >= 0 {
            selected.saturating_add(step).min(len - 1)
        } else {
            selected.saturating_sub(step)
        }
    }

    /// Selection movement over a plain `usize` field; no-ops on an empty
    /// collection.
    fn apply_selection(self, selected: &mut usize, len: usize, viewport_height: u16) {
        if len == 0 {
            return;
        }
        *selected = self.apply_index(*selected, len, viewport_height);
    }

    /// Selection movement over a `TableState`; no-ops on an empty collection.
    fn apply_table(self, table: &mut TableState, len: usize, viewport_height: u16) {
        if len == 0 {
            return;
        }
        let cur = table.selected().unwrap_or(0);
        table.select(Some(self.apply_index(cur, len, viewport_height)));
    }

    /// Offset scrolling; `Bottom` saturates to `u16::MAX` -- ratatui clamps
    /// scroll to content length.
    fn apply_offset(self, offset: u16, viewport_height: u16) -> u16 {
        match self {
            ScrollMotion::Down => offset.saturating_add(1),
            ScrollMotion::Up => offset.saturating_sub(1),
            ScrollMotion::Top => 0,
            ScrollMotion::Bottom => u16::MAX,
            ScrollMotion::HalfPageDown => offset.saturating_add((viewport_height / 2).max(1)),
            ScrollMotion::HalfPageUp => offset.saturating_sub((viewport_height / 2).max(1)),
            ScrollMotion::PageDown => offset.saturating_add(viewport_height.max(1)),
            ScrollMotion::PageUp => offset.saturating_sub(viewport_height.max(1)),
        }
    }
}

/// Map a navigation `Action` onto its `ScrollMotion`, or `None` if it isn't
/// one.
fn scroll_motion(action: keymap::Action) -> Option<ScrollMotion> {
    use keymap::Action;
    Some(match action {
        Action::MoveDown => ScrollMotion::Down,
        Action::MoveUp => ScrollMotion::Up,
        Action::MoveTop => ScrollMotion::Top,
        Action::MoveBottom => ScrollMotion::Bottom,
        Action::HalfPageDown => ScrollMotion::HalfPageDown,
        Action::HalfPageUp => ScrollMotion::HalfPageUp,
        Action::PageDown => ScrollMotion::PageDown,
        Action::PageUp => ScrollMotion::PageUp,
        _ => return None,
    })
}

/// Read-only context a view's consume/re-query needs. Built inline from
/// disjoint `App` field borrows: an accessor method would borrow all of
/// `self` and conflict with a simultaneous `&mut self.views`.
pub struct StateCtx<'a> {
    pub db: &'a lt_runtime::db::Database,
    pub viewer_name: Option<&'a str>,
}

/// `Pass` cascades to the next layer, then the Esc/q floor. A `Pass`
/// handler must not mutate anything (in particular the stack), so the
/// walk's indices stay valid.
pub enum KeyFlow {
    Consumed,
    Pass,
}

/// One dispatch pass's chord prefix, once-normalized key, and the raw event
/// editor widgets need verbatim -- computed once per keypress rather than
/// threaded as separate parameters.
struct DispatchKey {
    pending: Option<keymap::Key>,
    key: keymap::Key,
    ev: KeyEvent,
}

/// Background-sync typestate: a pure consumer of the events the sync loop
/// reports.
pub enum SyncStatus {
    /// Nothing has happened yet, or the loop reported `NotAuthenticated`.
    Idle,
    /// The loop announced a cycle (`Sync(Started)`).
    Syncing,
    Synced {
        synced_at: chrono::DateTime<chrono::Utc>,
    },
    Failed {
        message: String,
    },
}

/// Authentication typestate: the TUI never holds tokens itself; its witness
/// of authentication is the viewer identity.
pub enum AuthStatus {
    /// The startup identity fetch failed but a token may exist, resolved by
    /// the in-flight startup sync. Not `Unauthenticated`: a token-holding but
    /// offline user must not be blocked by the retry gate.
    Unknown,
    /// The OAuth login flow is in flight; gates `L`.
    Authenticating,
    Authenticated {
        viewer: viewer::User,
    },
    /// The sync layer reported no stored token.
    Unauthenticated,
    /// The last login attempt failed.
    Failed {
        message: String,
    },
}

impl AuthStatus {
    /// The authenticated user's display name, for the header identity and
    /// `assignee:me` resolution. `None` on every non-`Authenticated` state.
    pub fn viewer_name(&self) -> Option<&str> {
        match self {
            AuthStatus::Authenticated { viewer } => Some(&viewer.name),
            _ => None,
        }
    }

    /// The authenticated user's Linear organization name, for the header
    /// identity.
    pub fn org_name(&self) -> Option<&str> {
        match self {
            AuthStatus::Authenticated { viewer } => Some(&viewer.organization.name),
            _ => None,
        }
    }
}

/// Terminal/session capability flags.
pub struct Session {
    /// Whether the terminal supports the kitty keyboard protocol. Without it,
    /// Ctrl-Enter is indistinguishable from Enter, so submit hints show
    /// Alt-Enter instead (which legacy terminals can encode).
    pub keyboard_enhanced: bool,
}

/// A recording, thread-free fake [`SyncService`] for render/loop tests.
/// Write methods delegate to a real `LinearSyncService` sharing the test's
/// in-memory database, so they really enqueue; everything else just records
/// the call for assertions. `run` is a no-op.
#[cfg(all(test, feature = "sim"))]
struct RecordingSyncService {
    inner: lt_runtime::LinearSyncService,
    watched: std::sync::Mutex<Vec<Scope>>,
    unwatched: std::sync::Mutex<Vec<Scope>>,
    request_sync_calls: std::sync::atomic::AtomicUsize,
    login_calls: std::sync::atomic::AtomicUsize,
}

#[cfg(all(test, feature = "sim"))]
impl RecordingSyncService {
    fn new(db: &lt_runtime::db::Database, tx: mpsc::Sender<AppEvent>) -> Result<Self> {
        let on_event: lt_runtime::sync::service::OnEvent = Box::new(move |ev| {
            // Test fixture: the receiving `App` outlives every send in these
            // tests, so a disconnect is not expected; drop rather than assert.
            drop(tx.send(AppEvent::Runtime(ev)));
        });
        Ok(Self {
            inner: lt_runtime::LinearSyncService::new(db.share()?, on_event),
            watched: std::sync::Mutex::new(Vec::new()),
            unwatched: std::sync::Mutex::new(Vec::new()),
            request_sync_calls: std::sync::atomic::AtomicUsize::new(0),
            login_calls: std::sync::atomic::AtomicUsize::new(0),
        })
    }
}

#[cfg(all(test, feature = "sim"))]
impl SyncService for RecordingSyncService {
    fn run(&self) {}

    fn watch(&self, scope: Scope) {
        self.watched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(scope);
    }

    fn unwatch(&self, scope: Scope) {
        self.unwatched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(scope);
    }

    fn request_sync(&self) {
        self.request_sync_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn login(&self) {
        self.login_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn fetch_viewer(&self) -> Option<lt_types::viewer::User> {
        None
    }

    fn create_comment(&self, input: &lt_types::inputs::CommentCreateInput) -> Result<()> {
        self.inner.create_comment(input)
    }

    fn edit_issue(&self, issue_id: &str, edit: IssueEdit) -> Result<()> {
        self.inner.edit_issue(issue_id, edit)
    }

    fn create_issue(&self, input: &lt_types::inputs::IssueCreateInput) -> Result<String> {
        self.inner.create_issue(input)
    }
}

pub struct App {
    /// The view stack, bottom to top; the top is focused. Never empty:
    /// `views[0]` is always the base view (today, the issue list).
    pub views: Vec<View>,

    pub quit: bool,
    // Page size for key handlers; set once per frame.
    pub viewport_height: u16,

    /// A chord's first key, waiting for its second. No timer: it survives
    /// idle frames until the next key resolves or drops it.
    pending_key: Option<keymap::Key>,

    // -- footer message ----------------------------------------------
    pub footer_msg: Option<String>,

    // -- background-job typestates --------------------------------------
    pub sync: SyncStatus,
    pub auth: AuthStatus,

    pub session: Session,

    // -- double-esc reset -------------------------------------------------
    /// Timestamp of the last Esc keypress (used to detect double-esc).
    pub last_esc_time: Option<Instant>,

    /// Database handle. Defaults to the per-profile SQLite file; tests install
    /// an in-memory database via `Database::memory`.
    pub db: lt_runtime::db::Database,

    /// Wall-clock source. Defaults to the system clock; tests install a fixed
    /// clock so time-derived labels are deterministic.
    pub clock: Clock,

    /// The sync/API edge. A trait object so the TUI has no direct
    /// dependency on the sync implementation.
    pub service: Arc<dyn SyncService>,

    /// The single consumer of the app event queue, drained once per frame.
    events_rx: mpsc::Receiver<AppEvent>,
}

impl App {
    // `sync`/`auth` start unstarted (`Idle`/`Unknown`); they transition once
    // the loop's own events arrive.
    fn new(
        list: ListView,
        db: lt_runtime::db::Database,
        service: Arc<dyn SyncService>,
        events_rx: mpsc::Receiver<AppEvent>,
    ) -> Self {
        Self {
            views: vec![View::List(Box::new(list))],
            quit: false,
            viewport_height: 0,
            pending_key: None,
            footer_msg: None,
            sync: SyncStatus::Idle,
            auth: AuthStatus::Unknown,
            session: Session {
                keyboard_enhanced: false,
            },
            last_esc_time: None,
            db,
            clock: Clock::System,
            service,
            events_rx,
        }
    }

    /// An `App` for rendering tests: a throwaway in-memory database and
    /// event channel, backed by a [`RecordingSyncService`]. Seeds `issues`
    /// directly rather than through `ListView::open`, since the memory db is
    /// still empty at this point. Fallible (in-memory SQLite setup).
    #[cfg(all(test, feature = "sim"))]
    fn for_test(issues: Vec<Issue>) -> Result<Self> {
        let db = lt_runtime::db::Database::memory()?;
        let (tx, rx) = mpsc::channel();
        let service = RecordingSyncService::new(&db, tx)?;
        let query = ListQuery::from(IssueQuery::default());
        let list = ListView::new(issues, query);
        Ok(Self::new(list, db, Arc::new(service), rx))
    }

    /// Swap in a fresh database, shared with a fresh [`RecordingSyncService`]
    /// and event channel, so `db`/`service` agree on the same rows.
    #[cfg(all(test, feature = "sim"))]
    fn install_db(&mut self, db: lt_runtime::db::Database) -> Result<()> {
        self.install_recording_service(&db)?;
        self.db = db;
        Ok(())
    }

    /// Swap in a fresh `RecordingSyncService` (and its paired event channel)
    /// sharing `db`, returning it so callers can assert on its recorded
    /// calls.
    #[cfg(all(test, feature = "sim"))]
    fn install_recording_service(
        &mut self,
        db: &lt_runtime::db::Database,
    ) -> Result<Arc<RecordingSyncService>> {
        let (tx, rx) = mpsc::channel();
        let service = Arc::new(RecordingSyncService::new(db, tx)?);
        self.service = service.clone();
        self.events_rx = rx;
        Ok(service)
    }

    /// The base view (`views[0]`), always present.
    fn base(&self) -> &View {
        &self.views[0]
    }

    fn base_mut(&mut self) -> &mut View {
        &mut self.views[0]
    }

    /// Test-only infallible accessor: render/loop tests always seed a list
    /// base, so a panic here signals a broken fixture, not a runtime state to
    /// handle.
    #[cfg(all(test, feature = "sim"))]
    fn list_mut(&mut self) -> &mut ListView {
        match self.base_mut() {
            View::List(list) => list,
            _ => unreachable!("test base view is not a list"),
        }
    }

    fn selected_issue(&self) -> Option<&Issue> {
        match self.base() {
            View::List(list) => list.selected_issue(),
            _ => None,
        }
    }

    /// Push a view, watching the scopes it declares.
    fn push_view(&mut self, view: View) {
        for scope in view.scopes() {
            self.service.watch(scope);
        }
        self.views.push(view);
    }

    /// Pop the focused view, unwatching its scopes. The stack is never
    /// empty: popping the base resets it to the default instead.
    fn pop_view(&mut self) {
        if self.views.len() > 1 {
            self.close_view_at(self.views.len() - 1);
        } else {
            self.reset_base_view();
        }
    }

    /// Remove the view at `i`, unwatching the scopes it declared, without
    /// disturbing whatever else is on the stack.
    fn close_view_at(&mut self, i: usize) {
        if i < self.views.len() {
            let view = self.views.remove(i);
            for scope in view.scopes() {
                self.service.unwatch(scope);
            }
        }
    }

    /// Full reset to the state the TUI was launched with: sort, filters,
    /// and search query.
    fn reset_base_view(&mut self) {
        let ctx = StateCtx {
            db: &self.db,
            viewer_name: self.auth.viewer_name(),
        };
        if let Some(View::List(list)) = self.views.first_mut() {
            list.query.reset();
            list.refetch(&ctx, true);
        }
        self.last_esc_time = None;
    }

    /// `r`: an immediate re-read plus a sync request. Pressed mid-cycle,
    /// this coalesces into a follow-up sync rather than being ignored.
    fn refresh(&mut self) {
        let ctx = StateCtx {
            db: &self.db,
            viewer_name: self.auth.viewer_name(),
        };
        // immediate re-read for responsiveness
        if let Some(View::List(list)) = self.views.first_mut() {
            list.refetch(&ctx, false);
        }
        self.service.request_sync();
    }

    /// Downcast the view at `i` via `extract`.
    fn view_at_mut<T>(
        &mut self,
        i: usize,
        extract: fn(&mut View) -> Option<&mut T>,
    ) -> Option<&mut T> {
        self.views.get_mut(i).and_then(extract)
    }

    fn handle_list_esc(&mut self) {
        // Double-esc (within 500ms) resets sort, filters, and search query
        // back to the state the TUI was launched with.
        let now = Instant::now();
        let is_double_esc = self
            .last_esc_time
            .is_some_and(|t| t.elapsed() < Duration::from_millis(500));
        if is_double_esc {
            self.reset_base_view();
        } else {
            // First esc: standard refresh.
            self.last_esc_time = Some(now);
            let ctx = StateCtx {
                db: &self.db,
                viewer_name: self.auth.viewer_name(),
            };
            if let Some(View::List(list)) = self.views.first_mut() {
                list.refetch(&ctx, true);
            }
        }
    }

    fn open_search_overlay(&mut self) {
        let mut overlay = SearchOverlay::new();
        // Capture the query limit once: it can't change while Search has
        // focus, so the snapshot stays faithful for the overlay's lifetime.
        if let View::List(list) = self.base() {
            overlay.limit = list.query.args.limit;
        }
        // Restore the base list's filter when reopening, unless it's just
        // the default sort stem.
        if let View::List(list) = self.base()
            && list.query.filter.raw != search_query::DEFAULT_QUERY
        {
            let filter = list.query.filter.clone();
            overlay.query = TextInput::from(filter.raw.clone());
            overlay.ast = filter;
            overlay.last_changed = Some(Instant::now());
        }
        self.push_view(View::Search(overlay));
    }

    /// Apply a queued app event: a key cascades through `dispatch_key`; a
    /// state invalidation walks the view stack; a sync/login outcome
    /// transitions the typestates.
    fn apply(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.dispatch_key(key),
            AppEvent::Runtime(RuntimeEvent::State(ev)) => self.route_state_event(&ev),
            AppEvent::Runtime(RuntimeEvent::Sync(ev)) => self.consume_sync_event(ev),
            AppEvent::Runtime(RuntimeEvent::Login(ev)) => self.consume_login_event(ev),
        }
    }

    /// Normalize once, then walk the view stack top-down; `pending`, the
    /// chord prefix, is taken once here for the whole walk before falling to
    /// the Esc/q floor.
    fn dispatch_key(&mut self, ev: KeyEvent) {
        let key = keymap::Key::from(ev);
        // A chord in progress: Esc cancels it and does nothing else --
        // checked before anything else so it never reaches the floor's Back
        // or touches `last_esc_time`.
        if self.pending_key.is_some() && key == keymap::Key::plain(KeyCode::Esc) {
            self.pending_key = None;
            return;
        }
        let dk = DispatchKey {
            pending: self.pending_key.take(),
            key,
            ev,
        };
        let top = self.views.len() - 1;
        for i in (0..=top).rev() {
            if matches!(self.handle_view_key(i, &dk), KeyFlow::Consumed) {
                return;
            }
        }
        match key.code {
            // Back, above the base: `q` never reaches Quit from an overlay.
            KeyCode::Esc | KeyCode::Char('q') if top > 0 => self.pop_view(),
            KeyCode::Esc => self.handle_list_esc(), // double-esc reset
            KeyCode::Char('q') => self.quit = true,
            _ => {}
        }
    }

    /// Resolve `dk`'s key against the view at `i`'s keymap and act on the
    /// result; `esc` is never forwarded and always passes to the floor.
    fn handle_view_key(&mut self, i: usize, dk: &DispatchKey) -> KeyFlow {
        let km = self.views[i].keymap();
        match keymap::resolve(km.layers, dk.pending, dk.key) {
            keymap::Resolved::Act(action) => {
                if let Some(motion) = scroll_motion(action) {
                    let viewport = self.viewport_height;
                    if let Some(view) = self.views.get_mut(i) {
                        view.scroll(motion, viewport);
                    }
                } else if let Some(apply) = km.apply {
                    apply(self, i, action);
                }
                KeyFlow::Consumed
            }
            keymap::Resolved::Pending(k) => {
                self.pending_key = Some(k);
                KeyFlow::Consumed
            }
            // `resolve`'s `Unbound` payload is always `dk.key` (a chord miss
            // resolves the same key fresh); dropping it here avoids threading
            // a redundant argument through the match below.
            keymap::Resolved::Unbound(_) => {
                if dk.key == keymap::Key::plain(KeyCode::Esc) {
                    return KeyFlow::Pass; // esc is the floor's, never a forward
                }
                match km.unbound {
                    Unbound::Cascade => KeyFlow::Pass,
                    Unbound::Swallow => KeyFlow::Consumed,
                    Unbound::Forward(forward) => {
                        forward(self, i, dk.ev);
                        KeyFlow::Consumed
                    }
                }
            }
        }
    }

    /// Route a state invalidation down the stack, top first (order is
    /// semantically irrelevant; chosen for coherence with the key cascade).
    fn route_state_event(&mut self, ev: &StateEvent) {
        let ctx = StateCtx {
            db: &self.db,
            viewer_name: self.auth.viewer_name(),
        };
        let len = self.views.len();
        for (i, view) in self.views.iter_mut().enumerate().rev() {
            view.consume(&ctx, i + 1 == len, ev);
        }
    }

    /// `synced_at` for a `Done` transition: the DB's `last_synced_at` meta,
    /// falling back to the clock when the read fails or is unparseable.
    fn synced_at_now(&self) -> chrono::DateTime<chrono::Utc> {
        let raw = match self.db.connect() {
            Ok(conn) => match lt_runtime::db::get_meta(&conn, "last_synced_at") {
                Ok(ts) => ts,
                Err(e) => {
                    tracing::warn!(error = %e, "synced_at: failed to read last_synced_at meta");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "synced_at: failed to open db connection");
                None
            }
        };
        raw.and_then(|ts| chrono::DateTime::parse_from_rfc3339(&ts).ok())
            .map_or_else(|| self.clock.now(), |dt| dt.with_timezone(&chrono::Utc))
    }

    /// Transition the `sync` typestate, and `auth` if a cycle delivered an
    /// identity.
    fn consume_sync_event(&mut self, ev: SyncEvent) {
        match ev {
            SyncEvent::Started => {
                self.sync = SyncStatus::Syncing;
            }
            SyncEvent::Done(viewer) => {
                // A freshly-fetched identity implies authentication; absence
                // means it wasn't requested, so `auth` is left unchanged.
                if let Some(viewer) = viewer {
                    self.auth = AuthStatus::Authenticated { viewer };
                }
                self.sync = SyncStatus::Synced {
                    synced_at: self.synced_at_now(),
                };
            }
            SyncEvent::Error(message) => {
                self.sync = SyncStatus::Failed { message };
            }
            SyncEvent::NotAuthenticated => {
                self.auth = AuthStatus::Unauthenticated;
                self.sync = SyncStatus::Idle;
            }
        }
    }

    /// Transition the `auth` typestate from a login outcome.
    fn consume_login_event(&mut self, ev: LoginEvent) {
        match ev {
            LoginEvent::Success { viewer } => {
                self.auth = AuthStatus::Authenticated { viewer };
            }
            LoginEvent::Error(message) => {
                self.auth = AuthStatus::Failed {
                    message: message.clone(),
                };
                // A transient direct write: deriving it from `Failed` would
                // pin the message past whatever clears it.
                self.footer_msg = Some(format!("Login failed: {message}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

pub fn run(
    args: IssueQuery,
    service: Arc<dyn SyncService>,
    events_tx: mpsc::Sender<AppEvent>,
    events_rx: mpsc::Receiver<AppEvent>,
) -> Result<()> {
    let db = lt_runtime::db::Database::File;

    // Fetch viewer identity before `service` moves into `App::new` (a
    // shared read through the `Arc`, so ownership either order is fine).
    let viewer = service.fetch_viewer();

    // The query defines the view's initial data (local-first UX): `open`
    // warns and starts empty on a failed/missing db read, same as every
    // later refetch.
    let ctx = StateCtx {
        db: &db,
        viewer_name: viewer.as_ref().map(|v| v.name.as_str()),
    };
    let list = ListView::open(ListQuery::from(args), &ctx);
    let mut app = App::new(list, db, service, events_rx);

    app.auth = match viewer {
        Some(viewer) => AuthStatus::Authenticated { viewer },
        None => AuthStatus::Unknown,
    };
    // `sync` stays `Idle` until the loop's own `Sync(Started)` arrives.

    let mut terminal = ratatui::init();
    // Without the kitty keyboard protocol, terminals encode Ctrl-Enter and
    // Enter as the same byte, so the Ctrl-Enter submit binding never fires.
    // Enable it where supported; elsewhere the UI falls back to Alt-Enter.
    let keyboard_enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if keyboard_enhanced
        && let Err(e) = crossterm::execute!(
            std::io::stdout(),
            event::PushKeyboardEnhancementFlags(
                event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            )
        )
    {
        tracing::warn!(error = %e, "failed to push keyboard enhancement flags");
    }
    app.session.keyboard_enhanced = keyboard_enhanced;
    spawn_input_thread(events_tx);
    let mut pump = EventPump::Channel;
    let result = run_app(&mut terminal, &mut pump, &mut app);
    if keyboard_enhanced
        && let Err(e) = crossterm::execute!(std::io::stdout(), event::PopKeyboardEnhancementFlags)
    {
        tracing::warn!(error = %e, "failed to pop keyboard enhancement flags");
    }
    ratatui::restore();
    result
}

/// Detached input thread: forwards every key press onto the app event
/// queue; exits on a `send` failure or a read error.
fn spawn_input_thread(tx: mpsc::Sender<AppEvent>) {
    std::thread::spawn(move || {
        loop {
            match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    if tx.send(AppEvent::Key(key)).is_err() {
                        return;
                    }
                }
                Ok(_) => {}       // resize/mouse/release: dropped
                Err(_) => return, // terminal gone
            }
        }
    });
}

/// Where the loop's blocking wait gets its first event each frame. A closed
/// set: the channel in the binary, a script in tests.
enum EventPump {
    Channel,
    /// Scripted events for loop tests; errors when exhausted so a test that
    /// forgot to quit fails fast instead of hanging.
    #[cfg(all(test, feature = "sim"))]
    Scripted(VecDeque<AppEvent>),
}

impl EventPump {
    /// Block up to `timeout` for this frame's first event: `recv_timeout` on
    /// the real channel in production, the next scripted event in tests.
    /// `Disconnected` is unreachable in production -- `App` owns a sender for
    /// the lifetime of the loop -- so the `Channel` arm treats it as an idle
    /// tick, same as a timeout.
    // `Scripted`'s exhaustion error only exists under `#[cfg(test, feature =
    // "sim")]`; without it this function's only path is infallible, which
    // clippy flags on that compile.
    #[cfg_attr(not(all(test, feature = "sim")), allow(clippy::unnecessary_wraps))]
    fn next(
        &mut self,
        rx: &mpsc::Receiver<AppEvent>,
        timeout: Duration,
    ) -> Result<Option<AppEvent>> {
        match self {
            EventPump::Channel => match rx.recv_timeout(timeout) {
                Ok(event) => Ok(Some(event)),
                Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {
                    Ok(None)
                }
            },
            #[cfg(all(test, feature = "sim"))]
            EventPump::Scripted(events) => events
                .pop_front()
                .map(Some)
                .ok_or_else(|| anyhow::anyhow!("scripted events exhausted")),
        }
    }
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    pump: &mut EventPump,
    app: &mut App,
) -> Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    loop {
        // The frame loop's only inline timer is `poll_search_debounce`.
        poll_search_debounce(app);

        terminal.draw(|frame| ui::render(frame, app))?;
        if app.quit {
            return Ok(());
        }

        // Block up to 100ms for the first event, then drain without
        // blocking: events same-thread writers pushed onto the real channel
        // while we were blocked are seen in the same frame.
        if let Some(event) = pump.next(&app.events_rx, Duration::from_millis(100))? {
            app.apply(event);
        }
        while let Ok(event) = app.events_rx.try_recv() {
            app.apply(event);
        }
    }
}

/// Open `identifier`'s issue in the browser.
pub(crate) fn open_in_browser(identifier: &str) {
    let url = format!("https://linear.app/issue/{identifier}");
    if let Err(e) = open::that(url) {
        tracing::warn!(error = %e, "failed to open browser for issue url");
    }
}

// -- Help overlay registry ---------------------------------------------------

/// The help overlay's contexts, in display order. The two new-issue
/// contexts collapse into one: form-nav plus the picker's own rows, with no
/// duplicates.
pub(crate) static HELP_CONTEXTS: &[(&str, &[keymap::Table])] = &[
    ("global", &[keymap::GLOBAL]),
    ("list", &[list::LIST_BINDINGS]),
    ("detail", &[detail::DETAIL_BINDINGS]),
    ("popup", &[popup::POPUP_BINDINGS]),
    (
        "new issue",
        &[new_issue::FORM_NAV, new_issue::PICKER_BINDINGS],
    ),
    ("comment", &[detail::COMMENT_INPUT_BINDINGS]),
    ("search", &[popup::SEARCH_BINDINGS]),
    ("help", &[popup::HELP_BINDINGS]),
];

/// Every declared keymap, named for test diagnostics.
#[cfg(test)]
pub(crate) static ALL_KEYMAPS: &[(&str, &Keymap)] = &[
    ("list", &list::LIST_KEYMAP),
    ("detail", &detail::DETAIL_KEYMAP),
    ("comment_input", &detail::COMMENT_INPUT_KEYMAP),
    ("popup", &popup::POPUP_KEYMAP),
    ("new_issue_picker", &new_issue::PICKER_KEYMAP),
    ("new_issue_text", &new_issue::TEXT_KEYMAP),
    ("search", &popup::SEARCH_KEYMAP),
    ("help", &popup::HELP_KEYMAP),
];

#[cfg(test)]
mod tests {
    use super::ScrollMotion;

    #[test]
    fn apply_index_steps_down_and_up() {
        assert_eq!(ScrollMotion::Down.apply_index(2, 5, 10), 3);
        assert_eq!(ScrollMotion::Up.apply_index(2, 5, 10), 1);
    }

    #[test]
    fn apply_index_top_and_bottom_saturate() {
        assert_eq!(ScrollMotion::Top.apply_index(3, 5, 10), 0);
        assert_eq!(ScrollMotion::Bottom.apply_index(0, 5, 10), 4);
    }

    #[test]
    fn apply_index_saturates_at_both_ends() {
        assert_eq!(ScrollMotion::Up.apply_index(0, 5, 10), 0);
        assert_eq!(ScrollMotion::Down.apply_index(4, 5, 10), 4);
    }

    #[test]
    fn apply_index_half_page_and_page_steps() {
        assert_eq!(ScrollMotion::HalfPageDown.apply_index(0, 100, 10), 5);
        assert_eq!(ScrollMotion::HalfPageUp.apply_index(20, 100, 10), 15);
        assert_eq!(ScrollMotion::PageDown.apply_index(0, 100, 10), 10);
        assert_eq!(ScrollMotion::PageUp.apply_index(20, 100, 10), 10);
    }

    #[test]
    fn apply_offset_steps_down_and_up() {
        assert_eq!(ScrollMotion::Down.apply_offset(2, 10), 3);
        assert_eq!(ScrollMotion::Up.apply_offset(2, 10), 1);
    }

    #[test]
    fn apply_offset_top_and_bottom() {
        assert_eq!(ScrollMotion::Top.apply_offset(42, 10), 0);
        assert_eq!(ScrollMotion::Bottom.apply_offset(0, 10), u16::MAX);
    }

    #[test]
    fn apply_offset_saturates_at_both_ends() {
        assert_eq!(ScrollMotion::Up.apply_offset(0, 10), 0);
        assert_eq!(ScrollMotion::Down.apply_offset(u16::MAX, 10), u16::MAX);
    }

    #[test]
    fn apply_offset_half_page_and_page_steps() {
        assert_eq!(ScrollMotion::HalfPageDown.apply_offset(0, 10), 5);
        assert_eq!(ScrollMotion::HalfPageUp.apply_offset(20, 10), 15);
        assert_eq!(ScrollMotion::PageDown.apply_offset(0, 10), 10);
        assert_eq!(ScrollMotion::PageUp.apply_offset(20, 10), 10);
    }

    #[test]
    fn apply_offset_half_page_and_page_floor_at_one_step() {
        // A viewport under 2 rows still steps by at least one line.
        assert_eq!(ScrollMotion::HalfPageDown.apply_offset(0, 1), 1);
        assert_eq!(ScrollMotion::PageDown.apply_offset(5, 0), 6);
    }
}
