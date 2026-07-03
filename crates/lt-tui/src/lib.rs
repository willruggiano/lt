mod detail;
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

use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
pub use detail::DetailView;
#[cfg(all(test, feature = "sim"))]
pub(crate) use detail::{build_cached_detail, populate_relations};
use lt_runtime::query::IssueQuery;
use lt_runtime::search_query;
use lt_runtime::sync::service::{LoginEvent, SyncEvent, SyncService};
use lt_types::types::Issue;
#[cfg(all(test, feature = "sim"))]
pub(crate) use lt_types::types::priority_label_to_u8;
#[cfg(all(test, feature = "sim"))]
use lt_types::types::{Team, User, WorkflowState};
#[cfg(all(test, feature = "sim"))]
pub(crate) use new_issue::{ModalEvent, build_assignee_items};
pub(crate) use new_issue::{NewIssueField, NewIssueModal};
#[cfg(all(test, feature = "sim"))]
pub(crate) use popup::handle_key as handle_popup_key;
pub(crate) use popup::{
    HelpPopup, PopupItem, PopupKind, PopupView, SearchOverlay, poll_search_debounce,
    priority_popup_items,
};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::widgets::TableState;
pub(crate) use sync::{build_sync_status_label, poll_login_events, poll_sync_events};
pub(crate) use text_input::TextInput;

/// Wall-clock source. The set of clocks is closed -- the real system clock in
/// the binary, plus a fixed instant in tests -- so it is an enum rather than a
/// trait or a boxed closure (cf. `db::Database`, which is an enum for the same
/// reason). The `Fixed` variant is compiled out of the production binary.
pub enum Clock {
    /// The real wall clock.
    System,
    /// A clock frozen at a fixed instant, for deterministic tests.
    #[cfg(all(test, feature = "sim"))]
    Fixed(chrono::DateTime<chrono::Utc>),
}

impl Clock {
    /// Read the current instant.
    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        match self {
            Clock::System => chrono::Utc::now(),
            #[cfg(all(test, feature = "sim"))]
            Clock::Fixed(instant) => *instant,
        }
    }
}

/// Source of key events for the event loop. Abstracts crossterm so tests can
/// feed a scripted sequence instead of reading the real terminal.
trait EventSource {
    /// Return the next key press, or `None` if none arrived within `timeout`.
    fn next_key(&mut self, timeout: std::time::Duration) -> Result<Option<KeyEvent>>;
}

/// Production event source: poll-and-read from the real terminal.
struct CrosstermEvents;

impl EventSource for CrosstermEvents {
    fn next_key(&mut self, timeout: std::time::Duration) -> Result<Option<KeyEvent>> {
        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            return Ok(Some(key));
        }
        Ok(None)
    }
}

pub enum Status {
    Idle,
    Loading,
    Error(String),
}

// ---------------------------------------------------------------------------
// The app event queue
// ---------------------------------------------------------------------------

/// A message to the event loop. Only a state invalidation lands in this
/// stage; key presses stay on `EventSource` and background-job lifecycle
/// outcomes (`Lifecycle`) arrive in a later stage.
pub enum AppEvent {
    /// The named slice of application state changed; re-read it if displayed.
    State(StateEvent),
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

// ---------------------------------------------------------------------------
// The view stack
// ---------------------------------------------------------------------------

/// One view's complete state. A view exists iff it is displayed; there is no
/// separate mode flag to keep consistent.
pub enum View {
    List(ListView),
    // Boxed: `DetailView` is by far the largest variant (owns comments and
    // parent/children issues), so boxing it keeps every other `View`
    // push/pop from paying for its size.
    Detail(Box<DetailView>),
    Popup(PopupView),
    NewIssue(NewIssueModal),
    Search(SearchOverlay),
    Help(HelpPopup),
}

impl View {
    /// Route a state invalidation to this view's consumer, if it has one.
    /// `focused` is true iff this is the top of the stack. Popup/NewIssue/
    /// Search/Help have no consumer yet -- their `StateEvent` dependencies
    /// (`Team`/`Teams`) arrive in the cache-first pickers stage.
    fn consume(&mut self, ctx: &StateCtx, focused: bool, ev: &StateEvent) {
        match self {
            View::List(list) => list.consume(ctx, focused, ev),
            View::Detail(detail) => detail.consume(ctx, focused, ev),
            View::Popup(_) | View::NewIssue(_) | View::Search(_) | View::Help(_) => {}
        }
    }
}

/// The issue-list view: the base-list fields, owned. `status`'s only render
/// site is the base table's Loading/Error overlay (`ui/table.rs`); its
/// writers are `do_fetch` and the sync lifecycle's Loading->Idle repair
/// (reached from other views through `App::base_list_mut`).
pub struct ListView {
    pub issues: Vec<Issue>,
    pub table_state: TableState,
    pub pagination: Pagination,
    pub status: Status,
}

impl ListView {
    fn new(issues: Vec<Issue>, pagination: Pagination) -> Self {
        let mut table_state = TableState::default();
        if !issues.is_empty() {
            table_state.select(Some(0));
        }
        Self {
            issues,
            table_state,
            pagination,
            status: Status::Idle,
        }
    }

