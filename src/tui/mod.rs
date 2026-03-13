mod search_query;
mod ui;

use std::sync::mpsc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// TextInput -- single-line text field with vim-style editing
// ---------------------------------------------------------------------------

/// A single-line text input with a byte-offset cursor and vim-style bindings.
///
/// Bindings handled in `handle_key`:
///   Backspace / ctrl+h  -- delete char before cursor
///   ctrl+w              -- delete word before cursor
///   ctrl+u              -- delete to start of line
///   ctrl+k              -- delete to end of line
///   ctrl+d / Delete     -- delete char under cursor
///   alt+d               -- delete word after cursor
///   ctrl+a / Home       -- move to start
///   ctrl+e / End        -- move to end
///   ctrl+f / Right      -- move right one char
///   ctrl+b / Left       -- move left one char
///   ctrl+Left           -- move left one word
///   ctrl+Right          -- move right one word
#[derive(Clone, Default)]
pub struct TextInput {
    pub value: String,
    /// Byte offset of the cursor, always on a char boundary.
    pub cursor: usize,
    /// If set, the range cursor..selection_end is "selected" (highlighted).
    /// selection_end is always >= cursor and always on a char boundary.
    /// Typing a character replaces the selection; movement keys clear it.
    pub selection_end: Option<usize>,
}

