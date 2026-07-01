use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::widgets::TableState;

use lt_storage::search_query;

use super::search_completer::Completer;
use super::{ALL_KEYBINDINGS, App, Mode, TextInput};

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
pub(crate) fn priority_popup_items() -> Vec<PopupItem> {
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

// ---------------------------------------------------------------------------
// Help popup state
// ---------------------------------------------------------------------------

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
// FTS search overlay state
// ---------------------------------------------------------------------------

/// Mutable state for the FTS search overlay.
pub struct SearchOverlay {
    /// Current query typed by the user.
    pub query: TextInput,
    /// Issues returned by the last FTS query.
    pub results: Vec<lt_types::types::Issue>,
    /// Table selection state for the results list.
    pub table_state: TableState,
    /// When the query was last modified (used for 150ms debounce).
    pub last_changed: Option<Instant>,
    /// True when FTS index is unavailable (no sync yet).
    pub fts_unavailable: bool,
    /// True once `run_search()` has been called at least once.
    /// Used by the renderer to distinguish "never searched" from "searched, no results".
    pub has_searched: bool,
    /// Parsed AST of the current query string.
    pub ast: search_query::QueryAst,
    /// Tab-completion state.
    pub completer: Completer,
}

impl SearchOverlay {
    pub fn new() -> Self {
        // Pre-populate the query bar with the default sort stem.
        let default_q = search_query::DEFAULT_QUERY.to_string();
        let ast = search_query::parse_query_ast(&default_q);
        let query = TextInput::from(default_q);
        let mut completer = Completer::new();
        // Initialize completer so ghost text and Tab work immediately.
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

    /// Run the structured search query and refresh results.
    ///
    /// The query string is parsed into stems (sort:, assignee:, priority:,
    /// state:, team:) plus optional free-text FTS terms.  The default query
    /// is `sort:updated-` which shows all issues sorted by updated desc.
    ///
    /// `viewport_rows` is the number of visible rows in the content area
    /// (excluding the table header).  The result set is capped at this value
    /// so that the search overlay never grows taller than the normal list
    ///.
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
        match lt_storage::db::db_path()
            .and_then(lt_storage::db::open_db)
            .and_then(|conn| search_query::run_query(&conn, &parsed, limit))
        {
            Ok(issues) => {
                self.results = issues;
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
                // the user is still typing.
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
// Popup open/move/confirm methods
// ---------------------------------------------------------------------------

impl super::App {
    pub(crate) fn open_state_popup(&mut self) {
        let issue = match self.selected_issue() {
            Some(i) => i.clone(),
            None => return,
        };
        let current_state_name = issue.state.name.clone();
        match self.service.fetch_workflow_states(&issue.team.id) {
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

    pub(crate) fn open_priority_popup(&mut self) {
        let Some(priority) = self.selected_issue().map(|i| i.priority) else {
            return;
        };
        // Linear priority: 0=No priority, 1=Urgent, 2=High, 3=Normal, 4=Low
        self.popup_items = priority_popup_items();
        self.popup_selected = priority as usize;
        self.mode = Mode::Popup(PopupKind::Priority);
        self.footer_msg = None;
    }

    pub(crate) fn open_assignee_popup(&mut self) {
        let issue = match self.selected_issue() {
            Some(i) => i.clone(),
            None => return,
        };
        let mut items: Vec<PopupItem> = vec![PopupItem {
            label: "Unassign".to_string(),
            id: None,
        }];
        match self.service.fetch_team_members(&issue.team.id) {
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

    pub(crate) fn popup_move(&mut self, delta: i32) {
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

        // 1. Enqueue local intent (overlay + outbox) in one transaction. No
        //    network: the sync drainer replays it. The read model merges the
        //    overlay, so the change renders without a base write.
        enqueue_edit(&issue.id, &kind, &item);

        // 2. Update the in-memory issue list for instant feedback before the
        //    next DB-backed fetch reloads the merged read model.
        apply_optimistic_in_memory(self, &kind, &item);

        self.mode = Mode::List;
        self.popup_anchor = None;
    }

    pub(crate) fn popup_cancel(&mut self) {
        self.mode = Mode::List;
        self.popup_anchor = None;
    }
}

// ---------------------------------------------------------------------------
// Optimistic SQLite helpers
// ---------------------------------------------------------------------------

/// Enqueue a popup edit as local intent: the matching overlay row plus the
/// coalesced `issueUpdate` outbox command, in one transaction. Unset choices
/// (a priority/state item with no id) are no-ops; an assignee item with no id
/// clears the assignee.
fn enqueue_edit(issue_id: &str, kind: &PopupKind, item: &PopupItem) {
    use lt_storage::db::outbox::{
        enqueue_assignee_change, enqueue_priority_change, enqueue_state_change,
    };
    let Ok(conn) = lt_storage::db::db_path().and_then(lt_storage::db::open_db) else {
        return;
    };
    let _ = match kind {
        PopupKind::State => match &item.id {
            Some(id) => enqueue_state_change(&conn, issue_id, id, &item.label),
            None => Ok(()),
        },
        PopupKind::Priority => match item.id.as_deref().and_then(|s| s.parse::<u8>().ok()) {
            Some(p) => enqueue_priority_change(&conn, issue_id, p),
            None => Ok(()),
        },
        PopupKind::Assignee => enqueue_assignee_change(
            &conn,
            issue_id,
            item.id.as_deref().map(|id| (id, item.label.as_str())),
        ),
    };
}

/// Apply a popup choice to an issue fragment in place. Shared by the in-memory
/// update and the optimistic DB write so they never diverge.
fn apply_change(issue: &mut lt_types::types::Issue, kind: &PopupKind, item: &PopupItem) {
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
                issue.assignee = Some(lt_types::types::User {
                    id: item.id.clone().unwrap_or_default(),
                    name: item.label.clone(),
                });
            }
        }
    }
}

/// The optimistic issue fragment a popup choice produces: the selected issue
/// with the chosen field applied. Used by the render tests to exercise the
/// applied-change shape; the live write path applies the change in-memory via
/// [`apply_optimistic_in_memory`].
#[cfg(all(test, feature = "sim"))]
pub(crate) fn build_optimistic_issue(
    issue: &lt_types::types::Issue,
    kind: &PopupKind,
    item: &PopupItem,
) -> lt_types::types::Issue {
    let mut updated = issue.clone();
    apply_change(&mut updated, kind, item);
    updated
}

pub(crate) fn apply_optimistic_in_memory(app: &mut App, kind: &PopupKind, item: &PopupItem) {
    if let Some(issue) = app.selected_issue_mut() {
        apply_change(issue, kind, item);
    }
}

// ---------------------------------------------------------------------------
// Key handlers
// ---------------------------------------------------------------------------

// -- Popup key handler ----------------------------------------------

pub(crate) fn handle_popup_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('j') | KeyCode::Down => app.popup_move(1),
        KeyCode::Char('k') | KeyCode::Up => app.popup_move(-1),
        KeyCode::Enter => app.popup_confirm(),
        KeyCode::Esc => app.popup_cancel(),
        _ => {}
    }
}

// -- Help popup key handler -----------------------------------------

pub(crate) fn handle_help_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
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

// -- FTS search overlay key handler --------------------------------

pub(crate) fn handle_search_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
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
                overlay.query = TextInput::from(search_query::DEFAULT_QUERY.to_string());
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
        // Tab / Shift-Tab: apply stem-key completion.
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
        // character the user typed before hitting Enter.
        if overlay.last_changed.is_some() {
            overlay.last_changed = None;
            overlay.run_search(app.viewport_height, app.args.limit as usize);
        }
        let results = std::mem::take(&mut overlay.results);
        let selected = overlay.table_state.selected();
        // AST is the single source of truth.
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
pub(crate) fn poll_search_debounce(app: &mut App) {
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
