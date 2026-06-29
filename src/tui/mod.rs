mod markdown;
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
    /// If set, the range `cursor..selection_end` is "selected" (highlighted).
    /// `selection_end` is always >= cursor and always on a char boundary.
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
        let Some(ch) = self.value[self.cursor..].chars().next() else {
            return self.cursor;
        };
        self.cursor + ch.len_utf8()
    }

    fn prev_word_boundary(&self) -> usize {
        let before = &self.value[..self.cursor];
        let trimmed = before.trim_end();
        match trimmed.rfind(|c: char| c.is_whitespace()) {
            Some(i) => trimmed[i..]
                .chars()
                .next()
                .map_or(i, |ws_char| i + ws_char.len_utf8()),
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
    /// If a selection is active (`selection_end` is set), the selected range is
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
    /// Handle the deletion key bindings. Returns `Some(true)` when a deletion
    /// key was handled (the buffer always changes), or `None` if `code` is not
    /// a deletion key.
    fn handle_deletion_key(&mut self, code: KeyCode, ctrl: bool, alt: bool) -> Option<bool> {
        match code {
            KeyCode::Backspace => self.backspace(),
            KeyCode::Char('h') if ctrl => self.backspace(),
            KeyCode::Char('w') if ctrl => self.delete_word_before(),
            KeyCode::Char('u') if ctrl => self.delete_to_start(),
            KeyCode::Char('k') if ctrl => self.delete_to_end(),
            KeyCode::Char('d') if ctrl => self.delete_forward(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Char('d') if alt => self.delete_word_after(),
            _ => return None,
        }
        Some(true)
    }

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        let alt = modifiers.contains(KeyModifiers::ALT);
        if let Some(changed) = self.handle_deletion_key(code, ctrl, alt) {
            return changed;
        }
        match code {
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

#[cfg(test)]
mod text_input_tests {
    use super::*;

    fn input(s: &str, cursor: usize) -> TextInput {
        TextInput {
            value: s.to_string(),
            cursor,
            selection_end: None,
        }
    }

    #[test]
    fn from_string_places_cursor_at_end() {
        let t = TextInput::from_string("hello".to_string());
        assert_eq!(t.cursor, 5);
        assert_eq!(t.as_str(), "hello");
    }

    #[test]
    fn display_parts_splits_around_cursor() {
        let t = input("hello", 2);
        assert_eq!(t.display_parts(), ("he", Some('l'), "lo"));
        let at_end = input("hi", 2);
        assert_eq!(at_end.display_parts(), ("hi", None, ""));
    }

    #[test]
    fn insert_advances_cursor_and_handles_multibyte() {
        let mut t = TextInput::new();
        t.insert('a');
        t.insert('é'); // 2-byte char
        assert_eq!(t.value, "aé");
        assert_eq!(t.cursor, 3);
    }

    #[test]
    fn move_left_right_respect_char_boundaries() {
        let mut t = input("aé", 3);
        t.move_left();
        assert_eq!(t.cursor, 1); // stepped over the 2-byte 'é'
        t.move_left();
        assert_eq!(t.cursor, 0);
        t.move_left(); // clamp at 0
        assert_eq!(t.cursor, 0);
        t.move_right();
        assert_eq!(t.cursor, 1);
        t.move_end();
        assert_eq!(t.cursor, 3);
        t.move_right(); // clamp at end
        assert_eq!(t.cursor, 3);
        t.move_start();
        assert_eq!(t.cursor, 0);
    }

    #[test]
    fn word_movement_skips_whitespace_runs() {
        let mut t = input("foo  bar baz", 12);
        t.move_word_left();
        assert_eq!(&t.value[t.cursor..], "baz");
        t.move_word_left();
        assert_eq!(&t.value[t.cursor..], "bar baz");
        let mut f = input("foo  bar", 0);
        f.move_word_right();
        assert_eq!(f.cursor, 3); // stops at end of "foo"
        f.move_word_right();
        assert_eq!(f.cursor, 8); // skips spaces, then to end of "bar"
    }

    #[test]
    fn backspace_and_delete_forward() {
        let mut t = input("abc", 2);
        t.backspace();
        assert_eq!((t.value.as_str(), t.cursor), ("ac", 1));
        let mut at_start = input("abc", 0);
        at_start.backspace(); // no-op
        assert_eq!(at_start.value, "abc");
        let mut d = input("abc", 1);
        d.delete_forward();
        assert_eq!((d.value.as_str(), d.cursor), ("ac", 1));
        let mut at_end = input("abc", 3);
        at_end.delete_forward(); // no-op
        assert_eq!(at_end.value, "abc");
    }

    #[test]
    fn word_and_line_deletions() {
        let mut w = input("foo bar", 7);
        w.delete_word_before();
        assert_eq!((w.value.as_str(), w.cursor), ("foo ", 4));
        let mut a = input("foo bar", 3);
        a.delete_word_after();
        assert_eq!(a.value, "foo"); // deletes " bar"
        let mut u = input("foo bar", 4);
        u.delete_to_start();
        assert_eq!((u.value.as_str(), u.cursor), ("bar", 0));
        let mut k = input("foo bar", 3);
        k.delete_to_end();
        assert_eq!(k.value, "foo");
    }

    #[test]
    fn selection_is_replaced_or_deleted_then_cleared() {
        // Insert over a selection replaces the range.
        let mut t = TextInput {
            value: "hello".to_string(),
            cursor: 1,
            selection_end: Some(4),
        };
        t.insert('X');
        assert_eq!(t.value, "hXo");
        assert!(t.selection_end.is_none());

        // Backspace deletes the selection.
        let mut b = TextInput {
            value: "hello".to_string(),
            cursor: 1,
            selection_end: Some(4),
        };
        b.backspace();
        assert_eq!(b.value, "ho");
        assert!(b.selection_end.is_none());

        // delete_forward deletes the selection.
        let mut d = TextInput {
            value: "hello".to_string(),
            cursor: 1,
            selection_end: Some(4),
        };
        d.delete_forward();
        assert_eq!(d.value, "ho");

        // Movement clears a selection without editing.
        let mut m = TextInput {
            value: "hello".to_string(),
            cursor: 1,
            selection_end: Some(4),
        };
        m.move_left();
        assert!(m.selection_end.is_none());
        assert_eq!(m.value, "hello");
    }

    #[test]
    fn handle_key_insert_movement_and_unhandled() {
        let ctrl = KeyModifiers::CONTROL;
        let none = KeyModifiers::NONE;
        let mut ti = TextInput::new();

        // Insert returns true (changed).
        assert!(ti.handle_key(KeyCode::Char('a'), none));
        assert_eq!(ti.value, "a");

        // Movement returns false (cursor only).
        for (code, mods) in [
            (KeyCode::Left, none),
            (KeyCode::Char('e'), ctrl), // move_end
            (KeyCode::Char('a'), ctrl), // move_start
            (KeyCode::Home, none),
            (KeyCode::End, none),
            (KeyCode::Right, ctrl), // word right
        ] {
            assert!(!ti.handle_key(code, mods));
        }

        // Unhandled key, and a non-binding ctrl+char, are ignored.
        assert!(!ti.handle_key(KeyCode::Esc, none));
        assert!(!ti.handle_key(KeyCode::Char('z'), ctrl));
        assert_eq!(ti.value, "a");
    }

    #[test]
    fn handle_key_deletions_change_buffer() {
        let ctrl = KeyModifiers::CONTROL;
        let alt = KeyModifiers::ALT;
        let none = KeyModifiers::NONE;

        let mut wrd = input("foo bar", 7);
        assert!(wrd.handle_key(KeyCode::Char('w'), ctrl)); // delete word before
        assert_eq!(wrd.value, "foo ");
        assert!(wrd.handle_key(KeyCode::Backspace, none));
        assert!(wrd.handle_key(KeyCode::Char('u'), ctrl)); // delete to start
        assert_eq!(wrd.value, "");

        let mut fwd = input("abcd", 0);
        assert!(fwd.handle_key(KeyCode::Char('d'), ctrl)); // forward delete
        assert_eq!(fwd.value, "bcd");
        assert!(fwd.handle_key(KeyCode::Delete, none));
        assert_eq!(fwd.value, "cd");

        let mut cut = input("hello world", 5);
        assert!(cut.handle_key(KeyCode::Char('k'), ctrl)); // delete to end
        assert_eq!(cut.value, "hello");

        let mut after = input("foo bar", 0);
        assert!(after.handle_key(KeyCode::Char('d'), alt)); // delete word after
        assert_eq!(after.value, " bar");
    }
}

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;

use crate::issues::IssueArgs;
use crate::issues::list::Issue;
use crate::linear::client::HttpTransport;
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
    /// Sync completed successfully; includes the refreshed issue list and,
    /// when requested, the authenticated identity for the header (bd-185).
    Done(Vec<Issue>, Option<crate::linear::viewer::Viewer>),
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

/// Linear priority options as popup items.
/// Index matches the Linear priority value: 0=No priority, 1=Urgent, 2=High,
/// 3=Normal, 4=Low.
fn priority_popup_items() -> Vec<PopupItem> {
    vec![
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
    ]
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

/// Mutable state for the help popup.
pub struct HelpPopup {
    /// Current search query typed by the user.
    pub search: TextInput,
    /// Indices into `ALL_KEYBINDINGS` that match the current search.
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
    /// True once `run_search()` has been called at least once (bd-zjy).
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
            Self::Title | Self::Team => Self::Title,
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

/// Forward/backward pagination state.
pub struct Pagination {
    pub has_next_page: bool,
    pub current_cursor: Option<String>,
    pub cursor_stack: Vec<Option<String>>,
    pub end_cursor: Option<String>,
}

/// Background sync state (bd-25j).
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
    pub sync: SyncState,

    // -- background comment sync (bd-2mx) ------------------------------------
    /// Receiver for background comment-sync events.
    pub detail_comment_rx: Option<mpsc::Receiver<CommentSyncEvent>>,

    // -- comment input --------------------------------------------------------
    /// Multiline buffer for a new comment, open in the detail pane.
    /// The cursor is always at the end (same model as the new-issue
    /// description field).
    pub comment_input: Option<String>,

    /// Terminal/session capability flags.
    pub session: Session,

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

    /// Keep app.args.sort/desc in sync with `active_filter` (bd-rbm).
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
    /// self.args.sort/desc.  Used by `cycle_sort` and `toggle_desc` (bd-rbm).
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
            // preserve them (bd-2i0).
            let limit = self.args.limit.min(250) as usize;
            match crate::db::open_db()
                .and_then(|conn| search_query::run_query(&conn, &parsed, limit))
            {
                Ok(db_issues) => {
                    self.issues = db_issues.into_iter().map(db_issue_to_list_issue).collect();
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
            match crate::db::open_db()
                .and_then(|conn| crate::db::query_issues_page(&conn, &self.args, offset))
            {
                Ok((issues, has_next_page)) => {
                    self.issues = issues.into_iter().map(db_issue_to_list_issue).collect();
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

    // -- Detail pane (bd-2g8) -------------------------------------------------

    /// Open the detail pane for the currently selected issue.
    ///
    /// The detail is populated instantly from the local SQLite cache so the
    /// pane appears without any network round-trip.  A background thread then
    /// calls `sync_comments` via the Linear API and sends the refreshed comment
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

        self.detail = Some(build_cached_detail(&issue, cached_comments));

        // Populate parent and children from the local DB cache.
        if let Some(ref mut detail) = self.detail {
            populate_relations(detail, &issue);
        }

        self.status = Status::Idle;

        // Spawn background thread to refresh comments from the Linear API.
        let issue_id = issue.id.clone();
        let (tx, rx) = std::sync::mpsc::channel::<CommentSyncEvent>();
        self.detail_comment_rx = Some(rx);

        std::thread::spawn(move || {
            let Ok(Some(token)) = crate::config::load_token() else {
                let _ = tx.send(CommentSyncEvent::Error("not logged in".to_string()));
                return;
            };
            let conn = match crate::db::open_db() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(CommentSyncEvent::Error(e.to_string()));
                    return;
                }
            };
            match crate::sync::comments::sync_comments(
                &conn,
                &HttpTransport::new(token.access_token),
                &issue_id,
            ) {
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
        self.comment_input = None;
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
        self.detail_scroll_by((self.viewport_height / 2).max(1), true);
    }

    fn detail_scroll_half_page_up(&mut self) {
        self.detail_scroll_by((self.viewport_height / 2).max(1), false);
    }

    fn detail_scroll_page_down(&mut self) {
        self.detail_scroll_by(self.viewport_height.max(1), true);
    }

    fn detail_scroll_page_up(&mut self) {
        self.detail_scroll_by(self.viewport_height.max(1), false);
    }

    /// Scroll the detail pane by `step` rows, `down` toward the bottom.
    fn detail_scroll_by(&mut self, step: u16, down: bool) {
        self.detail_scroll = if down {
            self.detail_scroll.saturating_add(step)
        } else {
            self.detail_scroll.saturating_sub(step)
        };
    }

    // -- Comment input ---------------------------------------------------------

    /// Submit the comment buffer to the Linear API.
    ///
    /// The comment is appended to the detail pane optimistically; a background
    /// thread runs the commentCreate mutation, re-syncs the issue's comments,
    /// and delivers the authoritative list via `detail_comment_rx`.  On error
    /// the optimistic comment is dropped (see `poll_detail_comment_events`).
    fn submit_comment(&mut self) {
        let body = match self.comment_input.as_ref() {
            Some(b) => b.trim().to_string(),
            None => return,
        };
        if body.is_empty() {
            self.comment_input = None;
            return;
        }
        let issue_id = match self.selected_issue() {
            Some(i) => i.id.clone(),
            None => return,
        };
        let Ok(Some(token)) = crate::config::load_token() else {
            self.footer_msg = Some("Not logged in".to_string());
            return;
        };
        self.comment_input = None;

        // Optimistic: show the comment immediately.
        if let Some(ref mut detail) = self.detail {
            detail.comments.nodes.push(crate::linear::types::Comment {
                body: body.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
                user: self
                    .viewer_name
                    .clone()
                    .map(|name| crate::linear::types::CommentUser { name }),
            });
        }

        let (tx, rx) = mpsc::channel::<CommentSyncEvent>();
        self.detail_comment_rx = Some(rx);

        std::thread::spawn(move || {
            let result = (|| -> Result<Vec<crate::linear::types::Comment>> {
                let transport = HttpTransport::new(token.access_token);
                crate::linear::mutations::create_comment(&transport, &issue_id, &body)?;
                let conn = crate::db::open_db()?;
                crate::sync::comments::sync_comments(&conn, &transport, &issue_id)?;
                Ok(crate::db::query_comments(&conn, &issue_id)?
                    .into_iter()
                    .map(db_comment_to_api)
                    .collect())
            })();
            match result {
                Ok(fresh) => {
                    let _ = tx.send(CommentSyncEvent::Done(fresh));
                }
                Err(e) => {
                    let _ = tx.send(CommentSyncEvent::PostError(e.to_string()));
                }
            }
        });
    }

    // -- Popup helpers (bd-3dz) -----------------------------------------------

    fn open_state_popup(&mut self) {
        let issue = match self.selected_issue() {
            Some(i) => i.clone(),
            None => return,
        };
        let Ok(Some(token)) = crate::config::load_token() else {
            self.footer_msg = Some("Not logged in".to_string());
            return;
        };
        let current_state_name = issue.state.name.clone();
        match crate::linear::mutations::fetch_workflow_states(
            &HttpTransport::new(token.access_token),
            &issue.team.id,
        ) {
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
                self.footer_msg = Some(format!("Failed to fetch states: {e}"));
            }
        }
    }

    fn open_priority_popup(&mut self) {
        let Some(priority) = self.selected_issue().map(|i| i.priority) else {
            return;
        };
        // Linear priority: 0=No priority, 1=Urgent, 2=High, 3=Normal, 4=Low
        self.popup_items = priority_popup_items();
        self.popup_selected = priority as usize;
        self.mode = Mode::Popup(PopupKind::Priority);
        self.footer_msg = None;
    }

    fn open_assignee_popup(&mut self) {
        let issue = match self.selected_issue() {
            Some(i) => i.clone(),
            None => return,
        };
        let Ok(Some(token)) = crate::config::load_token() else {
            self.footer_msg = Some("Not logged in".to_string());
            return;
        };
        let mut items: Vec<PopupItem> = vec![PopupItem {
            label: "Unassign".to_string(),
            id: None,
        }];
        match fetch_team_members(&HttpTransport::new(token.access_token), &issue.team.id) {
            Ok(members) => {
                for m in members {
                    items.push(PopupItem {
                        label: m.name,
                        id: Some(m.id),
                    });
                }
            }
            Err(e) => {
                self.footer_msg = Some(format!("Failed to fetch members: {e}"));
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
        let step = usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX);
        self.popup_selected = if delta >= 0 {
            self.popup_selected.saturating_add(step).min(n - 1)
        } else {
            self.popup_selected.saturating_sub(step)
        };
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
            let Ok(Some(token)) = crate::config::load_token() else {
                return;
            };
            let transport = HttpTransport::new(token.access_token);
            let result: anyhow::Result<()> = match kind2 {
                PopupKind::State => {
                    if let Some(state_id) = &item2.id {
                        crate::linear::mutations::update_issue_state(
                            &transport, &issue_id, state_id,
                        )
                        .map(|_| ())
                    } else {
                        Ok(())
                    }
                }
                PopupKind::Priority => {
                    if let Some(pstr) = &item2.id {
                        let p: u8 = pstr.parse().unwrap_or(0);
                        crate::linear::mutations::update_issue_priority(&transport, &issue_id, p)
                            .map(|_| ())
                    } else {
                        Ok(())
                    }
                }
                PopupKind::Assignee => crate::linear::mutations::update_issue_assignee(
                    &transport,
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
        let Ok(Some(token)) = crate::config::load_token() else {
            self.footer_msg = Some("Not logged in".to_string());
            return;
        };

        // Pre-fill team from active filter if set.
        let preset_team = self.args.team.clone();

        let mut modal = NewIssueModal {
            focused_field: NewIssueField::Title,
            title: TextInput::new(),
            description: String::new(),
            teams: Vec::new(),
            team_selected: 0,
            priorities: priority_popup_items(),
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
        match crate::linear::mutations::fetch_teams(&HttpTransport::new(token.access_token)) {
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
                modal.error = format!("Failed to fetch teams: {e}");
                modal.loading = false;
            }
        }

        self.mode = Mode::NewIssue;
        self.new_issue_modal = Some(modal);
    }

    /// Kick off background loading of states and assignees for the selected team (bd-vfi).
    fn new_issue_load_states_and_assignees_bg(&mut self) {
        let Some(modal) = self.new_issue_modal.as_mut() else {
            return;
        };
        let Some(team_id) = modal
            .teams
            .get(modal.team_selected)
            .and_then(|t| t.id.clone())
        else {
            return;
        };

        modal.loading = true;
        modal.error.clear();

        let (tx, rx) = mpsc::channel::<ModalEvent>();
        modal.modal_rx = Some(rx);

        std::thread::spawn(move || {
            let Ok(Some(token)) = crate::config::load_token() else {
                let _ = tx.send(ModalEvent::LoadError("Not logged in".to_string()));
                return;
            };

            let transport = HttpTransport::new(token.access_token);

            // Fetch viewer for "me" shortcut (bd-1fz).
            let viewer = fetch_viewer(&transport).ok();

            // Fetch states.
            match crate::linear::mutations::fetch_workflow_states(&transport, &team_id) {
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
                        "Failed to fetch states: {e}"
                    )));
                    return;
                }
            }

            // Fetch assignees.
            match fetch_team_members(&transport, &team_id) {
                Ok(members) => {
                    let items = build_assignee_items(viewer.as_ref(), members);
                    let _ = tx.send(ModalEvent::AssigneesLoaded(items));
                }
                Err(e) => {
                    let _ = tx.send(ModalEvent::LoadError(format!(
                        "Failed to fetch assignees: {e}"
                    )));
                }
            }
        });
    }

    fn new_issue_submit(&mut self) {
        let Ok(Some(token)) = crate::config::load_token() else {
            if let Some(m) = self.new_issue_modal.as_mut() {
                m.error = "Not logged in".to_string();
            }
            return;
        };

        let Some(modal) = self.new_issue_modal.as_ref() else {
            return;
        };

        if modal.title.value.trim().is_empty() {
            if let Some(m) = self.new_issue_modal.as_mut() {
                m.error = "Title is required".to_string();
                m.focused_field = NewIssueField::Title;
            }
            return;
        }

        let Some(team_id) = modal
            .teams
            .get(modal.team_selected)
            .and_then(|t| t.id.clone())
        else {
            if let Some(m) = self.new_issue_modal.as_mut() {
                m.error = "Select a team".to_string();
            }
            return;
        };

        let (input, display) = build_create_request(modal, team_id);

        match crate::linear::mutations::create_issue(&HttpTransport::new(token.access_token), input)
        {
            Ok(created) => {
                cache_created_issue(&created, display);
                // Refresh list and highlight new issue (bd-3ba).
                let new_identifier = created.identifier.clone();
                self.mode = Mode::List;
                self.new_issue_modal = None;
                self.footer_msg = Some(format!("Created {}", created.identifier));
                self.do_fetch_and_select(Some(new_identifier));
            }
            Err(e) => {
                if let Some(m) = self.new_issue_modal.as_mut() {
                    m.error = format!("Failed to create issue: {e}");
                }
            }
        }
    }

    /// Poll modal background channel and update modal state (bd-vfi).
    fn poll_modal_events(&mut self) {
        // Collect events before mutating -- avoids borrow issues.
        let events: Vec<ModalEvent> = {
            let Some(modal) = self.new_issue_modal.as_ref() else {
                return;
            };
            let Some(rx) = modal.modal_rx.as_ref() else {
                return;
            };
            let mut evts = Vec::new();
            while let Ok(ev) = rx.try_recv() {
                evts.push(ev);
            }
            evts
        };

        for ev in events {
            let Some(modal) = self.new_issue_modal.as_mut() else {
                break;
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

use crate::linear::viewer::fetch_viewer;

// ---------------------------------------------------------------------------
// Team member fetch (used by assignee popup)
// ---------------------------------------------------------------------------

struct Member {
    pub id: String,
    pub name: String,
}

/// Display fields captured from the new-issue modal, used to optimistically
/// cache a freshly-created issue before the next sync overwrites it.
struct CreatedIssueDisplay {
    title: String,
    priority_label: String,
    state_name: String,
    assignee_name: Option<String>,
    team_name: String,
    team_key: String,
}

/// Build the create-issue API input and the display fields used for optimistic
/// caching from the modal's current selections. `team_id` is the resolved
/// (validated) team id.
fn build_create_request(
    modal: &NewIssueModal,
    team_id: String,
) -> (
    crate::linear::mutations::CreateIssueInput,
    CreatedIssueDisplay,
) {
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

    let display = CreatedIssueDisplay {
        title: input.title.clone(),
        priority_label: modal
            .priorities
            .get(modal.priority_selected)
            .map_or_else(|| "No priority".to_string(), |p| p.label.clone()),
        state_name: modal
            .states
            .get(modal.state_selected)
            .map_or_else(|| "Backlog".to_string(), |s| s.label.clone()),
        assignee_name: modal.assignees.get(modal.assignee_selected).and_then(|a| {
            if a.id.is_some() {
                Some(a.label.clone())
            } else {
                None
            }
        }),
        team_name: modal
            .teams
            .get(modal.team_selected)
            .map(|t| t.label.clone())
            .unwrap_or_default(),
        team_key: team_id,
    };

    (input, display)
}

/// Optimistically insert a freshly-created issue into the local SQLite cache.
fn cache_created_issue(
    created: &crate::linear::mutations::CreatedIssue,
    display: CreatedIssueDisplay,
) {
    let now = chrono::Utc::now().to_rfc3339();
    let db_issue = crate::db::Issue {
        id: created.id.clone(),
        identifier: created.identifier.clone(),
        title: display.title,
        priority_label: display.priority_label,
        state_name: display.state_name,
        assignee_name: display.assignee_name,
        team_name: display.team_name,
        team_key: Some(display.team_key),
        created_at: now.clone(),
        updated_at: now,
        synced_at: chrono::Utc::now().to_rfc3339(),
        description: None,
        labels: String::new(),
        project_name: None,
        cycle_name: None,
        creator_name: None,
        parent_id: None,
        parent_identifier: None,
    };
    if let Ok(conn) = crate::db::open_db() {
        let _ = crate::db::upsert_issues(&conn, &[db_issue]);
    }
}

/// Build an `IssueDetail` from a cached list `Issue` plus its cached comments.
fn build_cached_detail(
    issue: &Issue,
    cached_comments: Vec<crate::linear::types::Comment>,
) -> crate::linear::types::IssueDetail {
    crate::linear::types::IssueDetail {
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
        parent: None,
        children: Vec::new(),
    }
}

/// Populate a detail's parent/children fields from the local DB cache.
fn populate_relations(detail: &mut crate::linear::types::IssueDetail, issue: &Issue) {
    let Ok(conn) = crate::db::open_db() else {
        return;
    };
    // Look up children.
    if let Ok(children) = crate::db::query_children(&conn, &issue.id) {
        detail.children = children
            .into_iter()
            .map(|c| crate::linear::types::IssueRef {
                identifier: c.identifier,
                title: c.title,
                state_name: c.state_name,
            })
            .collect();
    }
    // Look up parent.
    if let Some(ref parent) = issue.parent {
        let parent_sql = "SELECT identifier, title, state_name FROM issues WHERE id = ?1";
        if let Ok(mut stmt) = conn.prepare(parent_sql)
            && let Ok(row) = stmt.query_row(rusqlite::params![parent.id], |row| {
                Ok(crate::linear::types::IssueRef {
                    identifier: row.get(0)?,
                    title: row.get(1)?,
                    state_name: row.get(2)?,
                })
            })
        {
            detail.parent = Some(row);
        }
    }
}

/// Build the assignee popup items: "Me (name)" at top if the viewer is known,
/// then "Unassigned", then the remaining team members (excluding the viewer).
fn build_assignee_items(
    viewer: Option<&crate::linear::viewer::Viewer>,
    members: Vec<Member>,
) -> Vec<PopupItem> {
    let mut items: Vec<PopupItem> = Vec::new();
    if let Some(v) = viewer {
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
        if viewer.is_some_and(|v| v.id == m.id) {
            continue;
        }
        items.push(PopupItem {
            label: m.name,
            id: Some(m.id),
        });
    }
    items
}

fn fetch_team_members(
    transport: &dyn crate::linear::client::GraphqlTransport,
    team_id: &str,
) -> Result<Vec<Member>> {
    use serde::Deserialize;
    use serde_json::json;

    const TEAM_MEMBERS_QUERY: &str = r"
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
";

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
        crate::linear::client::query_as(transport, TEAM_MEMBERS_QUERY, variables)?;
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
    let Ok(conn) = crate::db::open_db() else {
        return;
    };
    let db_issue = build_db_issue_optimistic(issue, kind, item);
    let _ = crate::db::upsert_issues(&conn, &[db_issue]);
}

fn revert_sqlite(orig: &crate::issues::list::Issue, _kind: &PopupKind) {
    let Ok(conn) = crate::db::open_db() else {
        return;
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
        cycle_name: orig.cycle.as_ref().and_then(|c| c.name.clone()),
        creator_name: orig.creator.as_ref().map(|u| u.name.clone()),
        parent_id: orig.parent.as_ref().map(|p| p.id.clone()),
        parent_identifier: orig.parent.as_ref().map(|p| p.identifier.clone()),
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
        cycle_name: issue.cycle.as_ref().and_then(|c| c.name.clone()),
        creator_name: issue.creator.as_ref().map(|u| u.name.clone()),
        parent_id: issue.parent.as_ref().map(|p| p.id.clone()),
        parent_identifier: issue.parent.as_ref().map(|p| p.identifier.clone()),
    }
}

fn apply_optimistic_in_memory(app: &mut App, kind: &PopupKind, item: &PopupItem) {
    let Some(issue) = app.selected_issue_mut() else {
        return;
    };
    match kind {
        PopupKind::State => {
            issue.state.name.clone_from(&item.label);
            if let Some(id) = &item.id {
                issue.state.id.clone_from(id);
            }
        }
        PopupKind::Priority => {
            issue.priority_label.clone_from(&item.label);
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
                    match mins {
                        ..=0 => "synced just now".to_string(),
                        1 => "synced 1 min ago".to_string(),
                        _ => format!("synced {mins} min ago"),
                    }
                }
                Err(_) => "synced".to_string(),
            }
        }
    }
}

/// Spawn a background sync thread and return the receiver (bd-25j).
///
/// When `full` is true the thread runs a full sync (re-fetches every issue);
/// otherwise it runs a delta sync (only issues updated since last sync).
///
/// When `fetch_identity` is true the thread also fetches the viewer identity
/// after a successful sync and includes it in `SyncEvent::Done`.  This keeps
/// the header current when authentication happened outside the TUI's own
/// login flow -- e.g. the sync's automatic re-auth, or `lt auth login` run in
/// another terminal.
fn spawn_sync_thread(
    args: IssueArgs,
    full: bool,
    fetch_identity: bool,
) -> mpsc::Receiver<SyncEvent> {
    let (tx, rx) = mpsc::channel::<SyncEvent>();
    std::thread::spawn(move || {
        // Skip sync when no auth token is stored; notify the TUI.
        match crate::config::load_token() {
            Ok(None) | Err(_) => {
                let _ = tx.send(SyncEvent::NotAuthenticated);
                return;
            }
            Ok(Some(_)) => {}
        }

        // Run the requested sync variant.
        let result = if full {
            crate::sync::full::run()
        } else {
            crate::sync::delta::run()
        };
        match result {
            Ok(()) => {
                // Re-query SQLite for a fresh issue list to send to TUI.
                let issues = (|| -> Result<Vec<Issue>> {
                    let conn = crate::db::open_db()?;
                    let db_issues = crate::db::query_issues(&conn, &args)?;
                    // Convert db::Issue -> issues::list::Issue.
                    Ok(db_issues.into_iter().map(db_issue_to_list_issue).collect())
                })();
                // A successful sync implies a valid token, so the identity
                // fetch is expected to succeed; failures leave the header
                // unchanged and the next sync retries.
                let viewer = if fetch_identity {
                    crate::config::load_token()
                        .ok()
                        .flatten()
                        .and_then(|t| fetch_viewer(&HttpTransport::new(t.access_token)).ok())
                } else {
                    None
                };
                match issues {
                    Ok(list) => {
                        let _ = tx.send(SyncEvent::Done(list, viewer));
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
            // Fetch viewer identity while the token is fresh (bd-3jl).
            let viewer = crate::config::load_token()
                .ok()
                .flatten()
                .and_then(|t| fetch_viewer(&HttpTransport::new(t.access_token)).ok());
            let _ = tx.send(LoginEvent::Success {
                viewer_name: viewer.as_ref().map(|v| v.name.clone()),
                org_name: viewer.as_ref().map(|v| v.org_name.clone()),
            });
        }
        Err(e) => {
            let _ = tx.send(LoginEvent::Error(e.to_string()));
        }
    });
    rx
}

/// Poll the background login channel and update app state on completion.
fn poll_login_events(app: &mut App) {
    let Some(rx) = app.login_rx.as_ref() else {
        return;
    };
    match rx.try_recv() {
        Ok(LoginEvent::Success {
            viewer_name,
            org_name,
        }) => {
            app.login_rx = None;
            if let Some(name) = viewer_name {
                app.viewer_name = Some(name);
            }
            if let Some(org) = org_name {
                app.org_name = Some(org);
            }
            app.session.not_authenticated = false;
            app.sync.syncing = true;
            app.sync.sync_status_label = build_sync_status_label(true);
            app.sync.sync_rx = Some(spawn_sync_thread(
                app.args.clone(),
                false,
                app.viewer_name.is_none(),
            ));
        }
        Ok(LoginEvent::Error(msg)) => {
            app.login_rx = None;
            app.footer_msg = Some(format!("Login failed: {msg}"));
            app.sync.sync_status_label = "not authenticated -- press L to log in".to_string();
        }
        Err(mpsc::TryRecvError::Empty) => {} // still waiting
        Err(mpsc::TryRecvError::Disconnected) => {
            app.login_rx = None;
        }
    }
}

/// Convert a `crate::db::Comment` row to the API comment type shown in the
/// detail pane.
fn db_comment_to_api(c: crate::db::Comment) -> crate::linear::types::Comment {
    crate::linear::types::Comment {
        body: c.body,
        created_at: c.created_at,
        user: c
            .author_name
            .map(|name| crate::linear::types::CommentUser { name }),
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
            name: Some(n),
        }),
        creator: src.creator_name.map(|n| crate::issues::list::User {
            id: String::new(),
            name: n,
        }),
        parent: src.parent_id.map(|id| crate::issues::list::Parent {
            id,
            identifier: src.parent_identifier.unwrap_or_default(),
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
            let limit = i64::from(args.limit.min(250));
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

    // Fetch viewer identity for header display (bd-185).
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
    let result = run_app(&mut terminal, app);
    if keyboard_enhanced {
        let _ = crossterm::execute!(std::io::stdout(), event::PopKeyboardEnhancementFlags);
    }
    ratatui::restore();
    result
}

fn run_app(terminal: &mut ratatui::DefaultTerminal, mut app: App) -> Result<()> {
    loop {
        // Poll background sync channel (bd-25j).
        poll_sync_events(&mut app);

        // Periodic delta sync: fire every 30s when authenticated.
        if !app.sync.syncing
            && !app.session.not_authenticated
            && let Some(t) = app.sync.next_sync_at
            && Instant::now() >= t
        {
            app.sync.syncing = true;
            app.sync.sync_status_label = build_sync_status_label(true);
            app.sync.sync_rx = Some(spawn_sync_thread(
                app.args.clone(),
                false,
                app.viewer_name.is_none(),
            ));
            app.sync.next_sync_at = None;
        }

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
    let Some(rx) = app.detail_comment_rx.take() else {
        return;
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
        Ok(CommentSyncEvent::PostError(msg)) => {
            // Posting failed: drop the optimistic comment by reloading the
            // cached set, and surface the error in the footer.
            let cached = app.selected_issue().map(|i| i.id.clone()).and_then(|id| {
                crate::db::open_db()
                    .and_then(|conn| crate::db::query_comments(&conn, &id))
                    .ok()
            });
            if let (Some(detail), Some(comments)) = (app.detail.as_mut(), cached) {
                detail.comments.nodes = comments.into_iter().map(db_comment_to_api).collect();
            }
            app.footer_msg = Some(format!("Failed to post comment: {msg}"));
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
    let Some(rx) = app.sync.sync_rx.take() else {
        return;
    };

    let mut got_event = false;
    loop {
        match rx.try_recv() {
            Ok(SyncEvent::Done(_new_issues, viewer)) => {
                // Update the header identity when the sync thread fetched it
                // (authentication happened outside the L-key login flow).
                if let Some(v) = viewer {
                    app.viewer_name = Some(v.name);
                    app.org_name = Some(v.org_name);
                    app.session.not_authenticated = false;
                }
                // Sync finished: refresh the issue list from SQLite so that
                // has_next_page and end_cursor are recalculated correctly.
                // Only refresh if the user is in normal list mode on page 1.
                if matches!(app.mode, Mode::List)
                    && app.pagination.cursor_stack.is_empty()
                    && app.pagination.current_cursor.is_none()
                {
                    app.do_fetch(false);
                }
                app.sync.syncing = false;
                app.sync.sync_status_label = build_sync_status_label(false);
                // Schedule next periodic delta sync in 30s.
                app.sync.next_sync_at = Some(Instant::now() + Duration::from_secs(30));
                got_event = true;
            }
            Ok(SyncEvent::Error(msg)) => {
                app.sync.syncing = false;
                app.sync.sync_status_label = format!("sync error: {msg}");
                if matches!(app.status, Status::Loading) {
                    app.status = Status::Idle;
                }
                // Retry periodic sync in 30s even after errors.
                app.sync.next_sync_at = Some(Instant::now() + Duration::from_secs(30));
                got_event = true;
            }
            Ok(SyncEvent::NotAuthenticated) => {
                app.sync.syncing = false;
                app.session.not_authenticated = true;
                app.sync.sync_status_label = "not authenticated -- press L to log in".to_string();
                if matches!(app.status, Status::Loading) {
                    app.status = Status::Idle;
                }
                // Don't schedule periodic sync when not authenticated.
                app.sync.next_sync_at = None;
                got_event = true;
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                app.sync.syncing = false;
                if app.sync.sync_status_label == "syncing..." {
                    app.sync.sync_status_label = build_sync_status_label(false);
                }
                got_event = true;
                break;
            }
        }
    }

    // Put the receiver back if the thread may still send more messages.
    if !got_event || app.sync.syncing {
        app.sync.sync_rx = Some(rx);
    }
}

// -- New-issue modal key handler (bd-l6r) ------------------------------------

fn handle_new_issue_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    let alt = modifiers.contains(KeyModifiers::ALT);

    // Ctrl-Enter submits the form (Alt-Enter on terminals that cannot
    // distinguish Ctrl-Enter from Enter).
    if (ctrl || alt) && code == KeyCode::Enter {
        app.new_issue_submit();
        return;
    }

    // Esc cancels.
    if code == KeyCode::Esc {
        app.mode = Mode::List;
        app.new_issue_modal = None;
        return;
    }

    let Some(modal) = app.new_issue_modal.as_mut() else {
        return;
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
        NewIssueField::Description => handle_description_key(modal, code, ctrl),
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

/// Handle a key press while the new-issue Description field is focused.
fn handle_description_key(modal: &mut NewIssueModal, code: KeyCode, ctrl: bool) {
    match code {
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

    // When the comment input is open, all keys go to it.
    if app.comment_input.is_some() {
        handle_comment_input_key(app, code, modifiers);
        return;
    }

    match code {
        KeyCode::Esc | KeyCode::Char('q') => app.close_detail(),
        // Open the comment input.
        KeyCode::Char('c') => {
            app.comment_input = Some(String::new());
            app.footer_msg = None;
        }
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

/// Key handling for the comment input box (same editing model as the
/// new-issue description field: cursor always at the end).
fn handle_comment_input_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);

    // Ctrl-Enter submits (Alt-Enter on terminals that cannot distinguish
    // Ctrl-Enter from Enter).
    if (ctrl || alt) && code == KeyCode::Enter {
        app.submit_comment();
        return;
    }
    // Esc cancels.
    if code == KeyCode::Esc {
        app.comment_input = None;
        return;
    }

    let Some(buf) = app.comment_input.as_mut() else {
        return;
    };
    match code {
        KeyCode::Enter => buf.push('\n'),
        KeyCode::Backspace => {
            buf.pop();
        }
        KeyCode::Char('h') if ctrl => {
            buf.pop();
        }
        KeyCode::Char('w') if ctrl => {
            let trimmed = buf.trim_end_matches(|c: char| !c.is_whitespace());
            let new_end = trimmed.trim_end().len();
            buf.truncate(new_end);
        }
        KeyCode::Char('u') if ctrl => buf.clear(),
        KeyCode::Char(c) if !ctrl => buf.push(c),
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
        KeyCode::Char('L') if app.login_rx.is_none() => {
            app.login_rx = Some(spawn_login_thread());
            app.sync.sync_status_label =
                "logging in -- complete authorization in browser".to_string();
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
        KeyCode::Enter => confirm_search(app),
        // Result-list navigation: <down>/<up> only. Plain j/k must fall
        // through to the query bar so they can be typed as filter text.
        KeyCode::Down => {
            if let Some(ref mut overlay) = app.search_overlay {
                overlay.move_down();
            }
        }
        KeyCode::Up => {
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
        KeyCode::Tab => apply_completion_tab(app, true),
        KeyCode::BackTab => apply_completion_tab(app, false),
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

/// Confirm the search: leave search mode with the filtered results visible by
/// transferring them into `app.issues` so normal keybindings work.
fn confirm_search(app: &mut App) {
    if let Some(ref mut overlay) = app.search_overlay {
        // Flush any pending debounce so the AST and results reflect every
        // character the user typed before hitting Enter (bd-3r1).
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

/// Apply stem-key completion in the given direction (Tab forward, Shift-Tab
/// backward) and re-parse the query AST.
fn apply_completion_tab(app: &mut App, forward: bool) {
    if let Some(ref mut overlay) = app.search_overlay {
        let ast_snapshot = search_query::parse_query_ast(&overlay.query.value);
        overlay
            .completer
            .apply_tab(&mut overlay.query, &ast_snapshot, forward);
        let new_raw = overlay.query.value.clone();
        overlay.ast = search_query::parse_query_ast(&new_raw);
        overlay.completer.update(&overlay.ast, overlay.query.cursor);
        overlay.last_changed = Some(Instant::now());
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

// ---------------------------------------------------------------------------
// Rendering tests (docs/design/visual-rendering-tests.md)
//
// These drive `ui::render` into a ratatui `TestBackend` and snapshot the
// resulting buffer with `insta`. They populate `App` state directly via
// `App::for_test` and skip the DB/thread action methods, so no DB, network, or
// profile global is touched. Data comes from the deterministic `sim` generator,
// so the module is gated on `feature = "sim"`.
// ---------------------------------------------------------------------------
#[cfg(all(test, feature = "sim"))]
mod render_tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    /// Convert a seeded `sim` dataset into the list issues the TUI renders.
    fn sim_issues(seed: u64, size: usize) -> Vec<Issue> {
        crate::sim::generate(seed, size)
            .issues
            .into_iter()
            .map(db_issue_to_list_issue)
            .collect()
    }

    /// Draw one frame at `w`x`h` and return the rendered buffer as text.
    fn draw(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| ui::render(f, app)).unwrap();
        format!("{}", term.backend())
    }

    /// An `App` seeded with sim issues and a fixed identity for a stable header.
    fn app_with_issues(seed: u64, size: usize) -> App {
        let mut app = App::for_test(sim_issues(seed, size));
        app.viewer_name = Some("Ada Lovelace".to_string());
        app.org_name = Some("Acme".to_string());
        app
    }

    fn item(label: &str, id: Option<&str>) -> PopupItem {
        PopupItem {
            label: label.to_string(),
            id: id.map(ToString::to_string),
        }
    }

    #[test]
    fn list_navigation_clamps_within_bounds() {
        let mut app = app_with_issues(0, 10);
        app.viewport_height = 4;
        app.table_state.select(Some(0));

        app.move_down();
        assert_eq!(app.table_state.selected(), Some(1));
        app.move_up();
        assert_eq!(app.table_state.selected(), Some(0));
        app.move_up(); // clamp at top
        assert_eq!(app.table_state.selected(), Some(0));
        app.move_bottom();
        assert_eq!(app.table_state.selected(), Some(9));
        app.move_top();
        assert_eq!(app.table_state.selected(), Some(0));
        app.page_down(); // +viewport (4)
        assert_eq!(app.table_state.selected(), Some(4));
        app.half_page_up(); // -2
        assert_eq!(app.table_state.selected(), Some(2));
        app.page_up(); // clamp at top
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn navigation_on_empty_list_is_noop() {
        let mut app = App::for_test(Vec::new());
        app.move_down();
        app.move_bottom();
        assert_eq!(app.table_state.selected(), None);
    }

    #[test]
    fn apply_fetched_selection_resets_or_clamps() {
        let mut app = app_with_issues(0, 3);
        app.table_state.select(Some(2));
        app.apply_fetched_selection(true); // reset
        assert_eq!(app.table_state.selected(), Some(0));

        app.table_state.select(Some(2));
        app.issues.truncate(1); // selection now out of range
        app.apply_fetched_selection(false); // clamp
        assert_eq!(app.table_state.selected(), Some(0));

        app.issues.clear();
        app.apply_fetched_selection(false);
        assert_eq!(app.table_state.selected(), None);
    }

    #[test]
    fn detail_scroll_saturates() {
        let mut app = app_with_issues(0, 1);
        app.viewport_height = 10;
        app.detail_scroll_down();
        assert_eq!(app.detail_scroll, 1);
        app.detail_scroll_up();
        app.detail_scroll_up(); // saturate at 0
        assert_eq!(app.detail_scroll, 0);
        app.detail_scroll_to_bottom();
        assert_eq!(app.detail_scroll, u16::MAX);
        app.detail_scroll_to_top();
        assert_eq!(app.detail_scroll, 0);
        app.detail_scroll_half_page_down(); // +5
        assert_eq!(app.detail_scroll, 5);
        app.detail_scroll_page_up(); // -10, saturating
        assert_eq!(app.detail_scroll, 0);
    }

    #[test]
    fn popup_move_clamps_and_cancel_resets_mode() {
        let mut app = app_with_issues(0, 1);
        app.popup_items = vec![item("a", None), item("b", None), item("c", None)];
        app.popup_selected = 0;
        app.popup_move(1);
        assert_eq!(app.popup_selected, 1);
        app.popup_move(5); // clamp at last
        assert_eq!(app.popup_selected, 2);
        app.popup_move(-10); // clamp at first
        assert_eq!(app.popup_selected, 0);

        app.mode = Mode::Popup(PopupKind::Priority);
        app.popup_anchor = Some(ratatui::layout::Rect::new(0, 0, 1, 1));
        app.popup_cancel();
        assert!(matches!(app.mode, Mode::List));
        assert!(app.popup_anchor.is_none());
    }

    #[test]
    fn close_detail_clears_pane_state() {
        let mut app = app_with_issues(0, 1);
        let issue = app.issues[0].clone();
        app.mode = Mode::Detail;
        app.detail = Some(build_cached_detail(&issue, Vec::new()));
        app.detail_scroll = 5;
        app.comment_input = Some("draft".to_string());
        app.close_detail();
        assert!(matches!(app.mode, Mode::List));
        assert!(app.detail.is_none());
        assert_eq!(app.detail_scroll, 0);
        assert!(app.comment_input.is_none());
    }

    #[test]
    fn filter_sort_sync_and_replacement() {
        let mut app = app_with_issues(0, 1);
        app.active_filter = search_query::parse_query_ast("sort:title+");
        app.sync_args_from_filter();
        assert!(matches!(app.args.sort, crate::issues::SortField::Title));
        assert!(!app.args.desc);

        // replace_sort_in_filter rewrites the sort token, preserving other stems.
        app.args.sort = crate::issues::SortField::Updated;
        app.args.desc = true;
        app.active_filter = search_query::parse_query_ast("state:todo sort:title+");
        let replaced = app.replace_sort_in_filter();
        let parsed = search_query::ParsedQuery::from(&replaced);
        assert_eq!(
            parsed.sort.map(|(_, d)| d),
            Some(search_query::SortDir::Desc)
        );
        assert_eq!(parsed.state.as_deref(), Some("todo"));
    }

    #[test]
    fn new_issue_field_cycles_both_directions() {
        use NewIssueField::{Assignee, Description, Priority, State, Team, Title};
        assert!(matches!(Title.next(), Team));
        assert!(matches!(Description.next(), Title)); // wraps
        assert!(matches!(State.prev(), Priority));
        assert!(matches!(Title.prev(), Title)); // clamps
        assert!(matches!(Assignee.prev(), State));
    }

    #[test]
    fn priority_label_to_u8_maps_levels() {
        assert_eq!(priority_label_to_u8("Urgent"), 1);
        assert_eq!(priority_label_to_u8("high"), 2);
        assert_eq!(priority_label_to_u8("normal"), 3);
        assert_eq!(priority_label_to_u8("medium"), 3);
        assert_eq!(priority_label_to_u8("low"), 4);
        assert_eq!(priority_label_to_u8("No priority"), 0);
    }

    #[test]
    fn db_to_api_and_list_conversions() {
        let comment = crate::db::Comment {
            id: "c1".to_string(),
            issue_id: "i1".to_string(),
            body: "hi".to_string(),
            author_name: Some("Alice".to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            synced_at: String::new(),
        };
        let api = db_comment_to_api(comment);
        assert_eq!(api.author(), "Alice");

        let mut row = crate::db::Issue {
            id: "1".to_string(),
            identifier: "ENG-1".to_string(),
            title: "t".to_string(),
            priority_label: "High".to_string(),
            state_name: "Todo".to_string(),
            assignee_name: Some("Bob".to_string()),
            team_name: "Eng".to_string(),
            team_key: Some("ENG".to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            synced_at: String::new(),
            description: Some("d".to_string()),
            labels: "bug,backend".to_string(),
            project_name: None,
            cycle_name: None,
            creator_name: None,
            parent_id: Some("9".to_string()),
            parent_identifier: Some("ENG-9".to_string()),
        };
        let listed = db_issue_to_list_issue(row.clone());
        assert_eq!(listed.priority, 2);
        assert_eq!(listed.labels.nodes.len(), 2);
        assert_eq!(
            listed.parent.as_ref().map(|p| p.identifier.as_str()),
            Some("ENG-9")
        );

        // Empty labels string yields no label nodes.
        row.labels = String::new();
        assert!(db_issue_to_list_issue(row).labels.nodes.is_empty());
    }

    #[test]
    fn optimistic_builders_apply_popup_choice() {
        let mut app = app_with_issues(0, 1);
        let issue = app.issues[0].clone();

        let db =
            build_db_issue_optimistic(&issue, &PopupKind::Priority, &item("Urgent", Some("1")));
        assert_eq!(db.priority_label, "Urgent");
        let unassigned = build_db_issue_optimistic(&issue, &PopupKind::Assignee, &item("x", None));
        assert!(unassigned.assignee_name.is_none());

        app.table_state.select(Some(0));
        apply_optimistic_in_memory(&mut app, &PopupKind::Priority, &item("Urgent", Some("1")));
        assert_eq!(app.issues[0].priority_label, "Urgent");
        assert_eq!(app.issues[0].priority, 1);
        apply_optimistic_in_memory(&mut app, &PopupKind::Assignee, &item("none", None));
        assert!(app.issues[0].assignee.is_none());
    }

    #[test]
    fn assignee_items_put_me_first_and_skip_viewer() {
        let viewer = crate::linear::viewer::Viewer {
            id: "v".to_string(),
            name: "Vic".to_string(),
            org_name: "Acme".to_string(),
        };
        let members = || {
            vec![
                Member {
                    id: "v".to_string(),
                    name: "Vic".to_string(),
                },
                Member {
                    id: "m".to_string(),
                    name: "Mara".to_string(),
                },
            ]
        };
        let with_viewer = build_assignee_items(Some(&viewer), members());
        let labels: Vec<&str> = with_viewer.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["Me (Vic)", "Unassigned", "Mara"]);

        let no_viewer = build_assignee_items(None, members());
        let labels: Vec<&str> = no_viewer.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["Unassigned", "Vic", "Mara"]);
    }

    #[test]
    fn list_view() {
        let mut app = app_with_issues(0, 12);
        insta::assert_snapshot!(draw(&mut app, 100, 20));
    }

    #[test]
    fn empty_list() {
        let mut app = App::for_test(Vec::new());
        app.viewer_name = Some("Ada Lovelace".to_string());
        app.org_name = Some("Acme".to_string());
        insta::assert_snapshot!(draw(&mut app, 80, 10));
    }

    #[test]
    fn detail_overlay() {
        let mut app = app_with_issues(0, 12);
        let issue = app.issues[0].clone();
        app.detail = Some(build_cached_detail(&issue, Vec::new()));
        app.mode = Mode::Detail;
        insta::assert_snapshot!(draw(&mut app, 100, 24));
    }

    #[test]
    fn priority_popup() {
        let mut app = app_with_issues(0, 12);
        app.popup_items = priority_popup_items();
        app.popup_selected = 1;
        app.mode = Mode::Popup(PopupKind::Priority);
        insta::assert_snapshot!(draw(&mut app, 100, 20));
    }

    #[test]
    fn search_overlay() {
        let mut app = app_with_issues(0, 12);
        let mut overlay = SearchOverlay::new();
        overlay.results = sim_issues(0, 12);
        overlay.has_searched = true;
        overlay.table_state.select(Some(0));
        app.search_overlay = Some(overlay);
        app.mode = Mode::Search;
        insta::assert_snapshot!(draw(&mut app, 100, 20));
    }

    #[test]
    fn help_popup() {
        let mut app = app_with_issues(0, 12);
        app.help_popup = Some(HelpPopup::new());
        app.mode = Mode::Help;
        insta::assert_snapshot!(draw(&mut app, 100, 24));
    }

    #[test]
    fn new_issue_modal() {
        let mut app = app_with_issues(0, 12);
        app.new_issue_modal = Some(NewIssueModal {
            focused_field: NewIssueField::Title,
            title: TextInput::from_string("Fix the renderer".to_string()),
            description: "Some description.".to_string(),
            teams: vec![PopupItem {
                label: "Engineering".to_string(),
                id: Some("ENG".to_string()),
            }],
            team_selected: 0,
            priorities: priority_popup_items(),
            priority_selected: 0,
            states: vec![PopupItem {
                label: "Todo".to_string(),
                id: Some("s1".to_string()),
            }],
            state_selected: 0,
            assignees: vec![PopupItem {
                label: "Ada Lovelace".to_string(),
                id: Some("u1".to_string()),
            }],
            assignee_selected: 0,
            loading: false,
            error: String::new(),
            modal_rx: None,
        });
        app.mode = Mode::NewIssue;
        insta::assert_snapshot!(draw(&mut app, 100, 30));
    }
}
