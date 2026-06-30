mod detail;
mod markdown;
mod new_issue;
mod popup;
mod search_query;
mod sync;
mod text_input;
mod ui;

#[cfg(all(test, feature = "sim"))]
mod render_tests;

#[cfg(all(test, feature = "sim"))]
mod loop_tests;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
#[cfg(all(test, feature = "sim"))]
pub(crate) use detail::{build_cached_detail, populate_relations};
pub(crate) use detail::{handle_detail_key, poll_detail_comment_events};
#[cfg(all(test, feature = "sim"))]
pub(crate) use new_issue::{Member, ModalEvent, build_assignee_items};
pub(crate) use new_issue::{
    NewIssueField, NewIssueModal, fetch_team_members, handle_new_issue_key,
};
pub(crate) use popup::{
    HelpPopup, PopupItem, PopupKind, SearchOverlay, handle_help_key, handle_popup_key,
    handle_search_key, poll_search_debounce, priority_popup_items,
};
#[cfg(all(test, feature = "sim"))]
pub(crate) use popup::{apply_optimistic_in_memory, build_db_issue_optimistic};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::widgets::TableState;
pub(crate) use sync::{
    build_sync_status_label, poll_login_events, poll_sync_events, spawn_login_thread,
    spawn_sync_thread,
};
pub(crate) use text_input::TextInput;

use crate::issues::IssueArgs;
use crate::linear::client::HttpTransport;
#[cfg(all(test, feature = "sim"))]
pub(crate) use crate::linear::types::priority_label_to_u8;
use crate::linear::types::{Issue, IssueDetail};
use crate::linear::viewer::fetch_viewer;

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
// Background sync events
// ---------------------------------------------------------------------------

/// Events sent from the background sync thread to the TUI event loop.
pub enum SyncEvent {
    /// Sync completed successfully; includes the refreshed issue list and,
    /// when requested, the authenticated identity for the header.
    Done(Vec<Issue>, Option<crate::linear::viewer::Viewer>),
    /// Sync encountered an error.
    Error(String),
    /// No auth token found -- sync was skipped.
    NotAuthenticated,
}

// ---------------------------------------------------------------------------
// Background comment sync events
// ---------------------------------------------------------------------------

/// Events sent from the background comment-sync thread to the TUI event loop.
pub enum CommentSyncEvent {
    /// Comments refreshed successfully from the Linear API.
    Done(Vec<crate::linear::types::Comment>),
    /// Comment sync error (non-fatal; cached data remains shown).
    Error(String),
    /// Posting a new comment failed; the optimistic comment must be dropped.
    PostError(String),
}

// ---------------------------------------------------------------------------
// Background login events
// ---------------------------------------------------------------------------

/// Events sent from the background login thread to the TUI event loop.
pub enum LoginEvent {
    /// OAuth login completed successfully.
    Success {
        viewer_name: Option<String>,
        org_name: Option<String>,
    },
    /// Login failed with an error message.
    Error(String),
}

/// Application mode -- only one active at a time.
pub enum Mode {
    /// Normal list browsing mode.
    List,
    /// Detail pane showing full issue content.
    Detail,
    /// A generic list-picker popup is open.
    Popup(PopupKind),
    /// New-issue modal form.
    NewIssue,
    /// Searchable help popup.
    Help,
    /// FTS incremental search overlay.
    Search,
}

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

pub struct App {
    pub issues: Vec<Issue>,
    pub table_state: TableState,
    pub args: IssueArgs,
    pub pagination: Pagination,
    pub status: Status,
    pub quit: bool,
    // Filter overlay (input_mode mirrors Mode::InputFilter for compatibility).
    pub input_mode: bool,
    pub input_buf: String,
    // Set by ui::render each frame so key handlers know page size.
    pub viewport_height: u16,

    // -- mode -----------------------------------------------------------------
    pub mode: Mode,

    // -- detail pane -------------------------------------------------
    /// Loaded detail for the currently-open issue.
    pub detail: Option<IssueDetail>,
    /// Vertical scroll offset inside the detail pane (in lines).
    pub detail_scroll: u16,