    fn selected_issue(&self) -> Option<&Issue> {
        self.table_state.selected().and_then(|i| self.issues.get(i))
    }

    fn move_by(&mut self, delta: i32) {
        let n = self.issues.len();
        if n == 0 {
            return;
        }
        let cur = self.table_state.selected().unwrap_or(0);
        let step = usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX);
        let new_i = if delta >= 0 {
            cur.saturating_add(step).min(n - 1)
        } else {
            cur.saturating_sub(step)
        };
        self.table_state.select(Some(new_i));
    }

    fn move_down(&mut self) {
        self.move_by(1);
    }
    fn move_up(&mut self) {
        self.move_by(-1);
    }
    fn move_top(&mut self) {
        self.move_by(i32::MIN / 2);
    }
    fn move_bottom(&mut self) {
        self.move_by(i32::MAX / 2);
    }
    fn page_down(&mut self, viewport_height: u16) {
        self.move_by(i32::from(viewport_height));
    }
    fn page_up(&mut self, viewport_height: u16) {
        self.move_by(-i32::from(viewport_height));
    }
    fn half_page_down(&mut self, viewport_height: u16) {
        self.move_by(i32::from(viewport_height) / 2);
    }
    fn half_page_up(&mut self, viewport_height: u16) {
        self.move_by(-(i32::from(viewport_height) / 2));
    }

    /// The base list's subscription: `Issues`, only while focused -- the
    /// don't-clobber policy expressed as `focused` instead of a mode check: a
    /// refresh must not swap the rows a popup is anchored to or a search
    /// overlay was opened over.
    fn consume(&mut self, ctx: &StateCtx, focused: bool, ev: &StateEvent) {
        if matches!(ev, StateEvent::Issues) && focused {
            self.do_fetch(ctx, false); // offset- and selection-preserving
        }
    }

    /// The base list's re-read: `db` + `args.limit` + the active filter + the
    /// viewer name for `assignee:me` resolution.
    fn do_fetch(&mut self, ctx: &StateCtx, reset_selection: bool) {
        self.status = Status::Loading;
        let mut parsed = search_query::ParsedQuery::from(ctx.filter);
        search_query::resolve_me(&mut parsed, ctx.viewer_name);

        if parsed.has_filters() {
            // Active filter has constraints beyond sort -- use run_query to
            // preserve them.
            let limit = ctx.args.limit.min(250) as usize;
            match ctx
                .db
                .connect()
                .and_then(|conn| search_query::run_query(&conn, &parsed, limit))
            {
                Ok(issues) => {
                    self.issues = issues;
                    self.pagination.has_next_page = false; // run_query has no pagination
                    self.pagination.end_cursor = None;
                    self.apply_fetched_selection(reset_selection);
                }
                Err(e) => {
                    self.status = Status::Error(e.to_string());
                }
            }
        } else {
            // No active filters -- use paginated query as before.
            let offset: i64 = self
                .pagination
                .current_cursor
                .as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            match ctx
                .db
                .connect()
                .and_then(|conn| lt_runtime::db::query_issues_page(&conn, ctx.args, offset))
            {
                Ok((issues, has_next_page)) => {
                    self.issues = issues;
                    self.pagination.has_next_page = has_next_page;
                    let limit = i64::from(ctx.args.limit.min(250));
                    self.pagination.end_cursor = if has_next_page {
                        Some((offset + limit).to_string())
                    } else {
                        None
                    };
                    self.apply_fetched_selection(reset_selection);
                }
                Err(e) => {
                    self.status = Status::Error(e.to_string());
                }
            }
        }
    }