impl TextInput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_string(s: String) -> Self {
        let cursor = s.len();
        Self {
            value: s,
            cursor,
            selection_end: None,
        }
    }

    #[allow(dead_code)]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// Returns `(before_cursor, char_at_cursor_or_none, after_cursor_past_that_char)`.
    /// Useful for rendering the cursor position.
    pub fn display_parts(&self) -> (&str, Option<char>, &str) {
        let before = &self.value[..self.cursor];
        let rest = &self.value[self.cursor..];
        let mut chars = rest.chars();
        let ch = chars.next();
        (before, ch, chars.as_str())
    }

    fn prev_char_boundary(&self) -> usize {
        if self.cursor == 0 {
            return 0;
        }
        let mut i = self.cursor - 1;
        while !self.value.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    fn next_char_boundary(&self) -> usize {
        if self.cursor >= self.value.len() {
            return self.value.len();
        }
        let ch = self.value[self.cursor..].chars().next().unwrap();
        self.cursor + ch.len_utf8()
    }

    fn prev_word_boundary(&self) -> usize {
        let before = &self.value[..self.cursor];
        let trimmed = before.trim_end();
        match trimmed.rfind(|c: char| c.is_whitespace()) {
            Some(i) => {
                let ws_char = trimmed[i..].chars().next().unwrap();
                i + ws_char.len_utf8()
            }
            None => 0,
        }
    }

    fn next_word_boundary(&self) -> usize {
        let rest = &self.value[self.cursor..];
        let mut chars = rest.char_indices().peekable();
        // Skip leading whitespace.
        while let Some(&(_, c)) = chars.peek() {
            if c.is_whitespace() {
                chars.next();
            } else {
                break;
            }
        }
        // Skip word characters.
        for (i, c) in chars {
            if c.is_whitespace() {
                return self.cursor + i;
            }
        }
        self.value.len()
    }

    pub fn move_left(&mut self) {
        self.selection_end = None;
        self.cursor = self.prev_char_boundary();
    }

    pub fn move_right(&mut self) {
        self.selection_end = None;
        self.cursor = self.next_char_boundary();
    }

    pub fn move_word_left(&mut self) {
        self.selection_end = None;
        self.cursor = self.prev_word_boundary();
    }

    pub fn move_word_right(&mut self) {
        self.selection_end = None;
        self.cursor = self.next_word_boundary();
    }

    pub fn move_start(&mut self) {
        self.selection_end = None;
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.selection_end = None;
        self.cursor = self.value.len();
    }

    /// Delete char before cursor (backspace).
    /// If a selection is active, deletes the selection range instead.
    pub fn backspace(&mut self) {
        if let Some(end) = self.selection_end.take() {
            self.value.drain(self.cursor..end);
        } else if self.cursor > 0 {
            let prev = self.prev_char_boundary();
            self.value.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    /// Delete char at cursor (forward delete).
    /// If a selection is active, deletes the selection range instead.
    pub fn delete_forward(&mut self) {
        if let Some(end) = self.selection_end.take() {
            self.value.drain(self.cursor..end);
        } else if self.cursor < self.value.len() {
            let next = self.next_char_boundary();
            self.value.drain(self.cursor..next);
        }
    }

    /// Delete word before cursor (ctrl+w).
    pub fn delete_word_before(&mut self) {
        self.selection_end = None;
        let start = self.prev_word_boundary();
        self.value.drain(start..self.cursor);
        self.cursor = start;
    }

    /// Delete word after cursor (alt+d).
    pub fn delete_word_after(&mut self) {
        self.selection_end = None;
        let end = self.next_word_boundary();
        self.value.drain(self.cursor..end);
    }

    /// Delete from cursor to start of line (ctrl+u).
    pub fn delete_to_start(&mut self) {
        self.selection_end = None;
        self.value.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Delete from cursor to end of line (ctrl+k).
    pub fn delete_to_end(&mut self) {
        self.selection_end = None;
        self.value.truncate(self.cursor);
    }

    /// Insert a char at the cursor.
    /// If a selection is active (selection_end is set), the selected range is
    /// deleted first so that typing replaces the selection.
    pub fn insert(&mut self, c: char) {
        if let Some(end) = self.selection_end.take() {
            // Delete the selected range cursor..end before inserting.
            self.value.drain(self.cursor..end);
        }
        self.value.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Handle a key event.  Returns `true` if the value was modified (caller
    /// may want to trigger re-filtering etc.), `false` if only cursor moved or
    /// key was not handled.
    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        let alt = modifiers.contains(KeyModifiers::ALT);
        match code {
            // -- deletion ----------------------------------------------------
            KeyCode::Backspace => {
                self.backspace();
                true
            }
            KeyCode::Char('h') if ctrl => {
                self.backspace();
                true
            }
            KeyCode::Char('w') if ctrl => {
                self.delete_word_before();
                true
            }
            KeyCode::Char('u') if ctrl => {
                self.delete_to_start();
                true
            }
            KeyCode::Char('k') if ctrl => {
                self.delete_to_end();
                true
            }
            KeyCode::Char('d') if ctrl => {
                self.delete_forward();
                true
            }
            KeyCode::Delete => {
                self.delete_forward();
                true
            }
            KeyCode::Char('d') if alt => {
                self.delete_word_after();
                true
            }
            // -- movement ----------------------------------------------------
            KeyCode::Char('a') if ctrl => {
                self.move_start();
                false
            }
            KeyCode::Char('e') if ctrl => {
                self.move_end();
                false
            }
            KeyCode::Char('f') if ctrl => {
                self.move_right();
                false
            }
            KeyCode::Char('b') if ctrl => {
                self.move_left();
                false
            }
            KeyCode::Left if ctrl => {
                self.move_word_left();
                false
            }
            KeyCode::Right if ctrl => {
                self.move_word_right();
                false
            }
            KeyCode::Left => {
                self.move_left();
                false
            }
            KeyCode::Right => {
                self.move_right();
                false
            }
            KeyCode::Home => {
                self.move_start();
                false
            }
            KeyCode::End => {
                self.move_end();
                false
            }
            // -- insert ------------------------------------------------------
            KeyCode::Char(c) if !ctrl && !alt => {
                self.insert(c);
                true
            }
            _ => false,
        }
    }
}

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;

use crate::issues::IssueArgs;
use crate::issues::list::Issue;
use crate::linear::types::IssueDetail;

pub enum Status {
    Idle,
    Loading,
    Error(String),
}

// ---------------------------------------------------------------------------
// Background sync events (bd-25j)
// ---------------------------------------------------------------------------

/// Events sent from the background sync thread to the TUI event loop.
pub enum SyncEvent {
    /// Sync completed successfully; includes the refreshed issue list.
    Done(Vec<Issue>),
    /// Sync encountered an error.
    Error(String),
    /// No auth token found -- sync was skipped.
    NotAuthenticated,
}

// ---------------------------------------------------------------------------
// Background comment sync events (bd-2mx)
// ---------------------------------------------------------------------------

/// Events sent from the background comment-sync thread to the TUI event loop.
pub enum CommentSyncEvent {
    /// Comments refreshed successfully from the Linear API.
    Done(Vec<crate::linear::types::Comment>),
    /// Comment sync error (non-fatal; cached data remains shown).
    Error(String),
}

// ---------------------------------------------------------------------------
// Background login events
// ---------------------------------------------------------------------------

/// Events sent from the background login thread to the TUI event loop.
pub enum LoginEvent {
    /// OAuth login completed successfully.
    Success,
    /// Login failed with an error message.
    Error(String),
}

// ---------------------------------------------------------------------------
// Popup support (bd-3dz)
// ---------------------------------------------------------------------------

/// Identifies which field a popup is editing.
#[derive(Clone)]
pub enum PopupKind {
    State,
    Priority,
    Assignee,
}

/// A single selectable item shown in the generic list-picker popup.
#[derive(Clone)]
pub struct PopupItem {
    /// Human-readable label.
    pub label: String,
    /// Opaque ID sent to the Linear API (state id, assignee id, etc.).
    /// None means "unassign" for the assignee popup.
    pub id: Option<String>,
}

/// Application mode -- only one active at a time.
pub enum Mode {
    /// Normal list browsing mode.
    List,
    /// Detail pane showing full issue content (bd-2g8).
    Detail,
    /// A generic list-picker popup is open (bd-3dz).
    Popup(PopupKind),
    /// New-issue modal form (bd-l6r).
    NewIssue,
    /// Searchable help popup (bd-5lz).
    Help,
    /// FTS incremental search overlay (bd-2g4).
    Search,
}

// ---------------------------------------------------------------------------
// Help popup state (bd-5lz)
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

/// Mutable state for the help popup.
pub struct HelpPopup {
    /// Current search query typed by the user.
    pub search: TextInput,
    /// Indices into ALL_KEYBINDINGS that match the current search.
    pub filtered: Vec<usize>,
    /// Currently highlighted row in the filtered list.
    pub selected: usize,
}

impl HelpPopup {
    pub fn new() -> Self {
        let filtered = (0..ALL_KEYBINDINGS.len()).collect();
        Self {
            search: TextInput::new(),
            filtered,
            selected: 0,
        }
    }

    pub fn update_filter(&mut self) {
        let q = self.search.value.to_lowercase();
        self.filtered = ALL_KEYBINDINGS
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                q.is_empty()
                    || e.key.to_lowercase().contains(&q)
                    || e.description.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }
}

// ---------------------------------------------------------------------------
// FTS search overlay state (bd-2g4)
// ---------------------------------------------------------------------------

/// Mutable state for the FTS search overlay.
pub struct SearchOverlay {
    /// Current query typed by the user.
    pub query: TextInput,
    /// Issues returned by the last FTS query.
    pub results: Vec<crate::issues::list::Issue>,
    /// Table selection state for the results list.
    pub table_state: TableState,
    /// When the query was last modified (used for 150ms debounce).
    pub last_changed: Option<Instant>,
    /// True when FTS index is unavailable (no sync yet).
    pub fts_unavailable: bool,
    /// True once run_search() has been called at least once (bd-zjy).
    /// Used by the renderer to distinguish "never searched" from "searched, no results".
    pub has_searched: bool,
    /// Parsed AST of the current query string (bd-3qb).
    pub ast: search_query::QueryAst,
    /// Tab-completion state (bd-3qb).
    pub completer: search_query::Completer,
}

impl SearchOverlay {
    pub fn new() -> Self {
        // Pre-populate the query bar with the default sort stem (bd-7qo).
        let default_q = search_query::DEFAULT_QUERY.to_string();
        let ast = search_query::parse_query_ast(&default_q);
        let query = TextInput::from_string(default_q);
        let mut completer = search_query::Completer::new();
        // Initialize completer so ghost text and Tab work immediately (bd-1dt).
        completer.update(&ast, query.cursor);
        Self {
            query,
            results: Vec::new(),
            table_state: TableState::default(),
            last_changed: None,
            fts_unavailable: false,
            has_searched: false,
            ast,
            completer,
        }
    }

    /// Run the structured search query and refresh results (bd-7qo).
    ///
    /// The query string is parsed into stems (sort:, assignee:, priority:,
    /// state:, team:) plus optional free-text FTS terms.  The default query
    /// is `sort:updated-` which shows all issues sorted by updated desc.
    ///
    /// `viewport_rows` is the number of visible rows in the content area
    /// (excluding the table header).  The result set is capped at this value
    /// so that the search overlay never grows taller than the normal list
    /// (bd-2qr).
    pub fn run_search(&mut self, viewport_rows: u16, list_limit: usize) {
        self.fts_unavailable = false;
        self.has_searched = true;
        let raw = self.query.value.trim().to_string();

        // An entirely blank query: show nothing (user cleared the bar).
        if raw.is_empty() {
            self.results.clear();
            self.table_state.select(None);
            return;
        }

        self.ast = search_query::parse_query_ast(&raw);
        let parsed = search_query::ParsedQuery::from(&self.ast);
        self.completer.update(&self.ast, self.query.cursor);

        // Use the same limit as the normal issue list so both views show the
        // same number of results.  Cap to viewport height so we never render
        // more rows than fit on screen.
        let limit = if viewport_rows > 0 {
            list_limit.min(viewport_rows as usize)
        } else {
            list_limit
        };
        match crate::db::open_db().and_then(|conn| search_query::run_query(&conn, &parsed, limit)) {
            Ok(db_issues) => {
                self.results = db_issues.into_iter().map(db_issue_to_list_issue).collect();
                if self.results.is_empty() {
                    self.table_state.select(None);
                } else {
                    self.table_state.select(Some(0));
                }
            }
            Err(e) => {
                // Only mark FTS as unavailable when the error is genuinely
                // about the FTS index or the issues table being missing (i.e.
                // no sync has been done yet).  A query-syntax error caused by
                // an incomplete stem token must NOT set fts_unavailable -- that
                // would show the misleading "run lt sync first" banner while
                // the user is still typing (bd-3q0).
                let msg = e.to_string().to_lowercase();
                let is_missing = msg.contains("issues_fts")
                    || msg.contains("no such table")
                    || msg.contains("could not open database");
                if is_missing {
                    self.fts_unavailable = true;
                }
                self.results.clear();
                self.table_state.select(None);
            }
        }
    }

    pub fn move_down(&mut self) {
        let n = self.results.len();
        if n == 0 {
            return;
        }
        let i = self.table_state.selected().unwrap_or(0);
        self.table_state.select(Some((i + 1).min(n - 1)));
    }

    pub fn move_up(&mut self) {
        let i = self.table_state.selected().unwrap_or(0);
        self.table_state.select(Some(i.saturating_sub(1)));
    }
}

// ---------------------------------------------------------------------------
// New-issue modal state (bd-l6r)
// ---------------------------------------------------------------------------

/// Which field of the new-issue form is currently focused.
#[derive(Clone, PartialEq)]
pub enum NewIssueField {
    Title,
    Team,
    Priority,
    State,
    Assignee,
    Description,
}

impl NewIssueField {
    pub fn next(&self) -> Self {
        match self {
            Self::Title => Self::Team,
            Self::Team => Self::Priority,
            Self::Priority => Self::State,
            Self::State => Self::Assignee,
            Self::Assignee => Self::Description,
            Self::Description => Self::Title,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            Self::Title => Self::Title,
            Self::Team => Self::Title,
            Self::Priority => Self::Team,
            Self::State => Self::Priority,
            Self::Assignee => Self::State,
            Self::Description => Self::Assignee,
        }
    }
}

// ---------------------------------------------------------------------------
// Events for modal background loading (bd-vfi)
// ---------------------------------------------------------------------------

/// Events sent from background threads that load modal picker data.
pub enum ModalEvent {
    /// States loaded for the selected team.
    StatesLoaded(Vec<PopupItem>),
    /// Assignees loaded for the selected team, plus an optional viewer ID.
    AssigneesLoaded(Vec<PopupItem>),
    /// Loading error.
    LoadError(String),
}

/// All mutable state for the new-issue modal form.
pub struct NewIssueModal {
    pub focused_field: NewIssueField,

    // Text fields
    pub title: TextInput,
    pub description: String,

    // Picker fields -- each holds a list of items + current selection index.
    pub teams: Vec<PopupItem>,
    pub team_selected: usize,

    pub priorities: Vec<PopupItem>,
    pub priority_selected: usize,

    pub states: Vec<PopupItem>,
    pub state_selected: usize,

    pub assignees: Vec<PopupItem>,
    pub assignee_selected: usize,

    /// True while we are waiting for picker data to load.
    pub loading: bool,
    /// Non-empty when a load or submit error occurred.
    pub error: String,

    /// Receiver for background-loaded modal data (bd-vfi).
    pub modal_rx: Option<mpsc::Receiver<ModalEvent>>,
}

pub struct App {
    pub issues: Vec<Issue>,
    pub table_state: TableState,
    pub args: IssueArgs,
    pub has_next_page: bool,
    // Pagination cursors.
    pub current_cursor: Option<String>,
    pub cursor_stack: Vec<Option<String>>,
    pub end_cursor: Option<String>,
    pub status: Status,
    pub quit: bool,
    // Filter overlay (input_mode mirrors Mode::InputFilter for compatibility).
    pub input_mode: bool,
    pub input_buf: String,
    // Set by ui::render each frame so key handlers know page size.
    pub viewport_height: u16,

    // -- mode -----------------------------------------------------------------
    pub mode: Mode,

    // -- detail pane (bd-2g8) -------------------------------------------------
    /// Loaded detail for the currently-open issue.
    pub detail: Option<IssueDetail>,
    /// Vertical scroll offset inside the detail pane (in lines).
    pub detail_scroll: u16,

    // -- popup state (bd-3dz) -------------------------------------------------
    pub popup_items: Vec<PopupItem>,
    pub popup_selected: usize,

    // -- footer message (bd-3dz) ----------------------------------------------
    pub footer_msg: Option<String>,

    // -- new-issue modal (bd-l6r) --------------------------------------------
    pub new_issue_modal: Option<NewIssueModal>,

    // -- background sync (bd-25j) --------------------------------------------
    /// Receiver for background sync events.
    pub sync_rx: Option<mpsc::Receiver<SyncEvent>>,
    /// True while a background sync thread is running.
    pub syncing: bool,
    /// Human-readable description of sync status, shown in footer.
    pub sync_status_label: String,

    // -- background comment sync (bd-2mx) ------------------------------------
    /// Receiver for background comment-sync events.
    pub detail_comment_rx: Option<mpsc::Receiver<CommentSyncEvent>>,

    // -- help popup (bd-5lz) -------------------------------------------------
    pub help_popup: Option<HelpPopup>,

    // -- FTS search overlay (bd-2g4) -------------------------------------------
    pub search_overlay: Option<SearchOverlay>,

    // -- popup anchor (bd-116) ------------------------------------------------
    /// Screen rect of the cell that triggered the popup, used to position it.
    pub popup_anchor: Option<ratatui::layout::Rect>,

    // -- active filter AST (bd-rbm) -------------------------------------------
    /// Single source of truth for the active filter/search state.
    /// Updated on Enter (confirm search), double-esc (reset), and sort shortcuts.
    pub active_filter: search_query::QueryAst,
    /// Snapshot of the filter at startup; used to reset on double-esc.
    pub initial_filter: search_query::QueryAst,

    // -- identity info (bd-185) -----------------------------------------------
    /// Authenticated user's display name.
    pub viewer_name: Option<String>,
    /// Linear organization (workspace) name.
    pub org_name: Option<String>,

    // -- double-esc reset (bd-1jt) --------------------------------------------
    /// The args as passed at startup; used to restore state on double-esc.
    pub initial_args: IssueArgs,
    /// Timestamp of the last Esc keypress (used to detect double-esc).
    pub last_esc_time: Option<Instant>,

    // -- re-auth (bd-vhp) -----------------------------------------------------
    /// Receiver for the background login thread, if one is in progress.
    pub login_rx: Option<mpsc::Receiver<LoginEvent>>,
    /// True when the last sync reported NotAuthenticated (no token stored).
    pub not_authenticated: bool,
}

impl App {
    fn new(
        issues: Vec<Issue>,
        has_next_page: bool,
        end_cursor: Option<String>,
        args: IssueArgs,
        sync_rx: Option<mpsc::Receiver<SyncEvent>>,
        syncing: bool,
        sync_status_label: String,
    ) -> Self {
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
            has_next_page,
            current_cursor: None,
            cursor_stack: Vec::new(),
            end_cursor,
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
            sync_rx,
            syncing,
            sync_status_label,
            detail_comment_rx: None,
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
            not_authenticated: false,
        }
    }

    /// Keep app.args.sort/desc in sync with active_filter (bd-rbm).
    /// Called after active_filter is updated so that do_fetch() and the
    /// table sort-column marker reflect the confirmed filter state.
    fn sync_args_from_filter(&mut self) {
        let parsed = search_query::ParsedQuery::from(&self.active_filter);
        if let Some((field, dir)) = parsed.sort {
            self.args.sort = field;
            self.args.desc = dir == search_query::SortDir::Desc;
        }
    }

    /// Produce a new QueryAst with the sort: token replaced to match
    /// self.args.sort/desc.  Used by cycle_sort and toggle_desc (bd-rbm).
    fn replace_sort_in_filter(&self) -> search_query::QueryAst {
        let dir = if self.args.desc { "-" } else { "+" };
        let new_sort = format!("sort:{}{}", self.args.sort.label(), dir);
        let mut parts: Vec<String> = self
            .active_filter
            .raw
            .split_whitespace()
            .filter(|t| !t.to_lowercase().starts_with("sort:"))
            .map(|s| s.to_string())
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
        let i = self.table_state.selected().unwrap_or(0) as i32;
        let new_i = (i + delta).clamp(0, n as i32 - 1) as usize;
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
        self.move_by(self.viewport_height as i32);
    }
    fn page_up(&mut self) {
        self.move_by(-(self.viewport_height as i32));
    }
    fn half_page_down(&mut self) {
        self.move_by(self.viewport_height as i32 / 2);
    }
    fn half_page_up(&mut self) {
        self.move_by(-(self.viewport_height as i32 / 2));
    }

    fn do_fetch(&mut self, reset_selection: bool) {
        self.status = Status::Loading;
        let mut parsed = search_query::ParsedQuery::from(&self.active_filter);
        // Resolve "me" to actual viewer name for assignee filter.
        search_query::resolve_me(&mut parsed, self.viewer_name.as_deref());

        if parsed.has_filters() {
            // Active filter has constraints beyond sort -- use run_query to
            // preserve them (bd-2i0).
            let limit = self.args.limit.min(250) as usize;
            match crate::db::open_db()
                .and_then(|conn| search_query::run_query(&conn, &parsed, limit))
            {
                Ok(db_issues) => {
                    self.issues = db_issues.into_iter().map(db_issue_to_list_issue).collect();
                    self.has_next_page = false; // run_query has no pagination
                    self.end_cursor = None;
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
                Err(e) => {
                    self.status = Status::Error(e.to_string());
                }
            }
        } else {
            // No active filters -- use paginated query as before.
            let offset: i64 = self
                .current_cursor
                .as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            match crate::db::open_db()
                .and_then(|conn| crate::db::query_issues_page(&conn, &self.args, offset))
            {
                Ok((issues, has_next_page)) => {
                    self.issues = issues.into_iter().map(db_issue_to_list_issue).collect();
                    self.has_next_page = has_next_page;
                    let limit = self.args.limit.min(250) as i64;
                    self.end_cursor = if has_next_page {
                        Some((offset + limit).to_string())
                    } else {
                        None
                    };
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
                Err(e) => {
                    self.status = Status::Error(e.to_string());
                }
            }
        }
    }

    /// Fetch and then seek to the newly created issue by identifier (bd-3ba).
    fn do_fetch_and_select(&mut self, target_identifier: Option<String>) {
        self.do_fetch(true);
        if let Some(id) = target_identifier
            && let Some(idx) = self.issues.iter().position(|i| i.identifier == id)
        {
            self.table_state.select(Some(idx));
        }
    }

    fn refresh(&mut self) {
        self.do_fetch(false);
    }

    fn cycle_sort(&mut self) {
        self.args.sort = self.args.sort.next();
        self.active_filter = self.replace_sort_in_filter();
        self.cursor_stack.clear();
        self.current_cursor = None;
        self.do_fetch(true);
    }

    fn toggle_desc(&mut self) {
        self.args.desc = !self.args.desc;
        self.active_filter = self.replace_sort_in_filter();
        self.cursor_stack.clear();
        self.current_cursor = None;
        self.do_fetch(true);
    }

    fn next_page(&mut self) {
        if !self.has_next_page {
            return;
        }
        let end = self.end_cursor.clone();
        self.cursor_stack.push(self.current_cursor.clone());
        self.current_cursor = end;
        self.do_fetch(true);
    }

    fn prev_page(&mut self) {
        if self.cursor_stack.is_empty() {
            return;
        }
        self.current_cursor = self.cursor_stack.pop().unwrap();
        self.do_fetch(true);
    }

    // -- Detail pane (bd-2g8) -------------------------------------------------

    /// Open the detail pane for the currently selected issue.
    ///
    /// The detail is populated instantly from the local SQLite cache so the
    /// pane appears without any network round-trip.  A background thread then
    /// calls sync_comments via the Linear API and sends the refreshed comment
    /// list back through `detail_comment_rx` (bd-2mx).
    fn open_detail(&mut self) {
        let issue = match self.selected_issue() {
            Some(i) => i.clone(),
            None => return,
        };

        self.mode = Mode::Detail;
        self.detail_scroll = 0;
        self.detail_comment_rx = None;

        // Build an IssueDetail immediately from cached data.
        let cached_comments: Vec<crate::linear::types::Comment> = crate::db::open_db()
            .and_then(|conn| crate::db::query_comments(&conn, &issue.id))
            .unwrap_or_default()
            .into_iter()
            .map(|c| crate::linear::types::Comment {
                body: c.body,
                created_at: c.created_at,
                user: c
                    .author_name
                    .map(|n| crate::linear::types::CommentUser { name: n }),
            })
            .collect();

        self.detail = Some(crate::linear::types::IssueDetail {
            identifier: issue.identifier.clone(),
            title: issue.title.clone(),
            description: issue.description.clone(),
            priority_label: issue.priority_label.clone(),
            state: crate::linear::types::IssueDetailState {
                name: issue.state.name.clone(),
            },
            assignee: issue
                .assignee
                .as_ref()
                .map(|a| crate::linear::types::IssueDetailUser {
                    name: a.name.clone(),
                }),
            team: crate::linear::types::IssueDetailTeam {
                name: issue.team.name.clone(),
            },
            labels: crate::linear::types::LabelConnection {
                nodes: issue
                    .labels
                    .nodes
                    .iter()
                    .map(|l| crate::linear::types::Label {
                        name: l.name.clone(),
                    })
                    .collect(),
            },
            created_at: issue.created_at.clone(),
            updated_at: issue.updated_at.clone(),
            comments: crate::linear::types::CommentConnection {
                nodes: cached_comments,
            },
        });
        self.status = Status::Idle;

        // Spawn background thread to refresh comments from the Linear API.
        let issue_id = issue.id.clone();
        let (tx, rx) = std::sync::mpsc::channel::<CommentSyncEvent>();
        self.detail_comment_rx = Some(rx);

        std::thread::spawn(move || {
            let token = match crate::config::load_token() {
                Ok(Some(t)) => t,
                _ => {
                    let _ = tx.send(CommentSyncEvent::Error("not logged in".to_string()));
                    return;
                }
            };
            let conn = match crate::db::open_db() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(CommentSyncEvent::Error(e.to_string()));
                    return;
                }
            };
            match crate::sync::comments::sync_comments(&conn, &token.access_token, &issue_id) {
                Ok(()) => {
                    // Read the freshly-synced comments back from the DB.
                    let fresh = crate::db::query_comments(&conn, &issue_id)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|c| crate::linear::types::Comment {
                            body: c.body,
                            created_at: c.created_at,
                            user: c
                                .author_name
                                .map(|n| crate::linear::types::CommentUser { name: n }),
                        })
                        .collect();
                    let _ = tx.send(CommentSyncEvent::Done(fresh));
                }
                Err(e) => {
                    let _ = tx.send(CommentSyncEvent::Error(e.to_string()));
                }
            }
        });
    }

    /// Close the detail pane and return to the list.
    fn close_detail(&mut self) {
        self.mode = Mode::List;
        self.detail = None;
        self.detail_scroll = 0;
        self.status = Status::Idle;
        // Drop the background comment-sync receiver so the thread stops being
        // polled and will be GC'd once it finishes its network request.
        self.detail_comment_rx = None;
    }

    fn detail_scroll_down(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_add(1);
    }

    fn detail_scroll_up(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_sub(1);
    }

    fn detail_scroll_to_top(&mut self) {
        self.detail_scroll = 0;
    }

    fn detail_scroll_to_bottom(&mut self) {
        // Ratatui clamps scroll to content length; use a large sentinel.
        self.detail_scroll = u16::MAX;
    }

    fn detail_scroll_half_page_down(&mut self) {
        let step = (self.viewport_height / 2).max(1);
        self.detail_scroll = self.detail_scroll.saturating_add(step);
    }

    fn detail_scroll_half_page_up(&mut self) {
        let step = (self.viewport_height / 2).max(1);
        self.detail_scroll = self.detail_scroll.saturating_sub(step);
    }

    fn detail_scroll_page_down(&mut self) {
        let step = self.viewport_height.max(1);
        self.detail_scroll = self.detail_scroll.saturating_add(step);
    }

    fn detail_scroll_page_up(&mut self) {
        let step = self.viewport_height.max(1);
        self.detail_scroll = self.detail_scroll.saturating_sub(step);
    }

    // -- Popup helpers (bd-3dz) -----------------------------------------------

    fn open_state_popup(&mut self) {
        let issue = match self.selected_issue() {
            Some(i) => i.clone(),
            None => return,
        };
        let token = match crate::config::load_token() {
            Ok(Some(t)) => t,
            _ => {
                self.footer_msg = Some("Not logged in".to_string());
                return;
            }
        };
        let current_state_name = issue.state.name.clone();
        match crate::linear::mutations::fetch_workflow_states(&token.access_token, &issue.team.id) {
            Ok(states) => {
                self.popup_items = states
                    .into_iter()
                    .map(|s| PopupItem {
                        label: s.name,
                        id: Some(s.id),
                    })
                    .collect();
                self.popup_selected = self
                    .popup_items
                    .iter()
                    .position(|item| item.label == current_state_name)
                    .unwrap_or(0);
                self.mode = Mode::Popup(PopupKind::State);
                self.footer_msg = None;
            }
            Err(e) => {
                self.footer_msg = Some(format!("Failed to fetch states: {}", e));
            }
        }
    }

    fn open_priority_popup(&mut self) {
        if self.selected_issue().is_none() {
            return;
        }
        let priority = self.selected_issue().unwrap().priority;
        // Linear priority: 0=No priority, 1=Urgent, 2=High, 3=Normal, 4=Low
        self.popup_items = vec![
            PopupItem {
                label: "No priority".to_string(),
                id: Some("0".to_string()),
            },
            PopupItem {
                label: "Urgent".to_string(),
                id: Some("1".to_string()),
            },
            PopupItem {
                label: "High".to_string(),
                id: Some("2".to_string()),
            },
            PopupItem {
                label: "Normal".to_string(),
                id: Some("3".to_string()),
            },
            PopupItem {
                label: "Low".to_string(),
                id: Some("4".to_string()),
            },
        ];
        self.popup_selected = priority as usize;
        self.mode = Mode::Popup(PopupKind::Priority);
        self.footer_msg = None;
    }

    fn open_assignee_popup(&mut self) {
        let issue = match self.selected_issue() {
            Some(i) => i.clone(),
            None => return,
        };
        let token = match crate::config::load_token() {
            Ok(Some(t)) => t,
            _ => {
                self.footer_msg = Some("Not logged in".to_string());
                return;
            }
        };
        let mut items: Vec<PopupItem> = vec![PopupItem {
            label: "Unassign".to_string(),
            id: None,
        }];
        match fetch_team_members(&token.access_token, &issue.team.id) {
            Ok(members) => {
                for m in members {
                    items.push(PopupItem {
                        label: m.name,
                        id: Some(m.id),
                    });
                }
            }
            Err(e) => {
                self.footer_msg = Some(format!("Failed to fetch members: {}", e));
                return;
            }
        }
        self.popup_selected = issue
            .assignee
            .as_ref()
            .and_then(|a| {
                items
                    .iter()
                    .position(|item| item.id.as_deref() == Some(a.id.as_str()))
            })
            .unwrap_or(0);
        self.popup_items = items;
        self.mode = Mode::Popup(PopupKind::Assignee);
        self.footer_msg = None;
    }

    fn popup_move(&mut self, delta: i32) {
        let n = self.popup_items.len();
        if n == 0 {
            return;
        }
        let i = self.popup_selected as i32;
        self.popup_selected = (i + delta).clamp(0, n as i32 - 1) as usize;
    }

    fn popup_confirm(&mut self) {
        let kind = match &self.mode {
            Mode::Popup(k) => k.clone(),
            _ => return,
        };
        let item = match self.popup_items.get(self.popup_selected) {
            Some(i) => i.clone(),
            None => return,
        };
        let issue = match self.selected_issue() {
            Some(i) => i.clone(),
            None => return,
        };

        // 1. Optimistic SQLite update.
        optimistic_update_sqlite(&issue, &kind, &item);

        // 2. Update in-memory issue list for instant feedback.
        apply_optimistic_in_memory(self, &kind, &item);

        // 3. Fire mutation in background thread.
        let issue_id: String = issue.id.clone();
        let kind2: PopupKind = kind.clone();
        let item2: PopupItem = item.clone();
        let orig_issue: crate::issues::list::Issue = issue.clone();

        std::thread::spawn(move || {
            let token = match crate::config::load_token() {
                Ok(Some(t)) => t,
                _ => return,
            };
            let result: anyhow::Result<()> = match kind2 {
                PopupKind::State => {
                    if let Some(state_id) = &item2.id {
                        crate::linear::mutations::update_issue_state(
                            &token.access_token,
                            &issue_id,
                            state_id,
                        )
                        .map(|_| ())
                    } else {
                        Ok(())
                    }
                }
                PopupKind::Priority => {
                    if let Some(pstr) = &item2.id {
                        let p: u8 = pstr.parse().unwrap_or(0);
                        crate::linear::mutations::update_issue_priority(
                            &token.access_token,
                            &issue_id,
                            p,
                        )
                        .map(|_| ())
                    } else {
                        Ok(())
                    }
                }
                PopupKind::Assignee => crate::linear::mutations::update_issue_assignee(
                    &token.access_token,
                    &issue_id,
                    item2.id.clone(),
                )
                .map(|_| ()),
            };
            if let Err(_e) = result {
                // On failure: revert SQLite to the original values.
                revert_sqlite(&orig_issue, &kind2);
            }
        });

        self.mode = Mode::List;
        self.popup_anchor = None;
    }

    fn popup_cancel(&mut self) {
        self.mode = Mode::List;
        self.popup_anchor = None;
    }

    // -- New-issue modal (bd-l6r) --------------------------------------------

    fn open_new_issue_modal(&mut self) {
        let token = match crate::config::load_token() {
            Ok(Some(t)) => t,
            _ => {
                self.footer_msg = Some("Not logged in".to_string());
                return;
            }
        };

        // Pre-fill team from active filter if set.
        let preset_team = self.args.team.clone();

        let mut modal = NewIssueModal {
            focused_field: NewIssueField::Title,
            title: TextInput::new(),
            description: String::new(),
            teams: Vec::new(),
            team_selected: 0,
            priorities: vec![
                PopupItem {
                    label: "No priority".to_string(),
                    id: Some("0".to_string()),
                },
                PopupItem {
                    label: "Urgent".to_string(),
                    id: Some("1".to_string()),
                },
                PopupItem {
                    label: "High".to_string(),
                    id: Some("2".to_string()),
                },
                PopupItem {
                    label: "Normal".to_string(),
                    id: Some("3".to_string()),
                },
                PopupItem {
                    label: "Low".to_string(),
                    id: Some("4".to_string()),
                },
            ],
            priority_selected: 0,
            states: Vec::new(),
            state_selected: 0,
            assignees: Vec::new(),
            assignee_selected: 0,
            loading: true,
            error: String::new(),
            modal_rx: None,
        };

        // Fetch teams synchronously (fast -- just a list).
        match crate::linear::mutations::fetch_teams(&token.access_token) {
            Ok(teams) => {
                modal.teams = teams
                    .into_iter()
                    .map(|t| PopupItem {
                        label: t.name.clone(),
                        id: Some(t.id),
                    })
                    .collect();
                // Pre-select team from filter.
                if let Some(ref preset) = preset_team
                    && let Some(idx) = modal
                        .teams
                        .iter()
                        .position(|t| t.label.to_lowercase().contains(&preset.to_lowercase()))
                {
                    modal.team_selected = idx;
                }
                modal.loading = false;
            }
            Err(e) => {
                modal.error = format!("Failed to fetch teams: {}", e);
                modal.loading = false;
            }
        }

        self.mode = Mode::NewIssue;
        self.new_issue_modal = Some(modal);
    }

    /// Kick off background loading of states and assignees for the selected team (bd-vfi).
    fn new_issue_load_states_and_assignees_bg(&mut self) {
        let modal = match self.new_issue_modal.as_mut() {
            Some(m) => m,
            None => return,
        };
        let team_id = match modal
            .teams
            .get(modal.team_selected)
            .and_then(|t| t.id.clone())
        {
            Some(id) => id,
            None => return,
        };

        modal.loading = true;
        modal.error.clear();

        let (tx, rx) = mpsc::channel::<ModalEvent>();
        modal.modal_rx = Some(rx);

        std::thread::spawn(move || {
            let token = match crate::config::load_token() {
                Ok(Some(t)) => t,
                _ => {
                    let _ = tx.send(ModalEvent::LoadError("Not logged in".to_string()));
                    return;
                }
            };

            // Fetch viewer for "me" shortcut (bd-1fz).
            let viewer = fetch_viewer(&token.access_token).ok();

            // Fetch states.
            match crate::linear::mutations::fetch_workflow_states(&token.access_token, &team_id) {
                Ok(states) => {
                    let items: Vec<PopupItem> = states
                        .into_iter()
                        .map(|s| PopupItem {
                            label: s.name,
                            id: Some(s.id),
                        })
                        .collect();
                    let _ = tx.send(ModalEvent::StatesLoaded(items));
                }
                Err(e) => {
                    let _ = tx.send(ModalEvent::LoadError(format!(
                        "Failed to fetch states: {}",
                        e
                    )));
                    return;
                }
            }

            // Fetch assignees.
            match fetch_team_members(&token.access_token, &team_id) {
                Ok(members) => {
                    // Build the assignees list: "Me (name)" at top if viewer is known,
                    // then "Unassigned", then team members.
                    let mut items: Vec<PopupItem> = Vec::new();
                    if let Some(ref v) = viewer {
                        items.push(PopupItem {
                            label: format!("Me ({})", v.name),
                            id: Some(v.id.clone()),
                        });
                    }
                    items.push(PopupItem {
                        label: "Unassigned".to_string(),
                        id: None,
                    });
                    for m in members {
                        // Skip the viewer entry since it is already at the top.
                        if viewer.as_ref().map(|v| v.id == m.id).unwrap_or(false) {
                            continue;
                        }
                        items.push(PopupItem {
                            label: m.name,
                            id: Some(m.id),
                        });
                    }
                    let _ = tx.send(ModalEvent::AssigneesLoaded(items));
                }
                Err(e) => {
                    let _ = tx.send(ModalEvent::LoadError(format!(
                        "Failed to fetch assignees: {}",
                        e
                    )));
                }
            }
        });
    }

    fn new_issue_submit(&mut self) {
        let token = match crate::config::load_token() {
            Ok(Some(t)) => t,
            _ => {
                if let Some(m) = self.new_issue_modal.as_mut() {
                    m.error = "Not logged in".to_string();
                }
                return;
            }
        };

        let modal = match self.new_issue_modal.as_ref() {
            Some(m) => m,
            None => return,
        };

        if modal.title.value.trim().is_empty() {
            if let Some(m) = self.new_issue_modal.as_mut() {
                m.error = "Title is required".to_string();
                m.focused_field = NewIssueField::Title;
            }
            return;
        }

        let team_id = match modal
            .teams
            .get(modal.team_selected)
            .and_then(|t| t.id.clone())
        {
            Some(id) => id,
            None => {
                if let Some(m) = self.new_issue_modal.as_mut() {
                    m.error = "Select a team".to_string();
                }
                return;
            }
        };

        let input = crate::linear::mutations::CreateIssueInput {
            title: modal.title.value.trim().to_string(),
            team_id: team_id.clone(),
            description: if modal.description.trim().is_empty() {
                None
            } else {
                Some(modal.description.trim().to_string())
            },
            state_id: modal
                .states
                .get(modal.state_selected)
                .and_then(|s| s.id.clone()),
            priority: modal
                .priorities
                .get(modal.priority_selected)
                .and_then(|p| p.id.as_ref())
                .and_then(|s| s.parse::<u8>().ok()),
            assignee_id: modal
                .assignees
                .get(modal.assignee_selected)
                .and_then(|a| a.id.clone()),
        };

        let title_for_db = input.title.clone();
        let team_name = modal
            .teams
            .get(modal.team_selected)
            .map(|t| t.label.clone())
            .unwrap_or_default();
        let state_name = modal
            .states
            .get(modal.state_selected)
            .map(|s| s.label.clone())
            .unwrap_or_else(|| "Backlog".to_string());
        let priority_label = modal
            .priorities
            .get(modal.priority_selected)
            .map(|p| p.label.clone())
            .unwrap_or_else(|| "No priority".to_string());
        let assignee_name = modal.assignees.get(modal.assignee_selected).and_then(|a| {
            if a.id.is_some() {
                Some(a.label.clone())
            } else {
                None
            }
        });

        match crate::linear::mutations::create_issue(&token.access_token, input) {
            Ok(created) => {
                // Optimistically insert into SQLite.
                let now = chrono::Utc::now().to_rfc3339();
                let db_issue = crate::db::Issue {
                    id: created.id.clone(),
                    identifier: created.identifier.clone(),
                    title: title_for_db,
                    priority_label,
                    state_name,
                    assignee_name,
                    team_name,
                    team_key: Some(team_id),
                    created_at: now.clone(),
                    updated_at: now,
                    synced_at: chrono::Utc::now().to_rfc3339(),
                    description: None,
                    labels: String::new(),
                    project_name: None,
                    cycle_name: None,
                    creator_name: None,
                };
                if let Ok(conn) = crate::db::open_db() {
                    let _ = crate::db::upsert_issues(&conn, &[db_issue]);
                }
                // Refresh list and highlight new issue (bd-3ba).
                let new_identifier = created.identifier.clone();
                self.mode = Mode::List;
                self.new_issue_modal = None;
                self.footer_msg = Some(format!("Created {}", created.identifier));
                self.do_fetch_and_select(Some(new_identifier));
            }
            Err(e) => {
                if let Some(m) = self.new_issue_modal.as_mut() {
                    m.error = format!("Failed to create issue: {}", e);
                }
            }
        }
    }

    /// Poll modal background channel and update modal state (bd-vfi).
    fn poll_modal_events(&mut self) {
        // Collect events before mutating -- avoids borrow issues.
        let events: Vec<ModalEvent> = {
            let modal = match self.new_issue_modal.as_ref() {
                Some(m) => m,
                None => return,
            };
            let rx = match modal.modal_rx.as_ref() {
                Some(r) => r,
                None => return,
            };
            let mut evts = Vec::new();
            loop {
                match rx.try_recv() {
                    Ok(ev) => evts.push(ev),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => break,
                }
            }
            evts
        };

        for ev in events {
            let modal = match self.new_issue_modal.as_mut() {
                Some(m) => m,
                None => break,
            };
            match ev {
                ModalEvent::StatesLoaded(items) => {
                    modal.states = items;
                    modal.state_selected = 0;
                }
                ModalEvent::AssigneesLoaded(items) => {
                    modal.assignees = items;
                    modal.assignee_selected = 0;
                    modal.loading = false;
                }
                ModalEvent::LoadError(msg) => {
                    modal.error = msg;
                    modal.loading = false;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Viewer query helper (bd-1fz)
// ---------------------------------------------------------------------------

struct ViewerInfo {
    pub id: String,
    pub name: String,
    pub org_name: String,
}

fn fetch_viewer(token: &str) -> Result<ViewerInfo> {
    use serde::Deserialize;
    use serde_json::json;

    const VIEWER_QUERY: &str = r#"
query Viewer {
  viewer {
    id
    name
    organization {
      name
    }
  }
}
"#;

    #[derive(Deserialize)]
    struct OrgNode {
        name: String,
    }
    #[derive(Deserialize)]
    struct ViewerNode {
        id: String,
        name: String,
        organization: OrgNode,
    }
    #[derive(Deserialize)]
    struct ViewerData {
        viewer: ViewerNode,
    }

    let data: ViewerData = crate::linear::client::graphql_query(token, VIEWER_QUERY, json!({}))?;
    Ok(ViewerInfo {
        id: data.viewer.id,
        name: data.viewer.name,
        org_name: data.viewer.organization.name,
    })
}

// ---------------------------------------------------------------------------
// Team member fetch (used by assignee popup)
// ---------------------------------------------------------------------------

struct Member {
    pub id: String,
    pub name: String,
}

fn fetch_team_members(token: &str, team_id: &str) -> Result<Vec<Member>> {
    use serde::Deserialize;
    use serde_json::json;

    const TEAM_MEMBERS_QUERY: &str = r#"
query TeamMembers($teamId: String!) {
  team(id: $teamId) {
    members {
      nodes {
        id
        name
      }
    }
  }
}
"#;

    #[derive(Deserialize)]
    struct MemberNode {
        id: String,
        name: String,
    }
    #[derive(Deserialize)]
    struct MemberConnection {
        nodes: Vec<MemberNode>,
    }
    #[derive(Deserialize)]
    struct TeamData {
        members: MemberConnection,
    }
    #[derive(Deserialize)]
    struct TeamWrapper {
        team: TeamData,
    }

    let variables = json!({ "teamId": team_id });
    let data: TeamWrapper =
        crate::linear::client::graphql_query(token, TEAM_MEMBERS_QUERY, variables)?;
    Ok(data
        .team
        .members
        .nodes
        .into_iter()
        .map(|m| Member {
            id: m.id,
            name: m.name,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Optimistic SQLite helpers (bd-3dz)
// ---------------------------------------------------------------------------

fn optimistic_update_sqlite(
    issue: &crate::issues::list::Issue,
    kind: &PopupKind,
    item: &PopupItem,
) {
    let conn = match crate::db::open_db() {
        Ok(c) => c,
        Err(_) => return,
    };
    let db_issue = build_db_issue_optimistic(issue, kind, item);
    let _ = crate::db::upsert_issues(&conn, &[db_issue]);
}

fn revert_sqlite(orig: &crate::issues::list::Issue, _kind: &PopupKind) {
    let conn = match crate::db::open_db() {
        Ok(c) => c,
        Err(_) => return,
    };
    let db_issue = crate::db::Issue {
        id: orig.id.clone(),
        identifier: orig.identifier.clone(),
        title: orig.title.clone(),
        priority_label: orig.priority_label.clone(),
        state_name: orig.state.name.clone(),
        assignee_name: orig.assignee.as_ref().map(|a| a.name.clone()),
        team_name: orig.team.name.clone(),
        team_key: Some(orig.team.id.clone()),
        created_at: orig.created_at.clone(),
        updated_at: orig.updated_at.clone(),
        synced_at: chrono::Utc::now().to_rfc3339(),
        description: orig.description.clone(),
        labels: orig
            .labels
            .nodes
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(","),
        project_name: orig.project.as_ref().map(|p| p.name.clone()),
        cycle_name: orig.cycle.as_ref().map(|c| c.name.clone()),
        creator_name: orig.creator.as_ref().map(|u| u.name.clone()),
    };
    let _ = crate::db::upsert_issues(&conn, &[db_issue]);
}

fn build_db_issue_optimistic(
    issue: &crate::issues::list::Issue,
    kind: &PopupKind,
    item: &PopupItem,
) -> crate::db::Issue {
    let priority_label = match kind {
        PopupKind::Priority => item.label.clone(),
        _ => issue.priority_label.clone(),
    };
    let state_name = match kind {
        PopupKind::State => item.label.clone(),
        _ => issue.state.name.clone(),
    };
    let assignee_name = match kind {
        PopupKind::Assignee => {
            if item.id.is_none() {
                None
            } else {
                Some(item.label.clone())
            }
        }
        _ => issue.assignee.as_ref().map(|a| a.name.clone()),
    };
    crate::db::Issue {
        id: issue.id.clone(),
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        priority_label,
        state_name,
        assignee_name,
        team_name: issue.team.name.clone(),
        team_key: Some(issue.team.id.clone()),
        created_at: issue.created_at.clone(),
        updated_at: issue.updated_at.clone(),
        synced_at: chrono::Utc::now().to_rfc3339(),
        description: issue.description.clone(),
        labels: issue
            .labels
            .nodes
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(","),
        project_name: issue.project.as_ref().map(|p| p.name.clone()),
        cycle_name: issue.cycle.as_ref().map(|c| c.name.clone()),
        creator_name: issue.creator.as_ref().map(|u| u.name.clone()),
    }
}

fn apply_optimistic_in_memory(app: &mut App, kind: &PopupKind, item: &PopupItem) {
    let issue = match app.selected_issue_mut() {
        Some(i) => i,
        None => return,
    };
    match kind {
        PopupKind::State => {
            issue.state.name = item.label.clone();
            if let Some(id) = &item.id {
                issue.state.id = id.clone();
            }
        }
        PopupKind::Priority => {
            issue.priority_label = item.label.clone();
            if let Some(pstr) = &item.id {
                issue.priority = pstr.parse().unwrap_or(issue.priority);
            }
        }
        PopupKind::Assignee => {
            if item.id.is_none() {
                issue.assignee = None;
            } else {
                issue.assignee = Some(crate::issues::list::User {
                    id: item.id.clone().unwrap_or_default(),
                    name: item.label.clone(),
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Sync status helpers (bd-25j)
// ---------------------------------------------------------------------------

/// Build a human-readable "synced X min ago" or "syncing..." label.
fn build_sync_status_label(syncing: bool) -> String {
    if syncing {
        return "syncing...".to_string();
    }
    // Read last_synced_at from DB.
    let last = (|| -> Option<String> {
        let conn = crate::db::open_db().ok()?;
        crate::db::get_meta(&conn, "last_synced_at").ok()?
    })();

    match last {
        None => "not synced".to_string(),
        Some(ts) => {
            // Parse RFC3339 and compute elapsed minutes.
            match chrono::DateTime::parse_from_rfc3339(&ts) {
                Ok(dt) => {
                    let elapsed =
                        chrono::Utc::now().signed_duration_since(dt.with_timezone(&chrono::Utc));
                    let mins = elapsed.num_minutes();
                    if mins < 1 {
                        "synced just now".to_string()
                    } else if mins == 1 {
                        "synced 1 min ago".to_string()
                    } else {
                        format!("synced {} min ago", mins)
                    }
                }
                Err(_) => "synced".to_string(),
            }
        }
    }
}

/// Spawn the background delta sync thread and return the receiver (bd-25j).
fn spawn_sync_thread(args: IssueArgs) -> mpsc::Receiver<SyncEvent> {
    let (tx, rx) = mpsc::channel::<SyncEvent>();
    std::thread::spawn(move || {
        // Skip sync when no auth token is stored; notify the TUI.
        match crate::config::load_token() {
            Ok(None) => {
                let _ = tx.send(SyncEvent::NotAuthenticated);
                return;
            }
            Err(_) => {
                let _ = tx.send(SyncEvent::NotAuthenticated);
                return;
            }
            Ok(Some(_)) => {}
        }

        // Run delta sync (falls back to full if no prior sync).
        match crate::sync::delta::run() {
            Ok(()) => {
                // Re-query SQLite for a fresh issue list to send to TUI.
                let issues = (|| -> Result<Vec<Issue>> {
                    let conn = crate::db::open_db()?;
                    let db_issues = crate::db::query_issues(&conn, &args)?;
                    // Convert db::Issue -> issues::list::Issue.
                    Ok(db_issues.into_iter().map(db_issue_to_list_issue).collect())
                })();
                match issues {
                    Ok(list) => {
                        let _ = tx.send(SyncEvent::Done(list));
                    }
                    Err(e) => {
                        let _ = tx.send(SyncEvent::Error(e.to_string()));
                    }
                }
            }
            Err(e) => {
                // Surface only the outermost error message to keep the
                // statusbar readable (the anyhow chain can be very long).
                let msg = e.to_string();
                let brief = msg.lines().next().unwrap_or(&msg).to_string();
                let _ = tx.send(SyncEvent::Error(brief));
            }
        }
    });
    rx
}

/// Spawn a background thread that runs the non-interactive OAuth login flow.
fn spawn_login_thread() -> mpsc::Receiver<LoginEvent> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || match crate::auth::login_non_interactive() {
        Ok(()) => {
            let _ = tx.send(LoginEvent::Success);
        }
        Err(e) => {
            let _ = tx.send(LoginEvent::Error(e.to_string()));
        }
    });
    rx
}

/// Poll the background login channel and update app state on completion.
fn poll_login_events(app: &mut App) {
    let rx = match app.login_rx.as_ref() {
        Some(rx) => rx,
        None => return,
    };
    match rx.try_recv() {
        Ok(LoginEvent::Success) => {
            app.login_rx = None;
            // Refresh viewer identity after successful login.
            if let Ok(Some(token)) = crate::config::load_token()
                && let Ok(viewer) = fetch_viewer(&token.access_token)
            {
                app.viewer_name = Some(viewer.name);
                app.org_name = Some(viewer.org_name);
            }
            app.not_authenticated = false;
            app.syncing = true;
            app.sync_status_label = build_sync_status_label(true);
            app.sync_rx = Some(spawn_sync_thread(app.args.clone()));
        }
        Ok(LoginEvent::Error(msg)) => {
            app.login_rx = None;
            app.footer_msg = Some(format!("Login failed: {}", msg));
            app.sync_status_label = "not authenticated -- press L to log in".to_string();
        }
        Err(mpsc::TryRecvError::Empty) => {} // still waiting
        Err(mpsc::TryRecvError::Disconnected) => {
            app.login_rx = None;
        }
    }
}

/// Convert a `crate::db::Issue` row to a `crate::issues::list::Issue` for TUI display.
fn db_issue_to_list_issue(src: crate::db::Issue) -> Issue {
    Issue {
        id: src.id,
        identifier: src.identifier,
        title: src.title,
        priority_label: src.priority_label.clone(),
        priority: priority_label_to_u8(&src.priority_label),
        state: crate::issues::list::State {
            id: String::new(),
            name: src.state_name,
        },
        assignee: src.assignee_name.map(|n| crate::issues::list::User {
            id: String::new(),
            name: n,
        }),
        team: crate::issues::list::Team {
            id: src.team_key.unwrap_or_default(),
            name: src.team_name,
        },
        created_at: src.created_at,
        updated_at: src.updated_at,
        description: src.description,
        labels: crate::issues::list::LabelConnection {
            nodes: src
                .labels
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|n| crate::issues::list::LabelNode {
                    name: n.to_string(),
                })
                .collect(),
        },
        project: src.project_name.map(|n| crate::issues::list::Project {
            id: String::new(),
            name: n,
        }),
        cycle: src.cycle_name.map(|n| crate::issues::list::Cycle {
            id: String::new(),
            name: n,
        }),
        creator: src.creator_name.map(|n| crate::issues::list::User {
            id: String::new(),
            name: n,
        }),
    }
}

fn priority_label_to_u8(label: &str) -> u8 {
    match label.to_lowercase().as_str() {
        "urgent" => 1,
        "high" => 2,
        "normal" | "medium" => 3,
        "low" => 4,
        _ => 0,
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
            let limit = args.limit.min(250) as i64;
            let (db_issues, has_next) = crate::db::query_issues_page(&conn, &args, 0)?;
            let end_cursor = if has_next {
                Some(limit.to_string())
            } else {
                None
            };
            let issues = db_issues.into_iter().map(db_issue_to_list_issue).collect();
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

    let sync_status_label = build_sync_status_label(syncing);

    // Spawn background sync thread.
    let sync_rx = spawn_sync_thread(args.clone());

    let mut app = App::new(
        issues,
        has_next_page,
        end_cursor,
        args,
        Some(sync_rx),
        syncing,
        sync_status_label,
    );

    // Fetch viewer identity for header display (bd-185).
    if let Ok(Some(token)) = crate::config::load_token()
        && let Ok(viewer) = fetch_viewer(&token.access_token)
    {
        app.viewer_name = Some(viewer.name);
        app.org_name = Some(viewer.org_name);
    }

    let mut terminal = ratatui::init();
    app.status = initial_status;
    let result = run_app(&mut terminal, app);
    ratatui::restore();
    result
}

fn run_app(terminal: &mut ratatui::DefaultTerminal, mut app: App) -> Result<()> {
    loop {
        // Poll background sync channel (bd-25j).
        poll_sync_events(&mut app);

        // Poll modal background loader channel (bd-vfi).
        app.poll_modal_events();

        // Poll background comment-sync channel (bd-2mx).
        poll_detail_comment_events(&mut app);

        // Poll FTS search debounce (bd-2g4).
        poll_search_debounce(&mut app);

        // Poll background login channel (bd-vhp).
        poll_login_events(&mut app);

        terminal.draw(|frame| ui::render(frame, &mut app))?;

        if app.quit {
            return Ok(());
        }

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match app.mode {
                Mode::Popup(_) => handle_popup_key(&mut app, key.code),
                Mode::Detail => handle_detail_key(&mut app, key.code, key.modifiers),
                Mode::NewIssue => handle_new_issue_key(&mut app, key.code, key.modifiers),
                Mode::Help => handle_help_key(&mut app, key.code, key.modifiers),
                Mode::Search => handle_search_key(&mut app, key.code, key.modifiers),
                Mode::List => handle_normal_key(&mut app, key.code, key.modifiers),
            }
        }
    }
}

/// Non-blocking poll of the background comment-sync channel (bd-2mx).
///
/// When the background thread finishes syncing comments from the Linear API,
/// the refreshed list replaces the cached comments shown in the detail pane.
fn poll_detail_comment_events(app: &mut App) {
    let rx = match app.detail_comment_rx.take() {
        Some(r) => r,
        None => return,
    };

    let finished = match rx.try_recv() {
        Ok(CommentSyncEvent::Done(comments)) => {
            if let Some(ref mut detail) = app.detail {
                detail.comments.nodes = comments;
            }
            true
        }
        Ok(CommentSyncEvent::Error(_msg)) => {
            // Non-fatal: keep whatever cached comments are already shown.
            true
        }
        Err(mpsc::TryRecvError::Empty) => false,
        Err(mpsc::TryRecvError::Disconnected) => true,
    };

    if !finished {
        app.detail_comment_rx = Some(rx);
    }
}

/// Non-blocking poll of the background sync channel (bd-25j).
fn poll_sync_events(app: &mut App) {
    // Take the receiver out temporarily so we can mutate app freely.
    let rx = match app.sync_rx.take() {
        Some(r) => r,
        None => return,
    };

    let mut got_event = false;
    loop {
        match rx.try_recv() {
            Ok(SyncEvent::Done(_new_issues)) => {
                // Sync finished: refresh the issue list from SQLite so that
                // has_next_page and end_cursor are recalculated correctly.
                // Only refresh if the user is in normal list mode on page 1.
                if matches!(app.mode, Mode::List)
                    && app.cursor_stack.is_empty()
                    && app.current_cursor.is_none()
                {
                    app.do_fetch(false);
                }
                app.syncing = false;
                app.sync_status_label = build_sync_status_label(false);
                got_event = true;
            }
            Ok(SyncEvent::Error(msg)) => {
                app.syncing = false;
                app.sync_status_label = format!("sync error: {}", msg);
                if matches!(app.status, Status::Loading) {
                    app.status = Status::Idle;
                }
                got_event = true;
            }
            Ok(SyncEvent::NotAuthenticated) => {
                app.syncing = false;
                app.not_authenticated = true;
                app.sync_status_label = "not authenticated -- press L to log in".to_string();
                if matches!(app.status, Status::Loading) {
                    app.status = Status::Idle;
                }
                got_event = true;
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                app.syncing = false;
                if app.sync_status_label == "syncing..." {
                    app.sync_status_label = build_sync_status_label(false);
                }
                got_event = true;
                break;
            }
        }
    }

    // Put the receiver back if the thread may still send more messages.
    if !got_event || app.syncing {
        app.sync_rx = Some(rx);
    }
}

// -- New-issue modal key handler (bd-l6r) ------------------------------------

fn handle_new_issue_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let shift = modifiers.contains(KeyModifiers::SHIFT);

    // Ctrl-Enter submits the form.
    if ctrl && code == KeyCode::Enter {
        app.new_issue_submit();
        return;
    }

    // Esc cancels.
    if code == KeyCode::Esc {
        app.mode = Mode::List;
        app.new_issue_modal = None;
        return;
    }

    let modal = match app.new_issue_modal.as_mut() {
        Some(m) => m,
        None => return,
    };

    match &modal.focused_field.clone() {
        // ---- Text fields ----
        NewIssueField::Title => match code {
            KeyCode::Tab => {
                modal.focused_field = modal.focused_field.next();
            }
            KeyCode::BackTab => {
                modal.focused_field = modal.focused_field.prev();
            }
            _ => {
                modal.title.handle_key(code, modifiers);
            }
        },
        NewIssueField::Description => match code {
            KeyCode::Tab => {
                // Description is last field; Tab wraps to Title.
                modal.focused_field = modal.focused_field.next();
            }
            KeyCode::BackTab => {
                modal.focused_field = modal.focused_field.prev();
            }
            KeyCode::Enter => {
                modal.description.push('\n');
            }
            // Vim word/line deletion for the description field (cursor always at end).
            KeyCode::Backspace => {
                modal.description.pop();
            }
            KeyCode::Char('h') if ctrl => {
                modal.description.pop();
            }
            KeyCode::Char('w') if ctrl => {
                let trimmed = modal
                    .description
                    .trim_end_matches(|c: char| !c.is_whitespace());
                let new_end = trimmed.trim_end().len();
                modal.description.truncate(new_end);
            }
            KeyCode::Char('u') if ctrl => {
                modal.description.clear();
            }
            KeyCode::Char('k') if ctrl => {
                // cursor is at end, so ctrl+k is a no-op here
            }
            KeyCode::Char(c) if !ctrl => {
                modal.description.push(c);
            }
            _ => {}
        },
        // ---- Picker fields ----
        field => {
            let field = field.clone();
            match code {
                KeyCode::Tab if !shift => {
                    // When leaving Team field, pre-load states and assignees in background (bd-vfi).
                    if field == NewIssueField::Team {
                        let next = modal.focused_field.next();
                        modal.focused_field = next;
                        // Release the mutable borrow before calling the method.
                        let _ = modal;
                        app.new_issue_load_states_and_assignees_bg();
                    } else {
                        modal.focused_field = modal.focused_field.next();
                    }
                }
                KeyCode::BackTab => {
                    modal.focused_field = modal.focused_field.prev();
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    let (items_len, selected) = new_issue_picker_state(modal, &field);
                    if items_len > 0 {
                        *selected = (*selected + 1).min(items_len - 1);
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    let (_items_len, selected) = new_issue_picker_state(modal, &field);
                    *selected = selected.saturating_sub(1);
                }
                // "m" shortcut: select "Me (...)" entry in Assignee picker (bd-1fz).
                KeyCode::Char('m') if field == NewIssueField::Assignee => {
                    // The "Me (name)" entry is always at index 0 when present.
                    if let Some(first) = modal.assignees.first()
                        && first.label.starts_with("Me (")
                    {
                        modal.assignee_selected = 0;
                    }
                }
                KeyCode::Enter => {
                    // Enter on a picker field advances to the next field.
                    if field == NewIssueField::Team {
                        let next = modal.focused_field.next();
                        modal.focused_field = next;
                        // Release the mutable borrow before calling the method.
                        let _ = modal;
                        app.new_issue_load_states_and_assignees_bg();
                    } else {
                        modal.focused_field = modal.focused_field.next();
                    }
                }
                _ => {}
            }
        }
    }
}

/// Returns a mutable reference to (item count, selected index) for a picker field.
fn new_issue_picker_state<'a>(
    modal: &'a mut NewIssueModal,
    field: &NewIssueField,
) -> (usize, &'a mut usize) {
    match field {
        NewIssueField::Team => (modal.teams.len(), &mut modal.team_selected),
        NewIssueField::Priority => (modal.priorities.len(), &mut modal.priority_selected),
        NewIssueField::State => (modal.states.len(), &mut modal.state_selected),
        NewIssueField::Assignee => (modal.assignees.len(), &mut modal.assignee_selected),
        // Text fields should not reach here.
        _ => (0, &mut modal.team_selected),
    }
}

// -- Popup key handler (bd-3dz) ----------------------------------------------

fn handle_popup_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('j') | KeyCode::Down => app.popup_move(1),
        KeyCode::Char('k') | KeyCode::Up => app.popup_move(-1),
        KeyCode::Enter => app.popup_confirm(),
        KeyCode::Esc => app.popup_cancel(),
        _ => {}
    }
}

// -- Detail pane keybindings (bd-2g8, bd-1wz) --------------------------------
//
// Vim-like scrolling bindings:
//   j / Down        -- scroll down one line
//   k / Up          -- scroll up one line
//   g               -- scroll to top
//   G               -- scroll to bottom
//   Ctrl+d          -- scroll down half page
//   Ctrl+u          -- scroll up half page
//   PageDown        -- scroll down one page
//   PageUp          -- scroll up one page

fn handle_detail_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Esc | KeyCode::Char('q') => app.close_detail(),
        KeyCode::Char('j') | KeyCode::Down => app.detail_scroll_down(),
        KeyCode::Char('k') | KeyCode::Up => app.detail_scroll_up(),
        KeyCode::Char('g') => app.detail_scroll_to_top(),
        KeyCode::Char('G') => app.detail_scroll_to_bottom(),
        KeyCode::Char('d') if ctrl => app.detail_scroll_half_page_down(),
        KeyCode::Char('u') if ctrl => app.detail_scroll_half_page_up(),
        KeyCode::PageDown => app.detail_scroll_page_down(),
        KeyCode::PageUp => app.detail_scroll_page_up(),
        KeyCode::Char('o') => {
            if let Some(detail) = &app.detail {
                let url = format!("https://linear.app/issue/{}", detail.identifier);
                let _ = open::that(url);
            }
        }
        _ => {}
    }
}

// -- Normal list keybindings -------------------------------------------------

fn handle_normal_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Char('q') => app.quit = true,
        KeyCode::Esc => {
            // Double-esc (within 500ms) resets sort, filters, and search query
            // back to the state the TUI was launched with (bd-1jt).
            let now = Instant::now();
            let is_double_esc = app
                .last_esc_time
                .map(|t| t.elapsed() < Duration::from_millis(500))
                .unwrap_or(false);
            if is_double_esc {
                // Full reset to initial state.
                app.args = app.initial_args.clone();
                app.active_filter = app.initial_filter.clone();
                app.cursor_stack.clear();
                app.current_cursor = None;
                app.last_esc_time = None;
                app.do_fetch(true);
            } else {
                // First esc: standard refresh.
                app.last_esc_time = Some(now);
                app.do_fetch(true);
            }
        }
        // Open detail pane (bd-2g8, bd-22j: space opens detail)
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
            // default sort stem (bd-rbm).
            if app.active_filter.raw != search_query::DEFAULT_QUERY {
                overlay.query = TextInput::from_string(app.active_filter.raw.clone());
                overlay.ast = app.active_filter.clone();
                overlay.last_changed = Some(Instant::now());
            }
            app.search_overlay = Some(overlay);
            app.mode = Mode::Search;
        }
        // Write op keybindings (bd-3dz)
        KeyCode::Char('s') => app.open_state_popup(),
        KeyCode::Char('p') => app.open_priority_popup(),
        KeyCode::Char('a') => app.open_assignee_popup(),
        // New issue modal (bd-l6r)
        KeyCode::Char('n') => app.open_new_issue_modal(),
        // Help popup (bd-5lz)
        KeyCode::Char('?') => {
            app.help_popup = Some(HelpPopup::new());
            app.mode = Mode::Help;
        }
        // Re-authenticate (bd-vhp): background OAuth login.
        KeyCode::Char('L') => {
            if app.login_rx.is_none() {
                app.login_rx = Some(spawn_login_thread());
                app.sync_status_label =
                    "logging in -- complete authorization in browser".to_string();
            }
        }
        _ => {}
    }
}

// -- Help popup key handler (bd-5lz) -----------------------------------------

fn handle_help_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Esc => {
            app.mode = Mode::List;
            app.help_popup = None;
        }
        // Navigation: j/k/<down>/<up> move the filtered list.
        KeyCode::Down | KeyCode::Char('j') if !ctrl => {
            if let Some(ref mut popup) = app.help_popup {
                let max = popup.filtered.len().saturating_sub(1);
                if popup.selected < max {
                    popup.selected += 1;
                }
            }
        }
        KeyCode::Up | KeyCode::Char('k') if !ctrl => {
            if let Some(ref mut popup) = app.help_popup {
                popup.selected = popup.selected.saturating_sub(1);
            }
        }
        // Everything else goes to the TextInput search bar.
        _ => {
            if let Some(ref mut popup) = app.help_popup
                && popup.search.handle_key(code, modifiers)
            {
                popup.update_filter();
            }
        }
    }
}

// -- FTS search overlay key handler (bd-2g4) --------------------------------

fn handle_search_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Esc => {
            // Esc exits the search overlay and returns to the full list (go back).
            app.mode = Mode::List;
            app.search_overlay = None;
        }
        KeyCode::Char('c') if ctrl => {
            // Ctrl+C resets the search query back to the default.
            if let Some(ref mut overlay) = app.search_overlay {
                overlay.query = TextInput::from_string(search_query::DEFAULT_QUERY.to_string());
                overlay.last_changed = Some(Instant::now());
            }
        }
        KeyCode::Enter => {
            // Confirm: leave search mode with filtered results visible.
            // Transfer results into app.issues so normal keybindings work.
            if let Some(ref mut overlay) = app.search_overlay {
                // Flush any pending debounce so the AST and results reflect
                // every character the user typed before hitting Enter (bd-3r1).
                if overlay.last_changed.is_some() {
                    overlay.last_changed = None;
                    overlay.run_search(app.viewport_height, app.args.limit as usize);
                }
                let results = std::mem::take(&mut overlay.results);
                let selected = overlay.table_state.selected();
                // AST is the single source of truth (bd-rbm).
                app.active_filter = overlay.ast.clone();
                app.sync_args_from_filter();
                app.issues = results;
                let n = app.issues.len();
                let sel = selected.unwrap_or(0).min(n.saturating_sub(1));
                app.table_state.select(if n > 0 { Some(sel) } else { None });
            }
            app.mode = Mode::List;
            app.search_overlay = None;
        }
        // Result-list navigation: j/k/<down>/<up>.
        KeyCode::Down | KeyCode::Char('j') if !ctrl => {
            if let Some(ref mut overlay) = app.search_overlay {
                overlay.move_down();
            }
        }
        KeyCode::Up | KeyCode::Char('k') if !ctrl => {
            if let Some(ref mut overlay) = app.search_overlay {
                overlay.move_up();
            }
        }
        // Ctrl+N -- cycle completion forward.
        KeyCode::Char('n') if ctrl => {
            if let Some(ref mut overlay) = app.search_overlay {
                overlay.completer.cycle_next();
            }
        }
        // Ctrl+P -- cycle completion backward.
        KeyCode::Char('p') if ctrl => {
            if let Some(ref mut overlay) = app.search_overlay {
                overlay.completer.cycle_prev();
            }
        }
        // Ctrl+Y -- accept the highlighted completion candidate.
        KeyCode::Char('y') if ctrl => {
            if let Some(ref mut overlay) = app.search_overlay {
                let ast_snapshot = search_query::parse_query_ast(&overlay.query.value);
                if overlay
                    .completer
                    .accept_completion(&mut overlay.query, &ast_snapshot)
                {
                    let new_raw = overlay.query.value.clone();
                    overlay.ast = search_query::parse_query_ast(&new_raw);
                    overlay.completer.update(&overlay.ast, overlay.query.cursor);
                    overlay.last_changed = Some(Instant::now());
                }
            }
        }
        // Tab / Shift-Tab: apply stem-key completion (bd-3qb).
        // These must NOT be forwarded to TextInput::handle_key.
        KeyCode::Tab => {
            if let Some(ref mut overlay) = app.search_overlay {
                let ast_snapshot = search_query::parse_query_ast(&overlay.query.value);
                overlay
                    .completer
                    .apply_tab(&mut overlay.query, &ast_snapshot, true);
                let new_raw = overlay.query.value.clone();
                overlay.ast = search_query::parse_query_ast(&new_raw);
                overlay.completer.update(&overlay.ast, overlay.query.cursor);
                overlay.last_changed = Some(Instant::now());
            }
        }
        KeyCode::BackTab => {
            if let Some(ref mut overlay) = app.search_overlay {
                let ast_snapshot = search_query::parse_query_ast(&overlay.query.value);
                overlay
                    .completer
                    .apply_tab(&mut overlay.query, &ast_snapshot, false);
                let new_raw = overlay.query.value.clone();
                overlay.ast = search_query::parse_query_ast(&new_raw);
                overlay.completer.update(&overlay.ast, overlay.query.cursor);
                overlay.last_changed = Some(Instant::now());
            }
        }
        // Everything else goes to the TextInput query bar.
        _ => {
            if let Some(ref mut overlay) = app.search_overlay
                && overlay.query.handle_key(code, modifiers)
            {
                overlay.last_changed = Some(Instant::now());
            }
        }
    }
}

/// Fire the FTS search when the debounce interval (150ms) has elapsed.
fn poll_search_debounce(app: &mut App) {
    let should_search = match app.search_overlay {
        Some(ref overlay) => match overlay.last_changed {
            Some(t) => t.elapsed() >= Duration::from_millis(150),
            None => false,
        },
        None => false,
    };
    if should_search && let Some(ref mut overlay) = app.search_overlay {
        overlay.last_changed = None;
        overlay.run_search(app.viewport_height, app.args.limit as usize);
    }
}