    // -- popup state -------------------------------------------------
    pub popup_items: Vec<PopupItem>,
    pub popup_selected: usize,

    // -- footer message ----------------------------------------------
    pub footer_msg: Option<String>,

    // -- new-issue modal --------------------------------------------
    pub new_issue_modal: Option<NewIssueModal>,

    // -- background sync --------------------------------------------
    pub sync: SyncState,

    // -- background comment sync ------------------------------------
    /// Receiver for background comment-sync events.
    pub detail_comment_rx: Option<mpsc::Receiver<CommentSyncEvent>>,

    // -- comment input --------------------------------------------------------
    /// Multiline buffer for a new comment, open in the detail pane.
    /// The cursor is always at the end (same model as the new-issue
    /// description field).
    pub comment_input: Option<String>,

    /// Terminal/session capability flags.
    pub session: Session,

    // -- help popup -------------------------------------------------
    pub help_popup: Option<HelpPopup>,

    // -- FTS search overlay -------------------------------------------
    pub search_overlay: Option<SearchOverlay>,

    // -- popup anchor ------------------------------------------------
    /// Screen rect of the cell that triggered the popup, used to position it.
    pub popup_anchor: Option<ratatui::layout::Rect>,

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
    pub initial_args: IssueArgs,
    /// Timestamp of the last Esc keypress (used to detect double-esc).
    pub last_esc_time: Option<Instant>,

    // -- re-auth -----------------------------------------------------
    /// Receiver for the background login thread, if one is in progress.
    pub login_rx: Option<mpsc::Receiver<LoginEvent>>,

    /// Database handle. Defaults to the per-profile SQLite file; tests install
    /// an in-memory database via `Database::memory`.
    pub db: crate::db::Database,

    /// Wall-clock source. Defaults to the system clock; tests install a fixed
    /// clock so time-derived labels are deterministic.
    pub clock: Clock,
}

impl App {
    fn new(issues: Vec<Issue>, pagination: Pagination, args: IssueArgs, sync: SyncState) -> Self {
        let mut table_state = TableState::default();
        if !issues.is_empty() {
            table_state.select(Some(0));
        }
        let initial_args = args.clone();
        let active_filter = search_query::args_to_ast(&args);
        let initial_filter = active_filter.clone();
        Self {
            issues,
            table_state,
            args,
            pagination,
            status: Status::Idle,
            quit: false,
            input_mode: false,
            input_buf: String::new(),
            viewport_height: 0,
            mode: Mode::List,
            detail: None,
            detail_scroll: 0,
            popup_items: Vec::new(),
            popup_selected: 0,
            footer_msg: None,
            new_issue_modal: None,
            sync,
            detail_comment_rx: None,
            comment_input: None,
            session: Session {
                keyboard_enhanced: false,
                not_authenticated: false,
            },
            help_popup: None,
            search_overlay: None,
            popup_anchor: None,
            active_filter,
            initial_filter,
            viewer_name: None,
            org_name: None,
            initial_args,
            last_esc_time: None,
            login_rx: None,
            db: crate::db::Database::Profile,
            clock: Clock::System,
        }
    }