    /// After replacing `self.issues`, clamp/reset the selection and mark idle.
    fn apply_fetched_selection(&mut self, reset_selection: bool) {
        let n = self.issues.len();
        let sel = if reset_selection {
            0
        } else {
            self.table_state
                .selected()
                .unwrap_or(0)
                .min(n.saturating_sub(1))
        };
        self.table_state
            .select(if n > 0 { Some(sel) } else { None });
        self.status = Status::Idle;
    }

    /// Fetch and then seek to the newly created issue by identifier.
    fn do_fetch_and_select(&mut self, ctx: &StateCtx, target_identifier: Option<String>) {
        self.do_fetch(ctx, true);
        if let Some(id) = target_identifier
            && let Some(idx) = self.issues.iter().position(|i| i.identifier == id)
        {
            self.table_state.select(Some(idx));
        }
    }

    fn next_page(&mut self, ctx: &StateCtx) {
        if !self.pagination.has_next_page {
            return;
        }
        let end = self.pagination.end_cursor.clone();
        self.pagination
            .cursor_stack
            .push(self.pagination.current_cursor.clone());
        self.pagination.current_cursor = end;
        self.do_fetch(ctx, true);
    }

    fn prev_page(&mut self, ctx: &StateCtx) {
        let Some(cursor) = self.pagination.cursor_stack.pop() else {
            return;
        };
        self.pagination.current_cursor = cursor;
        self.do_fetch(ctx, true);
    }
}

/// Read-only context the base list's re-read needs. Built inline from
/// disjoint `App` field borrows at each call site: an `App::state_ctx(&self)`
/// accessor would borrow all of `self` and conflict with any simultaneous
/// `&mut self.views` access.
pub struct StateCtx<'a> {
    pub db: &'a lt_runtime::db::Database,
    pub args: &'a IssueQuery,
    pub filter: &'a search_query::QueryAst,
    pub viewer_name: Option<&'a str>,
}

/// What a key handler did with a key. `Pass` hands it to the next view down;
/// a handler that returns `Pass` must not have mutated anything (in
/// particular the stack), so the walk's indices stay valid. Mechanism-only in
/// this stage: every handler returns `Consumed` unconditionally, so no key
/// cascades yet and behavior is unchanged from today's mode dispatch.
pub enum KeyFlow {
    Consumed,
    Pass,
}

type KeyHandler = fn(&mut App, usize, KeyEvent) -> KeyFlow;

// ---------------------------------------------------------------------------
// Help popup state
// ---------------------------------------------------------------------------

/// A single keybinding entry shown in the help popup.
pub struct HelpEntry {
    pub key: &'static str,
    pub description: &'static str,
}

/// All keybindings shown in the help popup.
pub const ALL_KEYBINDINGS: &[HelpEntry] = &[
    HelpEntry {
        key: "q",
        description: "quit",
    },
    HelpEntry {
        key: "<esc>",
        description: "clear search filter / reset list",
    },
    HelpEntry {
        key: "j / <down>",
        description: "move down",
    },
    HelpEntry {
        key: "k / <up>",
        description: "move up",
    },
    HelpEntry {
        key: "g",
        description: "go to top",
    },
    HelpEntry {
        key: "G",
        description: "go to bottom",
    },
    HelpEntry {
        key: "ctrl+d",
        description: "half page down",
    },
    HelpEntry {
        key: "ctrl+u",
        description: "half page up",
    },
    HelpEntry {
        key: "<page down>",
        description: "page down",
    },
    HelpEntry {
        key: "<page up>",
        description: "page up",
    },
    HelpEntry {
        key: "<space>",
        description: "open detail pane",
    },
    HelpEntry {
        key: "/",
        description: "filter by title",
    },
    HelpEntry {
        key: "?",
        description: "open this help popup",
    },
    HelpEntry {
        key: "n",
        description: "new issue",
    },
    HelpEntry {
        key: "s",
        description: "set state",
    },
    HelpEntry {
        key: "p",
        description: "set priority",
    },
    HelpEntry {
        key: "a",
        description: "set assignee",
    },
    HelpEntry {
        key: "o",
        description: "open in browser",
    },
    HelpEntry {
        key: "c",
        description: "comment on issue (in detail pane)",
    },
    HelpEntry {
        key: "r",
        description: "refresh",
    },
    HelpEntry {
        key: "S",
        description: "cycle sort field",
    },
    HelpEntry {
        key: "d",
        description: "toggle sort direction",
    },
    HelpEntry {
        key: "ctrl+n",
        description: "next page",
    },
    HelpEntry {
        key: "ctrl+p",
        description: "previous page",
    },
    HelpEntry {
        key: "L",
        description: "log in / re-authenticate",
    },
];

