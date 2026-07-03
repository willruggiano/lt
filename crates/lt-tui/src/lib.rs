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

#[cfg(all(test, feature = "sim"))]
use std::collections::VecDeque;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
pub use detail::DetailView;
#[cfg(all(test, feature = "sim"))]
pub(crate) use detail::{build_cached_detail, populate_relations};
use lt_runtime::query::IssueQuery;
use lt_runtime::search_query;
#[cfg(all(test, feature = "sim"))]
use lt_runtime::sync::service::IssueEdit;
pub use lt_runtime::sync::service::RuntimeEvent;
pub(crate) use lt_runtime::sync::service::StateEvent;
use lt_runtime::sync::service::{LoginEvent, Scope, SyncEvent, SyncService};
use lt_types::types::Issue;
#[cfg(all(test, feature = "sim"))]
pub(crate) use lt_types::types::priority_label_to_u8;
use lt_types::viewer;
#[cfg(all(test, feature = "sim"))]
pub(crate) use new_issue::build_assignee_items;
pub(crate) use new_issue::{NewIssueField, NewIssueModal};
#[cfg(all(test, feature = "sim"))]
pub(crate) use popup::handle_key as handle_popup_key;
pub(crate) use popup::{
    HelpPopup, PopupItem, PopupKind, PopupView, SearchOverlay, poll_search_debounce,
    priority_popup_items, state_items,
};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::widgets::TableState;
pub(crate) use sync::sync_status_label;
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

pub enum Status {
    Idle,
    Loading,
    Error(String),
}

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
    /// `focused` is true iff this is the top of the stack. Search/Help have
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
    /// `push_view` watches these before pushing; `pop_view` unwatches them.
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
    /// movement for `List`/`Popup`, offset scrolling for `Detail` (Decision
    /// 6). No-op default for views with no scrollable content of their own --
    /// `Search`/`Help` keep their navigation in-handler (they are text
    /// contexts and never reach this layer); `NewIssue`'s tab-focused fields
    /// have no scroll concept.
    fn scroll(&mut self, motion: ScrollMotion, viewport_height: u16) {
        match self {
            View::List(list) => list.scroll(motion, viewport_height),
            View::Detail(detail) => detail.scroll(motion, viewport_height),
            View::Popup(popup) => popup.scroll(motion, viewport_height),
            View::NewIssue(_) | View::Search(_) | View::Help(_) => {}
        }
    }
}

/// A scroll/selection motion, resolved at the focused view's [`View::scroll`]
/// when no handler in the key cascade binds the key (Decision 6). One method
/// over the shared `j`/`k`/`g`/`G`/Ctrl-d/Ctrl-u/PageDown/PageUp family,
/// rather than one method per motion: avoids eight near-identical trait
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
    /// The shared scroll-key set, checked once per key after the focused
    /// view's own bindings pass on it.
    fn from_key(key: KeyEvent) -> Option<Self> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => Some(Self::Down),
            KeyCode::Char('k') | KeyCode::Up => Some(Self::Up),
            KeyCode::Char('g') => Some(Self::Top),
            KeyCode::Char('G') => Some(Self::Bottom),
            KeyCode::Char('d') if ctrl => Some(Self::HalfPageDown),
            KeyCode::Char('u') if ctrl => Some(Self::HalfPageUp),
            KeyCode::PageDown => Some(Self::PageDown),
            KeyCode::PageUp => Some(Self::PageUp),
            _ => None,
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
    /// An identifier to seek to on the next `Issues` re-read, set by
    /// `new_issue_submit` after `create_issue` returns the optimistic
    /// identifier. Consumed (and cleared) by that re-read.
    pub pending_select: Option<String>,
    /// The issue-list query: sort/team/limit and the rest of the fields
    /// `do_fetch` reads. Kept in sync with `filter`'s `sort:` token by
    /// `sync_args_from_filter`.
    pub args: IssueQuery,
    /// Single source of truth for the active filter/search state. Updated on
    /// Enter (confirm search), double-esc (reset), and sort shortcuts.
    pub filter: search_query::QueryAst,
}

