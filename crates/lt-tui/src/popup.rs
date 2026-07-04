use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use lt_runtime::db::Connection;
use lt_runtime::query::IssueQuery;
use lt_runtime::search_query;
use ratatui::widgets::TableState;

use super::search_completer::Completer;
use super::{App, Scroll, ScrollMotion, StateCtx, StateEvent, TextInput, View, keymap};

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
    /// Current search query typed by the user.
    pub search: TextInput,
    /// The keymap's help rows (`keymap::help_rows()`), built once at
    /// construction so help can no longer drift from the tables it reads.
    /// Not `pub`: `keymap::HelpRow` is crate-private.
    pub(crate) rows: Vec<keymap::HelpRow>,
    /// Indices into `rows` that match the current search.
    pub filtered: Vec<usize>,
    /// Currently highlighted row in the filtered list.
    pub selected: usize,
}

impl HelpPopup {
    pub fn new() -> Self {
        let rows = keymap::help_rows();
        let filtered = (0..rows.len()).collect();
        Self {
            search: TextInput::new(),
            rows,
            filtered,
            selected: 0,
        }
    }

    /// Matches the query against the rendered binding form (`HelpRow::binding_form`,
    /// e.g. "j / down"), the label, and the context -- case-insensitive, like today.
    pub fn update_filter(&mut self) {
        let q = self.search.value.to_lowercase();
        self.filtered = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                q.is_empty()
                    || row.binding_form().to_lowercase().contains(&q)
                    || row.label.to_lowercase().contains(&q)
                    || row.context.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    fn move_down(&mut self) {
        let max = self.filtered.len().saturating_sub(1);
        if self.selected < max {
            self.selected += 1;
        }
    }

    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
}

/// This view's scroll override: `Down`/`Up` move the filtered-list
/// selection; every other motion no-ops via `Scroll`'s defaults (the help
/// popup has no "half page" concept, `docs/design/keybinds.md`, "Help").
impl Scroll for HelpPopup {
    fn move_down(&mut self) {
        self.move_down();
    }
    fn move_up(&mut self) {
        self.move_up();
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
    /// The base list's query limit, captured once at open time
    /// (`open_search_overlay`) so both views show the same number of
    /// results. Faithful for the overlay's whole lifetime: the base list's
    /// limit cannot change while Search has focus (it consumes every key).
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

        // Use the same limit as the normal issue list so both views show the
        // same number of results.  Cap to viewport height so we never render
        // more rows than fit on screen.
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

/// This view's scroll override: `Down`/`Up` move the result-list selection;
/// every other motion no-ops via `Scroll`'s defaults (the search overlay has
/// no "half page" concept, `docs/design/keybinds.md`, "Search").
impl Scroll for SearchOverlay {
    fn move_down(&mut self) {
        self.move_down();
    }
    fn move_up(&mut self) {
        self.move_up();
    }
}

// ---------------------------------------------------------------------------
// Popup open/move/confirm methods
// ---------------------------------------------------------------------------

/// A team's workflow states. Shared by `open_state_popup`/
/// `PopupView::consume`'s `State` arm and the new-issue modal's own state
/// picker.
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

/// The assignee popup's items -- "Unassign" plus a team's members. Shared by
/// `open_assignee_popup` and `PopupView::consume`'s `Assignee` arm.
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
    /// The state and assignee popups' subscription: a matching
    /// `Team{team_id}` rebuilds `items` and re-anchors the selection by item
    /// id. The priority popup is static (`team_id: None`) and never matches.
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

    /// Move the selection by `delta` items, clamped to the item list.
    fn move_by(&mut self, delta: i32) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let step = usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX);
        self.selected = if delta >= 0 {
            self.selected.saturating_add(step).min(n - 1)
        } else {
            self.selected.saturating_sub(step)
        };
    }

    /// This popup's scroll override: selection movement over the shared
    /// motion set (Decision 6). Previously only j/k moved the selection;
    /// g/G/Ctrl-d/Ctrl-u/PageDown/PageUp were ignored (behavior change 14).
    pub(crate) fn scroll(&mut self, motion: ScrollMotion, viewport_height: u16) {
        match motion {
            ScrollMotion::Down => self.move_by(1),
            ScrollMotion::Up => self.move_by(-1),
            ScrollMotion::Top => self.move_by(i32::MIN / 2),
            ScrollMotion::Bottom => self.move_by(i32::MAX / 2),
            ScrollMotion::HalfPageDown => self.move_by(i32::from(viewport_height) / 2),
            ScrollMotion::HalfPageUp => self.move_by(-(i32::from(viewport_height) / 2)),
            ScrollMotion::PageDown => self.move_by(i32::from(viewport_height)),
            ScrollMotion::PageUp => self.move_by(-i32::from(viewport_height)),
        }
    }
}

/// Confirm the popup choice: pop it, then edit the issue it was opened for
/// (its captured `issue_id`, not the current list selection) through the
/// sync service, which enqueues the write and emits the matching `State`
/// event on the queue. A failure surfaces in the footer.
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

/// The `Popup` context's non-navigation action. Navigation actions never
/// reach here: `resolve_and_apply` maps them to `ScrollMotion` and applies
/// them through `View::scroll` instead.
pub(crate) fn apply_popup(app: &mut App, i: usize, action: keymap::Action) {
    if let keymap::Action::Confirm = action {
        popup_confirm(app, i);
    }
}

// -- Help popup ------------------------------------------------------

/// The `Help` context's non-navigation actions: none today -- `HELP` binds
/// only `MoveDown`/`MoveUp`, both navigation, intercepted by `scroll_motion`
/// before this is reached. Kept as a named arm for symmetry with the other
/// contexts, and so a future non-navigation `Help` binding has an obvious
/// home.
pub(crate) fn apply_help(_app: &mut App, _i: usize, _action: keymap::Action) {}

/// Forward an unbound key to the help popup's filter bar. `j`/`k` stay
/// untypeable here (an existing limitation, carried forward deliberately):
/// they resolve to `MoveDown`/`MoveUp` in `HELP` and never reach `Unbound`.
pub(crate) fn forward_help(app: &mut App, i: usize, ev: KeyEvent) {
    if let Some(View::Help(popup)) = app.views.get_mut(i)
        && popup.search.handle_key(ev.code, ev.modifiers)
    {
        popup.update_filter();
    }
}

// -- FTS search overlay ------------------------------------------------

/// The `Search` context's non-navigation actions. Navigation (`MoveDown`/
/// `MoveUp`) never reaches here: `resolve_and_apply` maps it to
/// `ScrollMotion` and applies it through `View::scroll` instead.
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
        // Navigation and other contexts' actions never resolve to
        // `Search`'s table; the match stays exhaustive over `Action`
        // regardless.
        _ => {}
    }
}

/// Forward an unbound key to the query bar. `tab`/`shift+tab` never reach
/// here (`SEARCH` binds them to completion); plain `j`/`k` are deliberately
/// unbound so they land here as typeable filter text.
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
        list.filter = overlay.ast.clone();
        list.sync_args_from_filter();
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
/// live check; `viewport_height` and `&app.db` are copied/borrowed before the
/// `views.last_mut()` borrow since `run_search` needs both simultaneously.
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