/// Forward/backward pagination state.
pub struct Pagination {
    pub has_next_page: bool,
    pub current_cursor: Option<String>,
    pub cursor_stack: Vec<Option<String>>,
    pub end_cursor: Option<String>,
}

/// Background sync state.
pub struct SyncState {
    /// Receiver for background sync events.
    pub sync_rx: Option<mpsc::Receiver<SyncEvent>>,
    /// True while a background sync thread is running.
    pub syncing: bool,
    /// Human-readable description of sync status, shown in footer.
    pub sync_status_label: String,
    /// When to fire the next periodic delta sync (30s cadence).
    pub next_sync_at: Option<Instant>,
}

/// Terminal/session capability flags.
pub struct Session {
    /// Whether the terminal supports the kitty keyboard protocol. Without it,
    /// Ctrl-Enter is indistinguishable from Enter, so submit hints show
    /// Alt-Enter instead (which legacy terminals can encode).
    pub keyboard_enhanced: bool,
    /// True when the last sync reported `NotAuthenticated` (no token stored).
    pub not_authenticated: bool,
}

/// A do-nothing [`SyncService`] for render/loop tests: performs no I/O and
/// returns empty results, so tests never touch the network.
#[cfg(all(test, feature = "sim"))]
struct NoopSyncService;

#[cfg(all(test, feature = "sim"))]
impl SyncService for NoopSyncService {
    fn spawn_sync(
        &self,
        _query: IssueQuery,
        _full: bool,
        _identity: bool,
    ) -> mpsc::Receiver<SyncEvent> {
        let (_tx, rx) = mpsc::channel();
        rx
    }
    fn spawn_login(&self) -> mpsc::Receiver<LoginEvent> {
        let (_tx, rx) = mpsc::channel();
        rx
    }
    fn fetch_viewer(&self) -> Option<lt_types::viewer::User> {
        None
    }
    fn fetch_teams(&self) -> Result<Vec<Team>> {
        Ok(Vec::new())
    }
    fn fetch_workflow_states(&self, _team_id: &str) -> Result<Vec<WorkflowState>> {
        Ok(Vec::new())
    }
    fn fetch_team_members(&self, _team_id: &str) -> Result<Vec<User>> {
        Ok(Vec::new())
    }
    fn sync_comments(&self, _issue_id: &str) -> Result<()> {
        Ok(())
    }
    fn sync_teams(&self) -> Result<()> {
        Ok(())
    }
    fn sync_team_data(&self, _team_id: &str) -> Result<()> {
        Ok(())
    }
}

pub struct App {
    /// The live view stack, bottom to top. Never empty: `views[0]` is the
    /// base view for this CLI invocation -- today always the issue list. The
    /// top view is focused; every view renders, bottom to top.
    pub views: Vec<View>,

    pub args: IssueQuery,
    pub quit: bool,
    // Set by ui::render each frame so key handlers know page size.
    pub viewport_height: u16,

    // -- footer message ----------------------------------------------
    pub footer_msg: Option<String>,

    // -- background sync --------------------------------------------
    pub sync: SyncState,

    /// Terminal/session capability flags.
    pub session: Session,

    // -- active filter AST -------------------------------------------
    /// Single source of truth for the active filter/search state.
    /// Updated on Enter (confirm search), double-esc (reset), and sort shortcuts.
    pub active_filter: search_query::QueryAst,
    /// Snapshot of the filter at startup; used to reset on double-esc.
    pub initial_filter: search_query::QueryAst,

    // -- identity info -----------------------------------------------
    /// Authenticated user's display name.
    pub viewer_name: Option<String>,
    /// Linear organization (workspace) name.
    pub org_name: Option<String>,

    // -- double-esc reset --------------------------------------------
    /// The args as passed at startup; used to restore state on double-esc.
    pub initial_args: IssueQuery,
    /// Timestamp of the last Esc keypress (used to detect double-esc).
    pub last_esc_time: Option<Instant>,

