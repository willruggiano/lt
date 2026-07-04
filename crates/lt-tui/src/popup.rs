use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use lt_runtime::db::Connection;
use lt_runtime::query::IssueQuery;
use lt_runtime::search_query;
use ratatui::widgets::TableState;

use super::search_completer::Completer;
use super::{App, Keymap, ScrollMotion, StateCtx, StateEvent, TextInput, Unbound, View, keymap};

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
    pub label: String,
    /// Opaque ID sent to the Linear API (state id, assignee id, etc.).
    /// `None` means "unassign" for the assignee popup.
    pub id: Option<String>,
}

impl From<lt_types::types::Team> for PopupItem {
    fn from(team: lt_types::types::Team) -> Self {
        Self {
            label: team.name,
            id: Some(team.id.into_inner()),
        }
    }
}

impl From<lt_types::types::WorkflowState> for PopupItem {
    fn from(state: lt_types::types::WorkflowState) -> Self {
        Self {
            label: state.name,
            id: Some(state.id.into_inner()),
        }
    }
}

impl From<lt_types::types::User> for PopupItem {
    fn from(user: lt_types::types::User) -> Self {
        Self {
            label: user.name,
            id: Some(user.id.into_inner()),
        }
    }
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
    pub search: TextInput,
    /// Built once at construction so help can't drift from the tables it
    /// reads.
    pub(crate) rows: Vec<keymap::HelpRow>,
    /// Indices into `rows` that match the current search.
    pub filtered: Vec<usize>,
    pub selected: usize,
    /// The three help-panel columns' max width across every row, computed
    /// once since `rows` is immutable after construction.
    pub(crate) key_col_width: usize,
    pub(crate) context_col_width: usize,
    pub(crate) label_col_width: usize,
}

impl HelpPopup {
    pub fn new() -> Self {
        let rows = keymap::help_rows(crate::HELP_CONTEXTS);
        let filtered = (0..rows.len()).collect();
        let key_col_width = rows
            .iter()
            .map(|r| r.binding_form.len())
            .max()
            .unwrap_or(10);
        let context_col_width = rows.iter().map(|r| r.context.len()).max().unwrap_or(6);
        let label_col_width = rows.iter().map(|r| r.label.len()).max().unwrap_or(10);
        Self {
            search: TextInput::new(),
            rows,
            filtered,
            selected: 0,
            key_col_width,
            context_col_width,
            label_col_width,
        }
    }

    /// Matches the query against each row's precomputed `haystack`,
    /// case-insensitive.
    pub fn update_filter(&mut self) {
        let q = self.search.value.to_lowercase();
        self.filtered = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| q.is_empty() || row.haystack.contains(&q))
            .map(|(i, _)| i)
            .collect();
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }
}

impl HelpPopup {
    /// Selection movement over the shared motion set.
    pub(crate) fn scroll(&mut self, motion: ScrollMotion, viewport_height: u16) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = motion.apply_index(self.selected, self.filtered.len(), viewport_height);
    }
}

// ---------------------------------------------------------------------------
// FTS search overlay state
// ---------------------------------------------------------------------------

/// Mutable state for the FTS search overlay.
pub struct SearchOverlay {
    pub query: TextInput,
    pub results: Vec<lt_types::types::Issue>,
    pub table_state: TableState,
    /// When the query was last modified (used for 150ms debounce).
    pub last_changed: Option<Instant>,
    /// True when FTS index is unavailable (no sync yet).
    pub fts_unavailable: bool,
    /// True once `run_search()` has been called at least once.
    pub has_searched: bool,
    pub ast: search_query::QueryAst,
    pub completer: Completer,
    /// The base list's query limit, captured once at open so both views
    /// show the same number of results; can't change while Search has
    /// focus.
    pub limit: u32,
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
            limit: IssueQuery::default().limit,
        }
    }

    /// Run the structured search query and refresh results, capped to
    /// `viewport_rows` so the overlay never grows taller than the list.
    /// Reads through `db` rather than resolving `db_path()` directly, so
    /// tests with an in-memory database are honored.
    pub fn run_search(&mut self, db: &lt_runtime::db::Database, viewport_rows: u16) {
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

        // Cap to the viewport height so we never render more rows than fit.
        let list_limit = self.limit as usize;
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
                // Only a genuine missing-index/table error marks FTS
                // unavailable; a syntax error from an incomplete stem token
                // must not, or the "run lt sync first" banner would flash
                // while the user is still typing.
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
}

