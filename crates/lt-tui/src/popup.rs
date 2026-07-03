use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lt_runtime::db::Connection;
use lt_runtime::search_query;
use ratatui::widgets::TableState;

use super::search_completer::Completer;
use super::{ALL_KEYBINDINGS, App, KeyFlow, StateCtx, StateEvent, TextInput, View};

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

/// State/priority/assignee picker: the popup's items plus the target
/// captured at open.
pub struct PopupView {
    pub kind: PopupKind,
    /// Target issue id, captured at open; confirm no longer depends on the
    /// list selection being unchanged.
    pub issue_id: String,
    /// The issue's team -- the scope key for the `Team{team_id}` refresh
    /// (state and assignee popups; `None` for the static priority popup).
    pub team_id: Option<String>,
    pub items: Vec<PopupItem>,
    pub selected: usize,
    /// Written by the renderer when this popup sits directly on the base
    /// table; `None` => `render_popup` centers.
    pub anchor: Option<ratatui::layout::Rect>,
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
    ///. Reads through `db` rather than resolving `db_path()` directly, so
    /// tests that install an in-memory database are honored.
    pub fn run_search(
        &mut self,
        db: &lt_runtime::db::Database,
        viewport_rows: u16,
        list_limit: usize,
    ) {
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
        match db
            .connect()
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

/// A team's workflow states, from the local cache only. Shared by
/// `open_state_popup`/`PopupView::consume`'s `State` arm and the new-issue
/// modal's own state picker.
pub(crate) fn state_items(conn: &Connection, team_id: &str) -> Vec<PopupItem> {
    lt_runtime::db::query_team_states(conn, team_id)
        .unwrap_or_default()
        .into_iter()
        .map(|s| PopupItem {
            label: s.name,
            id: Some(s.id.into_inner()),
        })
        .collect()
}

/// The assignee popup's items -- "Unassign" plus a team's members, from the
/// local cache only. Shared by `open_assignee_popup` and `PopupView::consume`'s
/// `Assignee` arm.
fn assignee_popup_items(conn: &Connection, team_id: &str) -> Vec<PopupItem> {
    let mut items: Vec<PopupItem> = vec![PopupItem {
        label: "Unassign".to_string(),
        id: None,
    }];
    if let Ok(members) = lt_runtime::db::query_team_members(conn, team_id) {
        items.extend(members.into_iter().map(|m| PopupItem {
            label: m.name,
            id: Some(m.id.into_inner()),
        }));
    }
    items
}

impl super::App {
    pub(crate) fn open_state_popup(&mut self) {
        let Some(issue) = self.selected_issue() else {
            return;
        };
        let issue_id = issue.id.inner().to_string();
        let team_id = issue.team.id.inner().to_string();
        let current_state_name = issue.state.name.clone();

        let items = self
            .db
            .connect()
            .map(|conn| state_items(&conn, &team_id))
            .unwrap_or_default();
        let selected = items
            .iter()
            .position(|item| item.label == current_state_name)
            .unwrap_or(0);
        self.views.push(View::Popup(PopupView {
            kind: PopupKind::State,
            issue_id,
            team_id: Some(team_id.clone()),
            items,
            selected,
            anchor: None,
        }));
        self.footer_msg = None;
        self.spawn_state_refresh(
            StateEvent::Team {
                team_id: team_id.clone(),
            },
            move |s| s.sync_team_data(&team_id),
        );
    }

    pub(crate) fn open_priority_popup(&mut self) {
        let Some(issue) = self.selected_issue() else {
            return;
        };
        let issue_id = issue.id.inner().to_string();
        // Linear priority: 0=No priority, 1=Urgent, 2=High, 3=Normal, 4=Low
        let selected = usize::from(issue.priority.0);
        self.views.push(View::Popup(PopupView {
            kind: PopupKind::Priority,
            issue_id,
            team_id: None,
            items: priority_popup_items(),
            selected,
            anchor: None,
        }));
        self.footer_msg = None;
    }

    pub(crate) fn open_assignee_popup(&mut self) {
        let Some(issue) = self.selected_issue() else {
            return;
        };
        let issue_id = issue.id.inner().to_string();
        let team_id = issue.team.id.inner().to_string();
        let current_assignee = issue.assignee.as_ref().map(|a| a.id.inner().to_string());

        let items = self
            .db
            .connect()
            .map(|conn| assignee_popup_items(&conn, &team_id))
            .unwrap_or_default();
        let selected = current_assignee
            .and_then(|a| {
                items
                    .iter()
                    .position(|item| item.id.as_deref() == Some(a.as_str()))
            })
            .unwrap_or(0);
        self.views.push(View::Popup(PopupView {
            kind: PopupKind::Assignee,
            issue_id,
            team_id: Some(team_id.clone()),
            items,
            selected,
            anchor: None,
        }));
        self.footer_msg = None;
        self.spawn_state_refresh(
            StateEvent::Team {
                team_id: team_id.clone(),
            },
            move |s| s.sync_team_data(&team_id),
        );
    }
}

impl PopupView {
    /// The state and assignee popups' subscription: a matching
    /// `Team{team_id}` rebuilds `items` from the cache and re-anchors the
    /// selection by item id. The priority popup is static (`team_id: None`)
    /// and never matches.
    pub(crate) fn consume(&mut self, ctx: &StateCtx, _focused: bool, ev: &StateEvent) {
        let StateEvent::Team { team_id } = ev else {
            return;
        };
        if self.team_id.as_deref() != Some(team_id.as_str()) {
            return;
        }
        let Ok(conn) = ctx.db.connect() else {
            return;
        };
        let items = match &self.kind {
            PopupKind::State => state_items(&conn, team_id),
            PopupKind::Assignee => assignee_popup_items(&conn, team_id),
            PopupKind::Priority => return,
        };
        let current_id = self.items.get(self.selected).and_then(|i| i.id.clone());
        self.items = items;
        self.selected = self
            .items
            .iter()
            .position(|i| i.id.as_deref() == current_id.as_deref())
            .unwrap_or(0);
    }
}

fn popup_view_mut(app: &mut App, i: usize) -> Option<&mut PopupView> {
    app.view_at_mut(i, |v| match v {
        View::Popup(p) => Some(p),
        _ => None,
    })
}

fn popup_move(app: &mut App, i: usize, delta: i32) {
    let Some(popup) = popup_view_mut(app, i) else {
        return;
    };
    let n = popup.items.len();
    if n == 0 {
        return;
    }
    let step = usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX);
    popup.selected = if delta >= 0 {
        popup.selected.saturating_add(step).min(n - 1)
    } else {
        popup.selected.saturating_sub(step)
    };
}

/// Confirm the popup choice: pop it, enqueue the edit against the issue it
/// was opened for (its captured `issue_id`, not the current list selection),
/// then route the resulting `Issues` invalidation -- a function call, not a
/// channel round-trip: same frame, zero latency, one code path with the
/// async completions.
fn popup_confirm(app: &mut App, i: usize) {
    let Some(View::Popup(popup)) = app.views.get(i) else {
        return;
    };
    let Some(item) = popup.items.get(popup.selected).cloned() else {
        return;
    };
    let issue_id = popup.issue_id.clone();
    let kind = popup.kind.clone();
    app.pop_view();
    enqueue_edit(&app.db, &issue_id, &kind, &item);
    app.route_state_event(&StateEvent::Issues);
}

fn popup_cancel(app: &mut App) {
    app.pop_view();
}

// ---------------------------------------------------------------------------
// Optimistic SQLite helpers
// ---------------------------------------------------------------------------

/// Enqueue a popup edit as local intent: the matching overlay row plus the
/// coalesced `issueUpdate` outbox command, in one transaction. Unset choices
/// (a priority/state item with no id) are no-ops; an assignee item with no id
/// clears the assignee. Writes through `db` (rather than resolving
/// `db_path()` directly) so the write lands on the same connection
/// `route_state_event`'s re-read uses -- in production both resolve to the
/// same profile file; tests install an in-memory database instead.
fn enqueue_edit(db: &lt_runtime::db::Database, issue_id: &str, kind: &PopupKind, item: &PopupItem) {
    use lt_runtime::db::outbox::{
        enqueue_assignee_change, enqueue_priority_change, enqueue_state_change,
    };
    let Ok(conn) = db.connect() else {
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

// ---------------------------------------------------------------------------
// Key handlers
// ---------------------------------------------------------------------------

// -- Popup key handler ----------------------------------------------

pub(crate) fn handle_key(app: &mut App, i: usize, key: KeyEvent) -> KeyFlow {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => popup_move(app, i, 1),
        KeyCode::Char('k') | KeyCode::Up => popup_move(app, i, -1),
        KeyCode::Enter => popup_confirm(app, i),
        KeyCode::Esc => popup_cancel(app),
        _ => {}
    }
    KeyFlow::Consumed
}

// -- Help popup key handler -----------------------------------------

pub(crate) fn handle_help_key(app: &mut App, i: usize, key: KeyEvent) -> KeyFlow {
    let code = key.code;
    let modifiers = key.modifiers;
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Esc => app.pop_view(),
        // Navigation: j/k/<down>/<up> move the filtered list.
        KeyCode::Down | KeyCode::Char('j') if !ctrl => {
            if let Some(View::Help(popup)) = app.views.get_mut(i) {
                let max = popup.filtered.len().saturating_sub(1);
                if popup.selected < max {
                    popup.selected += 1;
                }
            }
        }
        KeyCode::Up | KeyCode::Char('k') if !ctrl => {
            if let Some(View::Help(popup)) = app.views.get_mut(i) {
                popup.selected = popup.selected.saturating_sub(1);
            }
        }
        // Everything else goes to the TextInput search bar.
        _ => {
            if let Some(View::Help(popup)) = app.views.get_mut(i)
                && popup.search.handle_key(code, modifiers)
            {
                popup.update_filter();
            }
        }
    }
    KeyFlow::Consumed
}

// -- FTS search overlay key handler --------------------------------

pub(crate) fn handle_search_key(app: &mut App, i: usize, key: KeyEvent) -> KeyFlow {
    let code = key.code;
    let modifiers = key.modifiers;
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        // Esc exits the search overlay and returns to the full list (go back).
        KeyCode::Esc => app.pop_view(),
        KeyCode::Char('c') if ctrl => {
            // Ctrl+C resets the search query back to the default.
            if let Some(View::Search(overlay)) = app.views.get_mut(i) {
                overlay.query = TextInput::from(search_query::DEFAULT_QUERY.to_string());
                overlay.last_changed = Some(Instant::now());
            }
        }
        KeyCode::Enter => confirm_search(app),
        // Result-list navigation: <down>/<up> only. Plain j/k must fall
        // through to the query bar so they can be typed as filter text.
        KeyCode::Down => {
            if let Some(View::Search(overlay)) = app.views.get_mut(i) {
                overlay.move_down();
            }
        }
        KeyCode::Up => {
            if let Some(View::Search(overlay)) = app.views.get_mut(i) {
                overlay.move_up();
            }
        }
        // Ctrl+N -- cycle completion forward.
        KeyCode::Char('n') if ctrl => {
            if let Some(View::Search(overlay)) = app.views.get_mut(i) {
                overlay.completer.cycle_next();
            }
        }
        // Ctrl+P -- cycle completion backward.
        KeyCode::Char('p') if ctrl => {
            if let Some(View::Search(overlay)) = app.views.get_mut(i) {
                overlay.completer.cycle_prev();
            }
        }
        // Ctrl+Y -- accept the highlighted completion candidate.
        KeyCode::Char('y') if ctrl => {
            if let Some(View::Search(overlay)) = app.views.get_mut(i) {
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
        KeyCode::Tab => apply_completion_tab(app, i, true),
        KeyCode::BackTab => apply_completion_tab(app, i, false),
        // Everything else goes to the TextInput query bar.
        _ => {
            if let Some(View::Search(overlay)) = app.views.get_mut(i)
                && overlay.query.handle_key(code, modifiers)
            {
                overlay.last_changed = Some(Instant::now());
            }
        }
    }
    KeyFlow::Consumed
}

/// Confirm the search: pop the overlay (the borrow requires it, and it
/// destroys the overlay anyway) and transfer its results into the base list
/// so normal keybindings work.
fn confirm_search(app: &mut App) {
    let Some(View::Search(mut overlay)) = app.views.pop() else {
        return;
    };
    // Flush any pending debounce so the AST and results reflect every
    // character the user typed before hitting Enter.
    if overlay.last_changed.is_some() {
        overlay.last_changed = None;
        overlay.run_search(&app.db, app.viewport_height, app.args.limit as usize);
    }
    let results = std::mem::take(&mut overlay.results);
    let selected = overlay.table_state.selected();
    // AST is the single source of truth.
    app.active_filter = overlay.ast.clone();
    app.sync_args_from_filter();
    if let Some(list) = app.base_list_mut() {
        list.issues = results;
        let n = list.issues.len();
        let sel = selected.unwrap_or(0).min(n.saturating_sub(1));
        list.table_state
            .select(if n > 0 { Some(sel) } else { None });
    }
}

/// Apply stem-key completion in the given direction (Tab forward, Shift-Tab
/// backward) and re-parse the query AST.
fn apply_completion_tab(app: &mut App, i: usize, forward: bool) {
    if let Some(View::Search(overlay)) = app.views.get_mut(i) {
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

/// Fire the FTS search when the debounce interval (150ms) has elapsed. The
/// search overlay is only ever the top of the stack, so `views.last` is the
/// live check; `viewport_height`/`args.limit` are copied out and `&app.db`
/// borrowed before the `views.last_mut()` borrow since `run_search` needs
/// all three simultaneously.
pub(crate) fn poll_search_debounce(app: &mut App) {
    let viewport_height = app.viewport_height;
    let limit = app.args.limit as usize;
    let db = &app.db;
    let should_search = matches!(
        app.views.last(),
        Some(View::Search(overlay))
            if overlay.last_changed.is_some_and(|t| t.elapsed() >= Duration::from_millis(150))
    );
    if should_search && let Some(View::Search(overlay)) = app.views.last_mut() {
        overlay.last_changed = None;
        overlay.run_search(db, viewport_height, limit);
    }
}