    // -- re-auth -----------------------------------------------------
    /// Receiver for the background login thread, if one is in progress.
    pub login_rx: Option<mpsc::Receiver<LoginEvent>>,

    /// Database handle. Defaults to the per-profile SQLite file; tests install
    /// an in-memory database via `Database::memory`.
    pub db: lt_runtime::db::Database,

    /// Wall-clock source. Defaults to the system clock; tests install a fixed
    /// clock so time-derived labels are deterministic.
    pub clock: Clock,

    /// The sync/API edge, injected by `lt-cli`. The TUI drives all network
    /// work through this trait object, so it has no dependency on `lt-sync`.
    pub service: Arc<dyn SyncService>,

    /// Producer end of the app event queue; cloned into every background
    /// worker that emits a `StateEvent`.
    pub events_tx: mpsc::Sender<AppEvent>,
    /// The single consumer, drained once per frame in `run_app`.
    events_rx: mpsc::Receiver<AppEvent>,
}

impl App {
    // A private constructor that wires the app's initial state plus the injected
    // sync service; the fields are distinct concerns, not worth a params struct.
    #[allow(clippy::too_many_arguments)]
    fn new(
        issues: Vec<Issue>,
        pagination: Pagination,
        args: IssueQuery,
        sync: SyncState,
        service: Arc<dyn SyncService>,
    ) -> Self {
        let initial_args = args.clone();
        let active_filter = search_query::args_to_ast(&args);
        let initial_filter = active_filter.clone();
        let (events_tx, events_rx) = mpsc::channel();
        Self {
            views: vec![View::List(ListView::new(issues, pagination))],
            args,
            quit: false,
            viewport_height: 0,
            footer_msg: None,
            sync,
            session: Session {
                keyboard_enhanced: false,
                not_authenticated: false,
            },
            active_filter,
            initial_filter,
            viewer_name: None,
            org_name: None,
            initial_args,
            last_esc_time: None,
            login_rx: None,
            db: lt_runtime::db::Database::File,
            clock: Clock::System,
            service,
            events_tx,
            events_rx,
        }
    }

    /// Build an `App` for rendering tests: no background sync channel, no
    /// threads, no DB. Callers populate the view stack/`viewer_name` directly
    /// and drive `ui::render`. See `docs/design/visual-rendering-tests.md`.
    #[cfg(all(test, feature = "sim"))]
    fn for_test(issues: Vec<Issue>) -> Self {
        Self::new(
            issues,
            Pagination {
                has_next_page: false,
                current_cursor: None,
                cursor_stack: Vec::new(),
                end_cursor: None,
            },
            IssueQuery::default(),
            SyncState {
                sync_rx: None,
                syncing: false,
                sync_status_label: String::new(),
                next_sync_at: None,
            },
            Arc::new(NoopSyncService),
        )
    }

    /// Keep app.args.sort/desc in sync with `active_filter`.
    /// Called after `active_filter` is updated so that `do_fetch()` and the
    /// table sort-column marker reflect the confirmed filter state.
    fn sync_args_from_filter(&mut self) {
        let parsed = search_query::ParsedQuery::from(&self.active_filter);
        if let Some((field, dir)) = parsed.sort {
            self.args.sort = field;
            self.args.desc = dir == search_query::SortDir::Desc;
        }
    }

    /// Produce a new `QueryAst` with the sort: token replaced to match
    /// self.args.sort/desc.  Used by `cycle_sort` and `toggle_desc`.
    fn replace_sort_in_filter(&self) -> search_query::QueryAst {
        let dir = if self.args.desc { "-" } else { "+" };
        let new_sort = format!("sort:{}{}", self.args.sort.label(), dir);
        let mut parts: Vec<String> = self
            .active_filter
            .raw
            .split_whitespace()
            .filter(|t| !t.to_lowercase().starts_with("sort:"))
            .map(std::string::ToString::to_string)
            .collect();
        parts.push(new_sort);
        search_query::parse_query_ast(&parts.join(" "))
    }

    /// Read-only access to the base view, when it is a list. Non-list writers
    /// reach the base through this and `base_list_mut`; `None` (a future
    /// non-list base) degrades those writes to no-ops.
    fn base_list(&self) -> Option<&ListView> {
        match self.views.first() {
            Some(View::List(list)) => Some(list),
            _ => None,
        }
    }