impl ListView {
    fn new(
        issues: Vec<Issue>,
        pagination: Pagination,
        args: IssueQuery,
        filter: search_query::QueryAst,
    ) -> Self {
        let mut table_state = TableState::default();
        if !issues.is_empty() {
            table_state.select(Some(0));
        }
        Self {
            issues,
            table_state,
            pagination,
            status: Status::Idle,
            pending_select: None,
            args,
            filter,
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
            self.seek_pending_select();
        }
    }

    /// The base list's re-read: `self.args`/`self.filter` plus `ctx.db` and
    /// the viewer name for `assignee:me` resolution.
    fn do_fetch(&mut self, ctx: &StateCtx, reset_selection: bool) {
        self.status = Status::Loading;
        let mut parsed = search_query::ParsedQuery::from(&self.filter);
        search_query::resolve_me(&mut parsed, ctx.viewer_name);

        if parsed.has_filters() {
            // Active filter has constraints beyond sort -- use run_query to
            // preserve them.
            let limit = self.args.limit.min(250) as usize;
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
                .and_then(|conn| lt_runtime::db::query_issues_page(&conn, &self.args, offset))
            {
                Ok((issues, has_next_page)) => {
                    self.issues = issues;
                    self.pagination.has_next_page = has_next_page;
                    let limit = i64::from(self.args.limit.min(250));
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

    /// Keep `args.sort`/`args.desc` in sync with `filter`. Called after
    /// `filter` is updated so that `do_fetch()` and the table sort-column
    /// marker reflect the confirmed filter state.
    fn sync_args_from_filter(&mut self) {
        let parsed = search_query::ParsedQuery::from(&self.filter);
        if let Some((field, dir)) = parsed.sort {
            self.args.sort = field;
            self.args.desc = dir == search_query::SortDir::Desc;
        }
    }

    /// Produce a new `QueryAst` with the sort: token replaced to match
    /// `args.sort`/`args.desc`. Used by `cycle_sort` and `toggle_desc`.
    fn replace_sort_in_filter(&self) -> search_query::QueryAst {
        let dir = if self.args.desc { "-" } else { "+" };
        let new_sort = format!("sort:{}{}", self.args.sort.label(), dir);
        let mut parts: Vec<String> = self
            .filter
            .raw
            .split_whitespace()
            .filter(|t| !t.to_lowercase().starts_with("sort:"))
            .map(std::string::ToString::to_string)
            .collect();
        parts.push(new_sort);
        search_query::parse_query_ast(&parts.join(" "))
    }

    /// `S`: cycle the sort field, rewrite `filter`'s `sort:` token to match,
    /// reset pagination, and re-fetch from the top.
    fn cycle_sort(&mut self, ctx: &StateCtx) {
        self.args.sort = self.args.sort.next();
        self.filter = self.replace_sort_in_filter();
        self.pagination.cursor_stack.clear();
        self.pagination.current_cursor = None;
        self.do_fetch(ctx, true);
    }

    /// `d`: toggle sort direction, rewrite `filter`'s `sort:` token to match,
    /// reset pagination, and re-fetch from the top.
    fn toggle_desc(&mut self, ctx: &StateCtx) {
        self.args.desc = !self.args.desc;
        self.filter = self.replace_sort_in_filter();
        self.pagination.cursor_stack.clear();
        self.pagination.current_cursor = None;
        self.do_fetch(ctx, true);
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

    /// Consume `pending_select`, if set: seek to the identifier and clear it.
    /// A miss (the create hasn't landed in this read, e.g. it is filtered
    /// out) also clears it -- `pending_select` is a one-shot seek, not a
    /// retried one.
    fn seek_pending_select(&mut self) {
        if let Some(id) = self.pending_select.take()
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

/// The eight scroll-motion primitives a stepped/offset view (`List`,
/// `Detail`) is built from. The provided `scroll` method maps a
/// [`ScrollMotion`] onto them once, so the same eight-arm dispatch isn't
/// duplicated per view (`cpd`/`cargo dupes`); implementors just name their
/// own movement primitives.
trait Scroll {
    fn motion_down(&mut self);
    fn motion_up(&mut self);
    fn motion_top(&mut self);
    fn motion_bottom(&mut self);
    fn motion_half_page_down(&mut self, viewport_height: u16);
    fn motion_half_page_up(&mut self, viewport_height: u16);
    fn motion_page_down(&mut self, viewport_height: u16);
    fn motion_page_up(&mut self, viewport_height: u16);

    fn scroll(&mut self, motion: ScrollMotion, viewport_height: u16) {
        match motion {
            ScrollMotion::Down => self.motion_down(),
            ScrollMotion::Up => self.motion_up(),
            ScrollMotion::Top => self.motion_top(),
            ScrollMotion::Bottom => self.motion_bottom(),
            ScrollMotion::HalfPageDown => self.motion_half_page_down(viewport_height),
            ScrollMotion::HalfPageUp => self.motion_half_page_up(viewport_height),
            ScrollMotion::PageDown => self.motion_page_down(viewport_height),
            ScrollMotion::PageUp => self.motion_page_up(viewport_height),
        }
    }
}

/// This view's scroll override: selection movement (Decision 6).
impl Scroll for ListView {
    fn motion_down(&mut self) {
        self.move_down();
    }
    fn motion_up(&mut self) {
        self.move_up();
    }
    fn motion_top(&mut self) {
        self.move_top();
    }
    fn motion_bottom(&mut self) {
        self.move_bottom();
    }
    fn motion_half_page_down(&mut self, viewport_height: u16) {
        self.half_page_down(viewport_height);
    }
    fn motion_half_page_up(&mut self, viewport_height: u16) {
        self.half_page_up(viewport_height);
    }
    fn motion_page_down(&mut self, viewport_height: u16) {
        self.page_down(viewport_height);
    }
    fn motion_page_up(&mut self, viewport_height: u16) {
        self.page_up(viewport_height);
    }
}

/// Read-only context a view's consume/re-query needs. Built inline from
/// disjoint `App` field borrows at each call site: an `App::state_ctx(&self)`
/// accessor would borrow all of `self` and conflict with any simultaneous
/// `&mut self.views` access.
pub struct StateCtx<'a> {
    pub db: &'a lt_runtime::db::Database,
    pub viewer_name: Option<&'a str>,
}

/// What a key handler did with a key. `Pass` hands it to the next layer:
/// the shared scroll defaults, then the cascade toward the base, then the
/// Esc/q floor (Decision 6). A handler that returns `Pass` must not have
/// mutated anything (in particular the stack), so the walk's indices stay
/// valid.
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

/// Background-sync typestate. The footer label is derived state and no
/// longer stored: it is formatted at render time from `(SyncStatus,
/// AuthStatus, Clock)` (`sync::sync_status_label`). Scheduling -- and so
/// `next_sync_at` -- belongs to the loop now (Decision 2); this typestate is
/// a pure consumer of the events it reports.
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

/// Authentication typestate. The TUI never holds tokens (they live in
/// `lt-config`/`lt-upstream` behind the `SyncService` seam); its witness of
/// authentication is the viewer identity.
pub enum AuthStatus {
    /// The startup identity fetch failed but a token may exist; the
    /// in-flight startup sync resolves this. Not `Unauthenticated`: the
    /// periodic-retry gate must not block a token-holding user who is merely
    /// offline (`fetch_viewer` `None` + first sync `Error`).
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
///
/// `watch`/`unwatch`/`request_sync`/`login` never touch the network; they
/// record their call so tests assert on the recording instead of a live
/// thread. The write methods (`create_comment`/`edit_issue`/`create_issue`)
/// delegate to a real [`crate::LinearSyncService`] sharing the test's
/// in-memory database (see [`lt_runtime::db::Database::share`]), so they
/// perform the real enqueue and emit through the same `on_event` the test
/// wired -- synchronously, so tests stay thread-free. `run` is a no-op: no
/// test drives the loop itself; they script `AppEvent::Runtime(..)` instead
/// (see `loop_tests`).
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
    /// The live view stack, bottom to top. Never empty: `views[0]` is the
    /// base view for this CLI invocation -- today always the issue list. The
    /// top view is focused; every view renders, bottom to top.
    pub views: Vec<View>,

    pub quit: bool,
    // Set by ui::render each frame so key handlers know page size.
    pub viewport_height: u16,

    // -- footer message ----------------------------------------------
    pub footer_msg: Option<String>,

    // -- background-job typestates (Decision 6) -----------------------
    pub sync: SyncStatus,
    pub auth: AuthStatus,

    /// Terminal/session capability flags.
    pub session: Session,

    // -- launch seeds / double-esc reset -------------------------------
    /// Snapshot of the filter at startup; the base list's own `filter`
    /// (Decision 5) is the live copy. Used to reset on double-esc.
    pub initial_filter: search_query::QueryAst,
    /// The args as passed at startup; the base list's own `args`
    /// (Decision 5) is the live copy. Used to restore state on double-esc.
    pub initial_args: IssueQuery,
    /// Timestamp of the last Esc keypress (used to detect double-esc).
    pub last_esc_time: Option<Instant>,

    /// Database handle. Defaults to the per-profile SQLite file; tests install
    /// an in-memory database via `Database::memory`.
    pub db: lt_runtime::db::Database,

    /// Wall-clock source. Defaults to the system clock; tests install a fixed
    /// clock so time-derived labels are deterministic.
    pub clock: Clock,

    /// The sync/API edge, injected by `lt-cli`. The TUI drives all network
    /// work through this trait object, so it has no dependency on `lt-sync`.
    pub service: Arc<dyn SyncService>,

    /// The single consumer of the app event queue, drained once per frame in
    /// `run_app`. `lt-cli` owns the sender: it feeds the input thread and
    /// wraps it into the service's `OnEvent` callback.
    events_rx: mpsc::Receiver<AppEvent>,
}

impl App {
    // A private constructor that wires the app's initial state plus the
    // injected sync service and the queue's receiving end (`lt-cli` owns the
    // sender). `sync`/`auth` start at their unstarted typestates
    // (`Idle`/`Unknown`); `run()` transitions `auth` from `fetch_viewer()`
    // before the loop starts, and `sync` transitions on the loop's first
    // `Sync(Started)`.
    fn new(
        list: ListView,
        service: Arc<dyn SyncService>,
        events_rx: mpsc::Receiver<AppEvent>,
    ) -> Self {
        let initial_args = list.args.clone();
        let initial_filter = list.filter.clone();
        Self {
            views: vec![View::List(list)],
            quit: false,
            viewport_height: 0,
            footer_msg: None,
            sync: SyncStatus::Idle,
            auth: AuthStatus::Unknown,
            session: Session {
                keyboard_enhanced: false,
            },
            initial_filter,
            initial_args,
            last_esc_time: None,
            db: lt_runtime::db::Database::File,
            clock: Clock::System,
            service,
            events_rx,
        }
    }

    /// Build an `App` for rendering tests: a throwaway in-memory database and
    /// event channel, and a [`RecordingSyncService`] sharing that database.
    /// Callers populate the view stack/`auth` directly and drive
    /// `ui::render`. See `docs/design/visual-rendering-tests.md`. Fallible
    /// (in-memory SQLite setup): callers -- always `#[test]` fns -- unwrap.
    #[cfg(all(test, feature = "sim"))]
    fn for_test(issues: Vec<Issue>) -> Result<Self> {
        let db = lt_runtime::db::Database::memory()?;
        let (tx, rx) = mpsc::channel();
        let service = RecordingSyncService::new(&db, tx)?;
        let args = IssueQuery::default();
        let filter = search_query::args_to_ast(&args);
        let list = ListView::new(
            issues,
            Pagination {
                has_next_page: false,
                current_cursor: None,
                cursor_stack: Vec::new(),
                end_cursor: None,
            },
            args,
            filter,
        );
        let mut app = Self::new(list, Arc::new(service), rx);
        app.db = db;
        Ok(app)
    }

    /// Swap in a fresh database, shared with a fresh [`RecordingSyncService`]
    /// and event channel -- so `app.db` and `app.service`'s writes/reads
    /// agree on the same rows. Used by tests that need a specific seeded
    /// database (`loop_tests::app_with_db`).
    #[cfg(all(test, feature = "sim"))]
    fn install_db(&mut self, db: lt_runtime::db::Database) -> Result<()> {
        self.install_recording_service(&db)?;
        self.db = db;
        Ok(())
    }

    /// Swap in a fresh `RecordingSyncService` (and its paired event channel)
    /// sharing `db`, returning it so the caller can assert on its recording
    /// (`watch`/`unwatch`/`request_sync`/`login` calls) -- the equivalent of
    /// the old `CountingSyncService`.
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

    /// The base list's query limit, degrading to `IssueQuery::default()`'s
    /// when the base is not a list (a future non-list base has none). Used
    /// by the search overlay, which caps its results at the same limit.
    fn list_limit(&self) -> u32 {
        self.base_list()
            .map_or_else(|| IssueQuery::default().limit, |l| l.args.limit)
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

    /// Push a view, watching the scopes it declares (Decision 3). The
    /// counterpart to `pop_view`'s unwatch.
    fn push_view(&mut self, view: View) {
        for scope in view.scopes() {
            self.service.watch(scope);
        }
        self.views.push(view);
    }

    /// Pop the focused view, unwatching the scopes it declared. The stack is
    /// never empty: popping the base resets it to the default base view for
    /// this CLI invocation instead (today: the issue list rebuilt from
    /// `initial_args`/`initial_filter` -- the same reset double-esc
    /// performs). No path reaches the `else` branch today (the list's Esc is
    /// the double-esc reset below, and never pops through here); the branch
    /// defines the semantics rather than defending against a bug.
    fn pop_view(&mut self) {
        if self.views.len() > 1 {
            if let Some(view) = self.views.pop() {
                for scope in view.scopes() {
                    self.service.unwatch(scope);
                }
            }
        } else {
            self.reset_base_view();
        }
    }

    /// Full reset to the state the TUI was launched with: sort, filters, and
    /// search query. The same reset the list's double-esc performs and
    /// `pop_view` falls back to at the floor.
    fn reset_base_view(&mut self) {
        let args = self.initial_args.clone();
        let filter = self.initial_filter.clone();
        if let Some(list) = self.base_list_mut() {
            list.args = args;
            list.filter = filter;
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
            viewer_name: self.auth.viewer_name(),
        };
        if let Some(View::List(list)) = self.views.first_mut() {
            list.do_fetch(&ctx, reset_selection);
        }
    }

    /// `r`: an immediate re-read plus a request to the loop for a full
    /// sync -- no typestate write; `Syncing` arrives via the loop's own
    /// `Sync(Started)`. Pressed mid-cycle, it coalesces into a follow-up
    /// sync instead of being ignored (the loop processes commands one at a
    /// time; this one just queues behind the in-flight cycle).
    fn refresh(&mut self) {
        self.fetch_base_list(false); // immediate re-read for responsiveness
        self.service.request_sync();
    }

    fn cycle_sort(&mut self) {
        self.with_base_list(ListView::cycle_sort);
    }

    fn toggle_desc(&mut self) {
        self.with_base_list(ListView::toggle_desc);
    }

    fn next_page(&mut self) {
        self.with_base_list(ListView::next_page);
    }

    fn prev_page(&mut self) {
        self.with_base_list(ListView::prev_page);
    }

    /// Build the (now slim) `StateCtx` and drive `op` against the base list
    /// -- shared by pagination and the sort commands, which just mutate
    /// list-owned query/pagination state and re-fetch.
    fn with_base_list(&mut self, op: fn(&mut ListView, &StateCtx)) {
        let ctx = StateCtx {
            db: &self.db,
            viewer_name: self.auth.viewer_name(),
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
        // Restore the base list's filter when re-opening, unless it is just
        // the default sort stem. `base_list()` degrades a future non-list
        // base to the freshly-created default overlay (Decision 5).
        if let Some(filter) = self.base_list().map(|l| l.filter.clone())
            && filter.raw != search_query::DEFAULT_QUERY
        {
            overlay.query = TextInput::from(filter.raw.clone());
            overlay.ast = filter;
            overlay.last_changed = Some(Instant::now());
        }
        self.push_view(View::Search(overlay));
    }

    /// Apply a queued app event: a key cascades through `dispatch_key`; a
    /// state invalidation walks the view stack; a sync/login outcome
    /// transitions the typestates (Decision 7).
    fn apply(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.dispatch_key(key),
            AppEvent::Runtime(RuntimeEvent::State(ev)) => self.route_state_event(&ev),
            AppEvent::Runtime(RuntimeEvent::Sync(ev)) => self.consume_sync_event(ev),
            AppEvent::Runtime(RuntimeEvent::Login(ev)) => self.consume_login_event(ev),
        }
    }

    /// Four layers, checked in order (Decision 6): the focused view's own
    /// bindings; the shared scroll defaults, resolved at the focused view
    /// only (they never cascade); the cascade toward the base for anything
    /// else unbound; and the Esc/q floor -- Back above the base, reset/quit
    /// at it.
    fn dispatch_key(&mut self, key: KeyEvent) {
        let top = self.views.len() - 1;
        if matches!(self.handle_view_key(top, key), KeyFlow::Consumed) {
            return;
        }
        if let Some(motion) = ScrollMotion::from_key(key) {
            let viewport = self.viewport_height;
            if let Some(view) = self.views.last_mut() {
                view.scroll(motion, viewport);
            }
            return;
        }
        for i in (0..top).rev() {
            if matches!(self.handle_view_key(i, key), KeyFlow::Consumed) {
                return;
            }
        }
        match key.code {
            // Back, above the base: `q` never reaches Quit from an overlay.
            KeyCode::Esc | KeyCode::Char('q') if top > 0 => self.pop_view(),
            KeyCode::Esc => self.handle_list_esc(), // double-esc reset, unchanged
            KeyCode::Char('q') => self.quit = true,
            _ => {}
        }
    }

    /// Dispatch `key` to the view at stack index `i`'s own key handler.
    fn handle_view_key(&mut self, i: usize, key: KeyEvent) -> KeyFlow {
        let handler: KeyHandler = match &self.views[i] {
            View::List(_) => handle_list_key,
            View::Detail(_) => detail::handle_key,
            View::Popup(_) => popup::handle_key,
            View::NewIssue(_) => new_issue::handle_key,
            View::Search(_) => popup::handle_search_key,
            View::Help(_) => popup::handle_help_key,
        };
        handler(self, i, key)
    }

    /// Route a state invalidation down the stack, top first. Applies are
    /// idempotent payload-free re-reads, so the order is semantically
    /// irrelevant; top-down is chosen for coherence with the key cascade --
    /// one direction to reason about. The base list is just `views[0]`'s
    /// consumer.
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

    /// The base list's Loading->Idle repair: a sync outcome that will not
    /// itself route an `Issues` invalidation (`Error`/`NotAuthenticated`)
    /// must not leave the list's own status stuck at `Loading` forever.
    fn repair_loading_list(&mut self) {
        if let Some(list) = self.base_list_mut()
            && matches!(list.status, Status::Loading)
        {
            list.status = Status::Idle;
        }
    }

    /// `synced_at` for a `Done` transition: the DB meta `last_synced_at`
    /// every successful sync writes, falling back to the clock (exact, since
    /// the sync just finished) when the read fails or the row is
    /// missing/unparseable.
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

    /// Transition the `sync` typestate (and `auth`, when a cycle delivered an
    /// identity) per Decision 7's table. The `State(Issues)` the loop emits
    /// alongside `Sync(Done)` is a separate queued event, routed through
    /// `route_state_event` -- this consumer no longer derives it.
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
                self.repair_loading_list();
            }
            SyncEvent::NotAuthenticated => {
                self.auth = AuthStatus::Unauthenticated;
                self.sync = SyncStatus::Idle;
                self.repair_loading_list();
            }
        }
    }

    /// Transition the `auth` typestate per Decision 7's table. The
    /// follow-up delta sync after a successful login is the loop's now
    /// (Decision 2), not this consumer's.
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
                // pin the message past the actions that clear it today.
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
    // Try to load issues already synced locally first (local-first UX). Use
    // query_issues_page so we can capture the correct has_next_page flag.
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
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "startup: failed to load cached issues");
            (Vec::new(), false, None)
        });

    let have_cache = !cached_issues.is_empty();

    // Determine whether to show "Syncing..." overlay (nothing synced yet).
    let (issues, has_next_page, end_cursor, initial_status) = if have_cache {
        (
            cached_issues,
            initial_has_next_page,
            initial_end_cursor,
            Status::Idle,
        )
    } else {
        (Vec::new(), false, None, Status::Loading)
    };

    // Fetch viewer identity for header display before `service` moves into
    // `App::new` (a shared read through the `Arc`, so ownership is fine
    // either way; the identity is needed to seed `auth` regardless).
    let viewer = service.fetch_viewer();

    let filter = search_query::args_to_ast(&args);
    let list = ListView::new(
        issues,
        Pagination {
            has_next_page,
            current_cursor: None,
            cursor_stack: Vec::new(),
            end_cursor,
        },
        args,
        filter,
    );
    let mut app = App::new(list, service, events_rx);

    app.auth = match viewer {
        Some(viewer) => AuthStatus::Authenticated { viewer },
        None => AuthStatus::Unknown,
    };
    // `sync` stays `Idle` until the loop's own `Sync(Started)` arrives; the
    // loop's `run` (spawned by `lt-cli` before this function is called) owns
    // the startup sync.

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
    if let Some(list) = app.base_list_mut() {
        list.status = initial_status;
    }
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

/// Detached input thread: blocks on `event::read()` and forwards every key
/// press onto the app event queue. Resize/mouse/release events are dropped,
/// as today. The thread outlives `run_app` -- it exits on a `send` failure
/// (the app dropped `events_rx`) or a read error (the terminal is gone).
fn spawn_input_thread(tx: mpsc::Sender<AppEvent>) {
    std::thread::spawn(move || {
        loop {
            match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    if tx.send(AppEvent::Key(key)).is_err() {
                        return;
                    }
                }
                Ok(_) => {}       // resize/mouse/release: dropped, as today
                Err(_) => return, // terminal gone
            }
        }
    });
}