    /// Build an `App` for rendering tests: no background sync channel, no
    /// threads, no DB. Callers populate `mode`/`detail`/`viewer_name` directly
    /// and drive `ui::render`. See `docs/design/visual-rendering-tests.md`.
    #[cfg(all(test, feature = "sim"))]
    fn for_test(issues: Vec<Issue>) -> Self {
        let mut app = Self::new(
            issues,
            Pagination {
                has_next_page: false,
                current_cursor: None,
                cursor_stack: Vec::new(),
                end_cursor: None,
            },
            IssueArgs::default(),
            SyncState {
                sync_rx: None,
                syncing: false,
                sync_status_label: String::new(),
                next_sync_at: None,
            },
        );
        app.status = Status::Idle;
        app
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

    fn selected_issue(&self) -> Option<&Issue> {
        self.table_state.selected().and_then(|i| self.issues.get(i))
    }

    fn selected_issue_mut(&mut self) -> Option<&mut Issue> {
        self.table_state
            .selected()
            .and_then(|i| self.issues.get_mut(i))
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
    fn page_down(&mut self) {
        self.move_by(i32::from(self.viewport_height));
    }
    fn page_up(&mut self) {
        self.move_by(-i32::from(self.viewport_height));
    }
    fn half_page_down(&mut self) {
        self.move_by(i32::from(self.viewport_height) / 2);
    }
    fn half_page_up(&mut self) {
        self.move_by(-(i32::from(self.viewport_height) / 2));
    }

    fn do_fetch(&mut self, reset_selection: bool) {
        self.status = Status::Loading;
        let mut parsed = search_query::ParsedQuery::from(&self.active_filter);
        // Resolve "me" to actual viewer name for assignee filter.
        search_query::resolve_me(&mut parsed, self.viewer_name.as_deref());

        if parsed.has_filters() {
            // Active filter has constraints beyond sort -- use run_query to
            // preserve them.
            let limit = self.args.limit.min(250) as usize;
            match self
                .db
                .connect()
                .and_then(|conn| search_query::run_query(&conn, &parsed, limit))
            {
                Ok(db_issues) => {
                    self.issues = db_issues.into_iter().map(Into::into).collect();
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
            match self
                .db
                .connect()
                .and_then(|conn| crate::db::query_issues_page(&conn, &self.args, offset))
            {
                Ok((issues, has_next_page)) => {
                    self.issues = issues.into_iter().map(Into::into).collect();
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
    fn do_fetch_and_select(&mut self, target_identifier: Option<String>) {
        self.do_fetch(true);
        if let Some(id) = target_identifier
            && let Some(idx) = self.issues.iter().position(|i| i.identifier == id)
        {
            self.table_state.select(Some(idx));
        }
    }

    fn refresh(&mut self) {
        self.do_fetch(false); // immediate cache read for responsiveness
        // Manual refresh triggers a full sync (not delta) to pick up all
        // remote changes, including any the delta window might miss.
        if !self.sync.syncing {
            self.sync.syncing = true;
            self.sync.sync_status_label = "full sync...".to_string();
            self.sync.sync_rx = Some(spawn_sync_thread(
                self.args.clone(),
                true,
                self.viewer_name.is_none(),
            ));
        }
    }

    fn cycle_sort(&mut self) {
        self.args.sort = self.args.sort.next();
        self.active_filter = self.replace_sort_in_filter();
        self.pagination.cursor_stack.clear();
        self.pagination.current_cursor = None;
        self.do_fetch(true);
    }

    fn toggle_desc(&mut self) {
        self.args.desc = !self.args.desc;
        self.active_filter = self.replace_sort_in_filter();
        self.pagination.cursor_stack.clear();
        self.pagination.current_cursor = None;
        self.do_fetch(true);
    }

    fn next_page(&mut self) {
        if !self.pagination.has_next_page {
            return;
        }
        let end = self.pagination.end_cursor.clone();
        self.pagination
            .cursor_stack
            .push(self.pagination.current_cursor.clone());
        self.pagination.current_cursor = end;
        self.do_fetch(true);
    }

    fn prev_page(&mut self) {
        let Some(cursor) = self.pagination.cursor_stack.pop() else {
            return;
        };
        self.pagination.current_cursor = cursor;
        self.do_fetch(true);
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

pub fn run(args: IssueArgs) -> Result<()> {
    // Try to load issues from the local SQLite cache first (local-first UX).
    // Use query_issues_page so we can capture the correct has_next_page flag.
    let (cached_issues, initial_has_next_page, initial_end_cursor) =
        (|| -> Result<(Vec<Issue>, bool, Option<String>)> {
            let conn = crate::db::open_db()?;
            let limit = i64::from(args.limit.min(250));
            let (db_issues, has_next) = crate::db::query_issues_page(&conn, &args, 0)?;
            let end_cursor = if has_next {
                Some(limit.to_string())
            } else {
                None
            };
            let issues = db_issues.into_iter().map(Into::into).collect();
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
    let viewer = crate::config::load_token()
        .ok()
        .flatten()
        .and_then(|token| fetch_viewer(&HttpTransport::new(token.access_token)).ok());

    // Spawn background sync thread. When the identity fetch above failed
    // (no token yet, or an expired one), ask the sync thread to deliver it
    // once authentication succeeds so the header gets updated.
    let sync_rx = spawn_sync_thread(args.clone(), false, viewer.is_none());

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
    );

    if let Some(viewer) = viewer {
        app.viewer_name = Some(viewer.name);
        app.org_name = Some(viewer.org_name);
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
    app.status = initial_status;
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
            app.sync.sync_rx = Some(spawn_sync_thread(
                app.args.clone(),
                false,
                app.viewer_name.is_none(),
            ));
            app.sync.next_sync_at = None;
        }

        // Poll modal background loader channel.
        app.poll_modal_events();

        // Poll background comment-sync channel.
        poll_detail_comment_events(app);

        // Poll FTS search debounce.
        poll_search_debounce(app);

        // Poll background login channel.
        poll_login_events(app);

        terminal.draw(|frame| ui::render(frame, app))?;

        if app.quit {
            return Ok(());
        }

        if let Some(key) = events.next_key(Duration::from_millis(100))? {
            match app.mode {
                Mode::Popup(_) => handle_popup_key(app, key.code),
                Mode::Detail => handle_detail_key(app, key.code, key.modifiers),
                Mode::NewIssue => handle_new_issue_key(app, key.code, key.modifiers),
                Mode::Help => handle_help_key(app, key.code, key.modifiers),
                Mode::Search => handle_search_key(app, key.code, key.modifiers),
                Mode::List => handle_normal_key(app, key.code, key.modifiers),
            }
        }
    }
}

// -- Normal list keybindings -------------------------------------------------

fn handle_normal_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Char('q') => app.quit = true,
        KeyCode::Esc => {
            // Double-esc (within 500ms) resets sort, filters, and search query
            // back to the state the TUI was launched with.
            let now = Instant::now();
            let is_double_esc = app
                .last_esc_time
                .is_some_and(|t| t.elapsed() < Duration::from_millis(500));
            if is_double_esc {
                // Full reset to initial state.
                app.args = app.initial_args.clone();
                app.active_filter = app.initial_filter.clone();
                app.pagination.cursor_stack.clear();
                app.pagination.current_cursor = None;
                app.last_esc_time = None;
                app.do_fetch(true);
            } else {
                // First esc: standard refresh.
                app.last_esc_time = Some(now);
                app.do_fetch(true);
            }
        }
        // Open detail pane (space opens detail)
        KeyCode::Char(' ') => app.open_detail(),
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
        KeyCode::Char('g') => app.move_top(),
        KeyCode::Char('G') => app.move_bottom(),
        KeyCode::Char('d') if ctrl => app.half_page_down(),
        KeyCode::Char('u') if ctrl => app.half_page_up(),
        KeyCode::Char('n') if ctrl => app.next_page(),
        KeyCode::Char('p') if ctrl => app.prev_page(),
        KeyCode::PageDown => app.page_down(),
        KeyCode::PageUp => app.page_up(),
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
        KeyCode::Char('/') => {
            let mut overlay = SearchOverlay::new();
            // Restore active filter when re-opening, unless it is just the
            // default sort stem.
            if app.active_filter.raw != search_query::DEFAULT_QUERY {
                overlay.query = TextInput::from(app.active_filter.raw.clone());
                overlay.ast = app.active_filter.clone();
                overlay.last_changed = Some(Instant::now());
            }
            app.search_overlay = Some(overlay);
            app.mode = Mode::Search;
        }
        // Write op keybindings
        KeyCode::Char('s') => app.open_state_popup(),
        KeyCode::Char('p') => app.open_priority_popup(),
        KeyCode::Char('a') => app.open_assignee_popup(),
        // New issue modal
        KeyCode::Char('n') => app.open_new_issue_modal(),
        // Help popup
        KeyCode::Char('?') => {
            app.help_popup = Some(HelpPopup::new());
            app.mode = Mode::Help;
        }
        // Re-authenticate: background OAuth login.
        KeyCode::Char('L') if app.login_rx.is_none() => {
            app.login_rx = Some(spawn_login_thread());
            app.sync.sync_status_label =
                "logging in -- complete authorization in browser".to_string();
        }
        _ => {}
    }
}