    fn base_list_mut(&mut self) -> Option<&mut ListView> {
        match self.views.first_mut() {
            Some(View::List(list)) => Some(list),
            _ => None,
        }
    }

    /// Test-only infallible accessor: render/loop tests always seed a list
    /// base, so a panic here signals a broken fixture, not a runtime state to
    /// handle.
    #[cfg(all(test, feature = "sim"))]
    fn list_mut(&mut self) -> &mut ListView {
        match self.views.first_mut() {
            Some(View::List(list)) => list,
            _ => unreachable!("test base view is not a list"),
        }
    }

    fn selected_issue(&self) -> Option<&Issue> {
        self.base_list().and_then(ListView::selected_issue)
    }

    /// Pop the focused view. The stack is never empty: popping the base
    /// resets it to the default base view for this CLI invocation instead
    /// (today: the issue list rebuilt from `initial_args`/`initial_filter` -- the
    /// same reset double-esc performs). No path reaches the `else` branch
    /// today (the list's Esc is the double-esc reset below, and never pops
    /// through here); the branch defines the semantics rather than defending
    /// against a bug.
    fn pop_view(&mut self) {
        if self.views.len() > 1 {
            self.views.pop();
        } else {
            self.reset_base_view();
        }
    }

    /// Full reset to the state the TUI was launched with: sort, filters, and
    /// search query. The same reset the list's double-esc performs and
    /// `pop_view` falls back to at the floor.
    fn reset_base_view(&mut self) {
        self.args = self.initial_args.clone();
        self.active_filter = self.initial_filter.clone();
        if let Some(list) = self.base_list_mut() {
            list.pagination.cursor_stack.clear();
            list.pagination.current_cursor = None;
        }
        self.last_esc_time = None;
        self.fetch_base_list(true);
    }

    /// Build a `StateCtx` from disjoint fields and re-fetch the base list.
    fn fetch_base_list(&mut self, reset_selection: bool) {
        let ctx = StateCtx {
            db: &self.db,
            args: &self.args,
            filter: &self.active_filter,
            viewer_name: self.viewer_name.as_deref(),
        };
        if let Some(View::List(list)) = self.views.first_mut() {
            list.do_fetch(&ctx, reset_selection);
        }
    }

    fn refresh(&mut self) {
        self.fetch_base_list(false); // immediate cache read for responsiveness
        // Manual refresh triggers a full sync (not delta) to pick up all
        // remote changes, including any the delta window might miss.
        if !self.sync.syncing {
            self.sync.syncing = true;
            self.sync.sync_status_label = "full sync...".to_string();
            self.sync.sync_rx = Some(self.service.spawn_sync(
                self.args.clone(),
                true,
                self.viewer_name.is_none(),
            ));
        }
    }

    fn cycle_sort(&mut self) {
        self.args.sort = self.args.sort.next();
        self.active_filter = self.replace_sort_in_filter();
        if let Some(list) = self.base_list_mut() {
            list.pagination.cursor_stack.clear();
            list.pagination.current_cursor = None;
        }
        self.fetch_base_list(true);
    }

    fn toggle_desc(&mut self) {
        self.args.desc = !self.args.desc;
        self.active_filter = self.replace_sort_in_filter();
        if let Some(list) = self.base_list_mut() {
            list.pagination.cursor_stack.clear();
            list.pagination.current_cursor = None;
        }
        self.fetch_base_list(true);
    }

    fn next_page(&mut self) {
        self.paginate(ListView::next_page);
    }

    fn prev_page(&mut self) {
        self.paginate(ListView::prev_page);
    }

    /// Shared by `next_page`/`prev_page`: build the `StateCtx` and drive `op`
    /// against the base list.
    fn paginate(&mut self, op: fn(&mut ListView, &StateCtx)) {
        let ctx = StateCtx {
            db: &self.db,
            args: &self.args,
            filter: &self.active_filter,
            viewer_name: self.viewer_name.as_deref(),
        };
        if let Some(View::List(list)) = self.views.first_mut() {
            op(list, &ctx);
        }
    }