/// Where the loop's blocking wait gets its first event each frame. A closed
/// set (cf. `Clock` and `db::Database`): the channel in the binary, a script
/// in tests.
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
        // The loop's clock owns sync scheduling now (Decision 2); the frame
        // loop's own inline timer is only `poll_search_debounce`.
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

// -- Normal list keybindings -------------------------------------------------

fn handle_list_key(app: &mut App, _i: usize, key: KeyEvent) -> KeyFlow {
    // The list is always the base view in this stage, so it reaches its own
    // state through `base_list_mut` rather than the index. Movement
    // (j/k/g/G/Ctrl-d/Ctrl-u/PageDown/PageUp) and Esc/q are not bound here:
    // they resolve at the scroll-default and floor layers of `dispatch_key`
    // (Decision 6).
    let code = key.code;
    let modifiers = key.modifiers;
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        // Open detail pane (space opens detail)
        KeyCode::Char(' ') => app.open_detail(),
        KeyCode::Char('n') if ctrl => app.next_page(),
        KeyCode::Char('p') if ctrl => app.prev_page(),
        KeyCode::Char('o') => {
            if let Some(issue) = app.selected_issue() {
                let url = format!("https://linear.app/issue/{}", issue.identifier);
                if let Err(e) = open::that(url) {
                    tracing::warn!(error = %e, "failed to open browser for issue url");
                }
            }
        }
        KeyCode::Char('r') => app.refresh(),
        // 'S' (capital) cycles sort field to avoid collision with 's' (state popup)
        KeyCode::Char('S') => app.cycle_sort(),
        // 'd' toggles sort direction; guarded so Ctrl-d (half-page-down, a
        // scroll default) doesn't also match this bare pattern.
        KeyCode::Char('d') if !ctrl => app.toggle_desc(),
        KeyCode::Char('/') => app.open_search_overlay(),
        // Write op keybindings
        KeyCode::Char('s') => app.open_state_popup(),
        KeyCode::Char('p') => app.open_priority_popup(),
        KeyCode::Char('a') => app.open_assignee_popup(),
        // New issue modal
        KeyCode::Char('n') => app.open_new_issue_modal(),
        // Help popup
        KeyCode::Char('?') => app.push_view(View::Help(HelpPopup::new())),
        // Re-authenticate: background OAuth login.
        KeyCode::Char('L') if !matches!(app.auth, AuthStatus::Authenticating) => {
            app.auth = AuthStatus::Authenticating;
            app.service.login();
        }
        _ => return KeyFlow::Pass,
    }
    KeyFlow::Consumed
}