impl SearchOverlay {
    /// Selection movement over the shared motion set.
    pub(crate) fn scroll(&mut self, motion: ScrollMotion, viewport_height: u16) {
        if self.results.is_empty() {
            return;
        }
        let cur = self.table_state.selected().unwrap_or(0);
        self.table_state.select(Some(motion.apply_index(
            cur,
            self.results.len(),
            viewport_height,
        )));
    }
}

// ---------------------------------------------------------------------------
// Popup open/move/confirm methods
// ---------------------------------------------------------------------------

/// A team's workflow states.
pub(crate) fn state_items(conn: &Connection, team_id: &str) -> Vec<PopupItem> {
    lt_runtime::db::query_team_states(conn, team_id)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, team_id, "failed to query team states");
            Vec::new()
        })
        .into_iter()
        .map(PopupItem::from)
        .collect()
}

/// The assignee popup's items: "Unassign" plus a team's members.
fn assignee_popup_items(conn: &Connection, team_id: &str) -> Vec<PopupItem> {
    let mut items: Vec<PopupItem> = vec![PopupItem {
        label: "Unassign".to_string(),
        id: None,
    }];
    match lt_runtime::db::query_team_members(conn, team_id) {
        Ok(members) => items.extend(members.into_iter().map(PopupItem::from)),
        Err(e) => tracing::warn!(error = %e, team_id, "failed to query team members"),
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

        let items = self.db.connect().map_or_else(
            |e| {
                tracing::warn!(error = %e, "state popup: failed to open db connection");
                Vec::new()
            },
            |conn| state_items(&conn, &team_id),
        );
        let selected = items
            .iter()
            .position(|item| item.label == current_state_name)
            .unwrap_or(0);
        self.push_view(View::Popup(PopupView {
            kind: PopupKind::State,
            issue_id,
            team_id: Some(team_id),
            items,
            selected,
            anchor: None,
        }));
        self.footer_msg = None;
    }

    pub(crate) fn open_priority_popup(&mut self) {
        let Some(issue) = self.selected_issue() else {
            return;
        };
        let issue_id = issue.id.inner().to_string();
        // Linear priority: 0=No priority, 1=Urgent, 2=High, 3=Normal, 4=Low
        let selected = usize::from(issue.priority.0);
        self.push_view(View::Popup(PopupView {
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

        let items = self.db.connect().map_or_else(
            |e| {
                tracing::warn!(error = %e, "assignee popup: failed to open db connection");
                Vec::new()
            },
            |conn| assignee_popup_items(&conn, &team_id),
        );
        let selected = current_assignee
            .and_then(|a| {
                items
                    .iter()
                    .position(|item| item.id.as_deref() == Some(a.as_str()))
            })
            .unwrap_or(0);
        self.push_view(View::Popup(PopupView {
            kind: PopupKind::Assignee,
            issue_id,
            team_id: Some(team_id),
            items,
            selected,
            anchor: None,
        }));
        self.footer_msg = None;
    }
}

impl PopupView {
    /// A matching `Team{team_id}` rebuilds `items` and re-anchors the
    /// selection by item id; the priority popup is static and never matches.
    pub(crate) fn consume(&mut self, ctx: &StateCtx, _focused: bool, ev: &StateEvent) {
        let StateEvent::Team { team_id } = ev else {
            return;
        };
        if self.team_id.as_deref() != Some(team_id.as_str()) {
            return;
        }
        let conn = match ctx.db.connect() {
            Ok(conn) => conn,
            Err(e) => {
                tracing::warn!(error = %e, "popup: failed to open db connection");
                return;
            }
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

    /// Selection movement over the shared motion set.
    pub(crate) fn scroll(&mut self, motion: ScrollMotion, viewport_height: u16) {
        if self.items.is_empty() {
            return;
        }
        self.selected = motion.apply_index(self.selected, self.items.len(), viewport_height);
    }
}

/// Confirm the popup choice: close the popup at its own index `i` (not
/// necessarily the stack top), then edit the issue it was opened for -- the
/// captured `issue_id`, not the current list selection.
fn popup_confirm(app: &mut App, i: usize) {
    let Some(View::Popup(popup)) = app.views.get(i) else {
        return;
    };
    let Some(item) = popup.items.get(popup.selected).cloned() else {
        return;
    };
    let issue_id = popup.issue_id.clone();
    let kind = popup.kind.clone();
    app.close_view_at(i);
    if let Some(edit) = popup_edit(&kind, &item)
        && let Err(e) = app.service.edit_issue(&issue_id, edit)
    {
        app.footer_msg = Some(format!("Failed to save: {e}"));
    }
}

// ---------------------------------------------------------------------------
// Popup selection -> IssueEdit mapping
// ---------------------------------------------------------------------------

/// Map a popup selection onto an `IssueEdit`. Unset choices (a priority/state
/// item with no id) are no-ops (`None`); an assignee item with no id clears
/// the assignee.
fn popup_edit(kind: &PopupKind, item: &PopupItem) -> Option<lt_runtime::sync::service::IssueEdit> {
    use lt_runtime::sync::service::IssueEdit;
    match kind {
        PopupKind::State => item.id.clone().map(|id| IssueEdit::State {
            id,
            name: item.label.clone(),
        }),
        PopupKind::Priority => item
            .id
            .as_deref()
            .and_then(|s| s.parse::<u8>().ok())
            .map(IssueEdit::Priority),
        PopupKind::Assignee => Some(IssueEdit::Assignee(
            item.id.clone().map(|id| (id, item.label.clone())),
        )),
    }
}

// ---------------------------------------------------------------------------
// Key handlers
// ---------------------------------------------------------------------------

// -- Popup actions ----------------------------------------------------

pub(crate) static POPUP_BINDINGS: keymap::Table = &[(
    keymap::Binding::Single(keymap::Key::plain(KeyCode::Enter)),
    keymap::Action::Confirm,
)];

pub(crate) static POPUP_KEYMAP: Keymap = Keymap {
    layers: &[POPUP_BINDINGS, keymap::GLOBAL],
    apply: Some(apply_popup),
    unbound: Unbound::Cascade,
};

/// The state/priority/assignee popup's non-navigation action.
pub(crate) fn apply_popup(app: &mut App, i: usize, action: keymap::Action) {
    if let keymap::Action::Confirm = action {
        popup_confirm(app, i);
    }
}

// -- Help popup ------------------------------------------------------

/// The keyboard-shortcuts help popup. `j`/`k` stay untypeable in the filter
/// bar -- an existing limitation, carried forward deliberately.
pub(crate) static HELP_BINDINGS: keymap::Table = &[
    (
        keymap::Binding::Single(keymap::Key::plain(KeyCode::Down)),
        keymap::Action::MoveDown,
    ),
    (
        keymap::Binding::Single(keymap::Key::char('j')),
        keymap::Action::MoveDown,
    ),
    (
        keymap::Binding::Single(keymap::Key::plain(KeyCode::Up)),
        keymap::Action::MoveUp,
    ),
    (
        keymap::Binding::Single(keymap::Key::char('k')),
        keymap::Action::MoveUp,
    ),
];

pub(crate) static HELP_KEYMAP: Keymap = Keymap {
    layers: &[HELP_BINDINGS],
    apply: None,
    unbound: Unbound::Forward(forward_help),
};

/// Forward an unbound key to the help popup's filter bar; `j`/`k` never
/// reach here since `HELP_BINDINGS` resolves them to `MoveDown`/`MoveUp`
/// first.
pub(crate) fn forward_help(app: &mut App, i: usize, ev: KeyEvent) {
    if let Some(View::Help(popup)) = app.views.get_mut(i)
        && popup.search.handle_key(ev.code, ev.modifiers)
    {
        popup.update_filter();
    }
}

// -- FTS search overlay ------------------------------------------------

/// The FTS search overlay. Plain `j`/`k` are deliberately unbound (typeable
/// filter text); `tab`/`shift+tab` drive stem-key completion and must not
/// reach the query bar.
pub(crate) static SEARCH_BINDINGS: keymap::Table = &[
    (
        keymap::Binding::Single(keymap::Key::plain(KeyCode::Enter)),
        keymap::Action::Confirm,
    ),
    (
        keymap::Binding::Single(keymap::Key::ctrl('c')),
        keymap::Action::ClearQuery,
    ),
    (
        keymap::Binding::Single(keymap::Key::plain(KeyCode::Down)),
        keymap::Action::MoveDown,
    ),
    (
        keymap::Binding::Single(keymap::Key::plain(KeyCode::Up)),
        keymap::Action::MoveUp,
    ),
    (
        keymap::Binding::Single(keymap::Key::ctrl('n')),
        keymap::Action::CompleteNext,
    ),
    (
        keymap::Binding::Single(keymap::Key::ctrl('p')),
        keymap::Action::CompletePrev,
    ),
    (
        keymap::Binding::Single(keymap::Key::ctrl('y')),
        keymap::Action::CompleteAccept,
    ),
    (
        keymap::Binding::Single(keymap::Key::plain(KeyCode::Tab)),
        keymap::Action::CompleteForward,
    ),
    (
        keymap::Binding::Single(keymap::Key::shift_tab()),
        keymap::Action::CompleteBackward,
    ),
];

pub(crate) static SEARCH_KEYMAP: Keymap = Keymap {
    layers: &[SEARCH_BINDINGS],
    apply: Some(apply_search),
    unbound: Unbound::Forward(forward_search),
};

/// The FTS search overlay's non-navigation actions.
pub(crate) fn apply_search(app: &mut App, i: usize, action: keymap::Action) {
    match action {
        keymap::Action::Confirm => confirm_search(app),
        keymap::Action::ClearQuery => {
            if let Some(View::Search(overlay)) = app.views.get_mut(i) {
                overlay.query = TextInput::from(search_query::DEFAULT_QUERY.to_string());
                overlay.last_changed = Some(Instant::now());
            }
        }
        keymap::Action::CompleteNext => {
            if let Some(View::Search(overlay)) = app.views.get_mut(i) {
                overlay.completer.cycle_next();
            }
        }
        keymap::Action::CompletePrev => {
            if let Some(View::Search(overlay)) = app.views.get_mut(i) {
                overlay.completer.cycle_prev();
            }
        }
        keymap::Action::CompleteAccept => {
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
        keymap::Action::CompleteForward => apply_completion_tab(app, i, true),
        keymap::Action::CompleteBackward => apply_completion_tab(app, i, false),
        // Other keymaps' actions never resolve here; kept exhaustive over
        // `Action` regardless.
        _ => {}
    }
}

/// Forward an unbound key to the query bar. `tab`/`shift+tab` never reach
/// here (`SEARCH_BINDINGS` binds them to completion); plain `j`/`k` are
/// deliberately unbound so they land here as typeable filter text.
pub(crate) fn forward_search(app: &mut App, i: usize, ev: KeyEvent) {
    if let Some(View::Search(overlay)) = app.views.get_mut(i)
        && overlay.query.handle_key(ev.code, ev.modifiers)
    {
        overlay.last_changed = Some(Instant::now());
    }
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
        overlay.run_search(&app.db, app.viewport_height);
    }
    let results = std::mem::take(&mut overlay.results);
    let selected = overlay.table_state.selected();
    if let View::List(list) = app.base_mut() {
        // AST is the single source of truth.
        list.query.filter = overlay.ast.clone();
        list.query.sync_args_from_filter();
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

/// Fire the FTS search once the 150ms debounce elapses. `viewport_height`/
/// `&app.db` are captured before the `views.last_mut()` borrow, since
/// `run_search` needs both simultaneously.
pub(crate) fn poll_search_debounce(app: &mut App) {
    let viewport_height = app.viewport_height;
    let db = &app.db;
    let should_search = matches!(
        app.views.last(),
        Some(View::Search(overlay))
            if overlay.last_changed.is_some_and(|t| t.elapsed() >= Duration::from_millis(150))
    );
    if should_search && let Some(View::Search(overlay)) = app.views.last_mut() {
        overlay.last_changed = None;
        overlay.run_search(db, viewport_height);
    }
}