    /// Downcast the view at `i` via `extract`. Shared by handlers that reach
    /// their own view by stack index (detail/popup key handlers).
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
            self.fetch_base_list(true);
        }
    }

    fn open_search_overlay(&mut self) {
        let mut overlay = SearchOverlay::new();
        // Restore active filter when re-opening, unless it is just the
        // default sort stem.
        if self.active_filter.raw != search_query::DEFAULT_QUERY {
            overlay.query = TextInput::from(self.active_filter.raw.clone());
            overlay.ast = self.active_filter.clone();
            overlay.last_changed = Some(Instant::now());
        }
        self.views.push(View::Search(overlay));
    }

    /// Apply a queued app event. Only `State` lands in this stage; `Key` and
    /// `Lifecycle` arrive in later stages.
    fn apply(&mut self, event: AppEvent) {
        match event {
            AppEvent::State(ev) => self.route_state_event(&ev),
        }
    }

    /// Keys go to the focused view and cascade toward the base: an unbound
    /// key falls through to the view beneath, with `views[0]` as the floor.
    /// Mechanism-only in this stage: every handler returns `Consumed`
    /// unconditionally, so the loop always stops at the top view, matching
    /// today's single-mode dispatch exactly.
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
    /// irrelevant; top-down is chosen for coherence with the key cascade --
    /// one direction to reason about. The base list is just `views[0]`'s
    /// consumer.
    fn route_state_event(&mut self, ev: &StateEvent) {
        let ctx = StateCtx {
            db: &self.db,
            args: &self.args,
            filter: &self.active_filter,
            viewer_name: self.viewer_name.as_deref(),
        };
        let len = self.views.len();
        for (i, view) in self.views.iter_mut().enumerate().rev() {
            view.consume(&ctx, i + 1 == len, ev);
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

pub fn run(args: IssueQuery, service: Arc<dyn SyncService>) -> Result<()> {
    // Try to load issues from the local SQLite cache first (local-first UX).
    // Use query_issues_page so we can capture the correct has_next_page flag.
    let (cached_issues, initial_has_next_page, initial_end_cursor) =
        (|| -> Result<(Vec<Issue>, bool, Option<String>)> {
            let conn = lt_runtime::db::open_db(lt_runtime::db::db_path()?)?;
            let limit = i64::from(args.limit.min(250));
            let (issues, has_next) = lt_runtime::db::query_issues_page(&conn, &args, 0)?;
            let end_cursor = if has_next {
                Some(limit.to_string())
            } else {
                None
            };
            Ok((issues, has_next, end_cursor))
        })()
        .unwrap_or_default();

    let have_cache = !cached_issues.is_empty();

    // Determine whether to show "Syncing..." overlay (no cache yet).
    let (issues, has_next_page, end_cursor, syncing, initial_status) = if have_cache {
        (
            cached_issues,
            initial_has_next_page,
            initial_end_cursor,
            true,
            Status::Idle,
        )
    } else {
        (Vec::new(), false, None, true, Status::Loading)
    };

    let sync_status_label = build_sync_status_label(syncing, &Clock::System);

    // Fetch viewer identity for header display.
    let viewer = service.fetch_viewer();

    // Spawn background sync thread. When the identity fetch above failed
    // (no token yet, or an expired one), ask the sync thread to deliver it
    // once authentication succeeds so the header gets updated.
    let sync_rx = service.spawn_sync(args.clone(), false, viewer.is_none());

    let mut app = App::new(
        issues,
        Pagination {
            has_next_page,
            current_cursor: None,
            cursor_stack: Vec::new(),
            end_cursor,
        },
        args,
        SyncState {
            sync_rx: Some(sync_rx),
            syncing,
            sync_status_label,
            next_sync_at: None,
        },
        service,
    );

    if let Some(viewer) = viewer {
        app.viewer_name = Some(viewer.name);
        app.org_name = Some(viewer.organization.name);
    }

    let mut terminal = ratatui::init();
    // Without the kitty keyboard protocol, terminals encode Ctrl-Enter and
    // Enter as the same byte, so the Ctrl-Enter submit binding never fires.
    // Enable it where supported; elsewhere the UI falls back to Alt-Enter.
    let keyboard_enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if keyboard_enhanced {
        let _ = crossterm::execute!(
            std::io::stdout(),
            event::PushKeyboardEnhancementFlags(
                event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            )
        );
    }
    app.session.keyboard_enhanced = keyboard_enhanced;
    if let Some(list) = app.base_list_mut() {
        list.status = initial_status;
    }
    let result = run_app(&mut terminal, &mut CrosstermEvents, &mut app);
    if keyboard_enhanced {
        let _ = crossterm::execute!(std::io::stdout(), event::PopKeyboardEnhancementFlags);
    }
    ratatui::restore();
    result
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    events: &mut dyn EventSource,
    app: &mut App,
) -> Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    loop {
        // Poll background sync channel.
        poll_sync_events(app);

        // Periodic delta sync: fire every 30s when authenticated.
        if !app.sync.syncing
            && !app.session.not_authenticated
            && let Some(t) = app.sync.next_sync_at
            && Instant::now() >= t
        {
            app.sync.syncing = true;
            app.sync.sync_status_label = build_sync_status_label(true, &app.clock);
            app.sync.sync_rx = Some(app.service.spawn_sync(
                app.args.clone(),
                false,
                app.viewer_name.is_none(),
            ));
            app.sync.next_sync_at = None;
        }

        // Poll modal background loader channel.
        app.poll_modal_events();

        // Drain the app event queue: state invalidations from background
        // workers and same-thread writers land here.
        while let Ok(event) = app.events_rx.try_recv() {
            app.apply(event);
        }

        // Poll FTS search debounce.
        poll_search_debounce(app);

        // Poll background login channel.
        poll_login_events(app);

        terminal.draw(|frame| ui::render(frame, app))?;

        if app.quit {
            return Ok(());
        }

        if let Some(key) = events.next_key(Duration::from_millis(100))? {
            app.dispatch_key(key);
        }
    }
}

// -- Normal list keybindings -------------------------------------------------

fn handle_list_key(app: &mut App, _i: usize, key: KeyEvent) -> KeyFlow {
    // The list is always the base view in this stage, so it reaches its own
    // state through `base_list_mut` rather than the index.
    let code = key.code;
    let modifiers = key.modifiers;
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let viewport_height = app.viewport_height;
    match code {
        KeyCode::Char('q') => app.quit = true,
        KeyCode::Esc => app.handle_list_esc(),
        // Open detail pane (space opens detail)
        KeyCode::Char(' ') => app.open_detail(),
        KeyCode::Char('j') | KeyCode::Down => {
            if let Some(l) = app.base_list_mut() {
                l.move_down();
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if let Some(l) = app.base_list_mut() {
                l.move_up();
            }
        }
        KeyCode::Char('g') => {
            if let Some(l) = app.base_list_mut() {
                l.move_top();
            }
        }
        KeyCode::Char('G') => {
            if let Some(l) = app.base_list_mut() {
                l.move_bottom();
            }
        }
        KeyCode::Char('d') if ctrl => {
            if let Some(l) = app.base_list_mut() {
                l.half_page_down(viewport_height);
            }
        }
        KeyCode::Char('u') if ctrl => {
            if let Some(l) = app.base_list_mut() {
                l.half_page_up(viewport_height);
            }
        }
        KeyCode::Char('n') if ctrl => app.next_page(),
        KeyCode::Char('p') if ctrl => app.prev_page(),
        KeyCode::PageDown => {
            if let Some(l) = app.base_list_mut() {
                l.page_down(viewport_height);
            }
        }
        KeyCode::PageUp => {
            if let Some(l) = app.base_list_mut() {
                l.page_up(viewport_height);
            }
        }
        KeyCode::Char('o') => {
            if let Some(issue) = app.selected_issue() {
                let url = format!("https://linear.app/issue/{}", issue.identifier);
                let _ = open::that(url);
            }
        }
        KeyCode::Char('r') => app.refresh(),
        // 'S' (capital) cycles sort field to avoid collision with 's' (state popup)
        KeyCode::Char('S') => app.cycle_sort(),
        KeyCode::Char('d') => app.toggle_desc(),
        KeyCode::Char('/') => app.open_search_overlay(),
        // Write op keybindings
        KeyCode::Char('s') => app.open_state_popup(),
        KeyCode::Char('p') => app.open_priority_popup(),
        KeyCode::Char('a') => app.open_assignee_popup(),
        // New issue modal
        KeyCode::Char('n') => app.open_new_issue_modal(),
        // Help popup
        KeyCode::Char('?') => app.views.push(View::Help(HelpPopup::new())),
        // Re-authenticate: background OAuth login.
        KeyCode::Char('L') if app.login_rx.is_none() => {
            app.login_rx = Some(app.service.spawn_login());
            app.sync.sync_status_label =
                "logging in -- complete authorization in browser".to_string();
        }
        _ => {}
    }
    KeyFlow::Consumed
}
