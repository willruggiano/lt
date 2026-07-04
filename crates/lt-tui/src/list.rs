use crossterm::event::KeyCode;
use lt_runtime::query::IssueQuery;
use lt_runtime::search_query;
use lt_types::types::Issue;
use ratatui::widgets::TableState;

use super::{
    App, AuthStatus, HelpPopup, Keymap, ScrollMotion, StateCtx, StateEvent, Unbound, View, keymap,
    open_in_browser,
};

/// Forward/backward pagination state.
pub struct Pagination {
    pub has_next_page: bool,
    pub current_cursor: Option<String>,
    pub cursor_stack: Vec<Option<String>>,
    pub end_cursor: Option<String>,
}

/// The issue-list query: inputs, the active filter, pagination cursor
/// state, and the launch snapshot the double-esc reset restores.
pub struct ListQuery {
    /// The issue-list query, kept in sync with `filter`'s `sort:` token.
    pub args: IssueQuery,
    /// Single source of truth for the active filter/search state. Updated on
    /// Enter (confirm search), double-esc (reset), and sort shortcuts.
    pub filter: search_query::QueryAst,
    pub pagination: Pagination,
    initial_args: IssueQuery,
    initial_filter: search_query::QueryAst,
}

impl From<IssueQuery> for ListQuery {
    fn from(args: IssueQuery) -> Self {
        let filter = search_query::args_to_ast(&args);
        Self {
            initial_args: args.clone(),
            initial_filter: filter.clone(),
            args,
            filter,
            pagination: Pagination {
                has_next_page: false,
                current_cursor: None,
                cursor_stack: Vec::new(),
                end_cursor: None,
            },
        }
    }
}

impl ListQuery {
    /// Both branches of the issue-list fetch -- `run_query` when the parsed
    /// filter has constraints beyond sort, else the paginated
    /// `query_issues_page` -- updating `pagination`. Returns the fetched
    /// rows, or an empty `Vec` (warning via `tracing`) on a query failure.
    fn fetch(&mut self, ctx: &StateCtx) -> Vec<Issue> {
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
                    self.pagination.has_next_page = false; // run_query has no pagination
                    self.pagination.end_cursor = None;
                    issues
                }
                Err(e) => {
                    tracing::warn!(error = %e, "issue list fetch failed");
                    Vec::new()
                }
            }
        } else {
            // No active filters: use the paginated query.
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
                    self.pagination.has_next_page = has_next_page;
                    let limit = i64::from(self.args.limit.min(250));
                    self.pagination.end_cursor = if has_next_page {
                        Some((offset + limit).to_string())
                    } else {
                        None
                    };
                    issues
                }
                Err(e) => {
                    tracing::warn!(error = %e, "issue list fetch failed");
                    Vec::new()
                }
            }
        }
    }

    /// Advance the cursor stack to the next page. Returns whether a re-fetch
    /// is needed (`false` when there is no next page).
    pub(crate) fn next_page(&mut self) -> bool {
        if !self.pagination.has_next_page {
            return false;
        }
        let end = self.pagination.end_cursor.clone();
        self.pagination
            .cursor_stack
            .push(self.pagination.current_cursor.clone());
        self.pagination.current_cursor = end;
        true
    }

    /// Pop the cursor stack back to the previous page. Returns whether a
    /// re-fetch is needed (`false` when already at the first page).
    pub(crate) fn prev_page(&mut self) -> bool {
        let Some(cursor) = self.pagination.cursor_stack.pop() else {
            return false;
        };
        self.pagination.current_cursor = cursor;
        true
    }

    /// `d`: toggle sort direction and rewrite `filter`'s `sort:` token to
    /// match, resetting pagination cursors; the caller re-fetches.
    fn toggle_desc(&mut self) {
        self.args.desc = !self.args.desc;
        self.filter = self.replace_sort_in_filter();
        self.pagination.cursor_stack.clear();
        self.pagination.current_cursor = None;
    }

    /// Keep `args.sort`/`args.desc` in sync with `filter`.
    pub(crate) fn sync_args_from_filter(&mut self) {
        let parsed = search_query::ParsedQuery::from(&self.filter);
        if let Some((field, dir)) = parsed.sort {
            self.args.sort = field;
            self.args.desc = dir == search_query::SortDir::Desc;
        }
    }

    /// Produce a new `QueryAst` with the `sort:` token replaced to match
    /// `args.sort`/`args.desc`.
    pub(crate) fn replace_sort_in_filter(&self) -> search_query::QueryAst {
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

    /// Restore the launch snapshot and clear pagination cursors.
    pub(crate) fn reset(&mut self) {
        self.args = self.initial_args.clone();
        self.filter = self.initial_filter.clone();
        self.pagination.cursor_stack.clear();
        self.pagination.current_cursor = None;
    }
}

/// The issue-list view: the base-list fields, owned.
pub struct ListView {
    pub issues: Vec<Issue>,
    pub table_state: TableState,
    /// An identifier to seek on the next `Issues` re-read; one-shot,
    /// cleared whether or not that re-read finds a match.
    pub pending_select: Option<String>,
    pub query: ListQuery,
}

impl ListView {
    pub(crate) fn new(issues: Vec<Issue>, query: ListQuery) -> Self {
        let mut table_state = TableState::default();
        if !issues.is_empty() {
            table_state.select(Some(0));
        }
        Self {
            issues,
            table_state,
            pending_select: None,
            query,
        }
    }

    /// Construct the view and populate it from `query`'s own fetch -- the
    /// query defines the view's initial data, same as every later refetch.
    pub(crate) fn open(query: ListQuery, ctx: &StateCtx) -> Self {
        let mut view = Self::new(Vec::new(), query);
        view.refetch(ctx, true); // reset_selection: select row 0 when non-empty
        view
    }

    pub(crate) fn selected_issue(&self) -> Option<&Issue> {
        self.table_state.selected().and_then(|i| self.issues.get(i))
    }

    /// Selection movement over the shared motion set.
    pub(crate) fn scroll(&mut self, motion: ScrollMotion, viewport_height: u16) {
        motion.apply_table(&mut self.table_state, self.issues.len(), viewport_height);
    }

    /// Only refetch while focused: a refresh must not swap the rows a popup
    /// is anchored to or a search overlay covers.
    pub(crate) fn consume(&mut self, ctx: &StateCtx, focused: bool, ev: &StateEvent) {
        if matches!(ev, StateEvent::Issues) && focused {
            self.refetch(ctx, false); // offset- and selection-preserving
        }
    }

    /// Re-fetch through `query`, apply the fetched-selection policy, and
    /// seek `pending_select` if set.
    pub(crate) fn refetch(&mut self, ctx: &StateCtx, reset_selection: bool) {
        self.issues = self.query.fetch(ctx);
        self.apply_fetched_selection(reset_selection);
        self.seek_pending_select();
    }

    /// After replacing `self.issues`, clamp/reset the selection and mark idle.
    pub(crate) fn apply_fetched_selection(&mut self, reset_selection: bool) {
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
    }

    /// Seek to `pending_select`'s identifier and clear it; a miss also
    /// clears it, since this is a one-shot seek, not a retried one.
    fn seek_pending_select(&mut self) {
        if let Some(id) = self.pending_select.take()
            && let Some(idx) = self.issues.iter().position(|i| i.identifier == id)
        {
            self.table_state.select(Some(idx));
        }
    }
}

// -- Normal list keybindings -------------------------------------------------

pub(crate) static LIST_BINDINGS: keymap::Table = &[
    (
        keymap::Binding::Single(keymap::Key::plain(KeyCode::Enter)),
        keymap::Action::OpenDetail,
    ),
    (
        keymap::Binding::Single(keymap::Key::char(' ')),
        keymap::Action::OpenDetail,
    ),
    (
        keymap::Binding::Single(keymap::Key::char('/')),
        keymap::Action::OpenSearch,
    ),
    (
        keymap::Binding::Single(keymap::Key::ctrl('/')),
        keymap::Action::OpenHelp,
    ),
    // Legacy terminals send Ctrl+/ as 0x1F, which crossterm decodes as
    // ctrl+'7'; kitty-enhanced terminals deliver a true ctrl+/. Both bound.
    (
        keymap::Binding::Single(keymap::Key::ctrl('7')),
        keymap::Action::OpenHelp,
    ),
    (
        keymap::Binding::Single(keymap::Key::char('c')),
        keymap::Action::CreateIssue,
    ),
    (
        keymap::Binding::Single(keymap::Key::char('s')),
        keymap::Action::SetStatus,
    ),
    (
        keymap::Binding::Single(keymap::Key::char('p')),
        keymap::Action::SetPriority,
    ),
    (
        keymap::Binding::Single(keymap::Key::char('a')),
        keymap::Action::SetAssignee,
    ),
    (
        keymap::Binding::Single(keymap::Key::ctrl('r')),
        keymap::Action::Refresh,
    ),
    (
        keymap::Binding::Single(keymap::Key::char('d')),
        keymap::Action::ToggleSortDirection,
    ),
    (
        keymap::Binding::Chord(keymap::Key::char('o'), keymap::Key::char('b')),
        keymap::Action::OpenInBrowser,
    ),
    (
        keymap::Binding::Single(keymap::Key::ctrl('n')),
        keymap::Action::NextPage,
    ),
    (
        keymap::Binding::Single(keymap::Key::ctrl('p')),
        keymap::Action::PrevPage,
    ),
    (
        keymap::Binding::Single(keymap::Key::char('L')),
        keymap::Action::Login,
    ),
];

pub(crate) static LIST_KEYMAP: Keymap = Keymap {
    layers: &[LIST_BINDINGS, keymap::GLOBAL],
    apply: Some(apply_list),
    unbound: Unbound::Cascade,
};

/// The list view's non-navigation actions.
pub(crate) fn apply_list(app: &mut App, i: usize, action: keymap::Action) {
    use keymap::Action;
    match action {
        Action::OpenDetail => app.open_detail(),
        Action::OpenSearch => app.open_search_overlay(),
        Action::OpenHelp => app.push_view(View::Help(HelpPopup::new())),
        Action::CreateIssue => app.open_new_issue_modal(),
        Action::SetStatus => app.open_state_popup(),
        Action::SetPriority => app.open_priority_popup(),
        Action::SetAssignee => app.open_assignee_popup(),
        Action::Refresh => app.refresh(),
        Action::ToggleSortDirection | Action::NextPage | Action::PrevPage => {
            let ctx = StateCtx {
                db: &app.db,
                viewer_name: app.auth.viewer_name(),
            };
            if let Some(View::List(list)) = app.views.get_mut(i) {
                let refetch = if action == Action::ToggleSortDirection {
                    list.query.toggle_desc();
                    true
                } else if action == Action::NextPage {
                    list.query.next_page()
                } else {
                    list.query.prev_page()
                };
                if refetch {
                    list.refetch(&ctx, true);
                }
            }
        }
        // Re-authenticate: background OAuth login.
        Action::Login if !matches!(app.auth, AuthStatus::Authenticating) => {
            app.auth = AuthStatus::Authenticating;
            app.service.login();
        }
        Action::OpenInBrowser => {
            if let Some(issue) = app.selected_issue() {
                open_in_browser(&issue.identifier);
            }
        }
        // Navigation is intercepted by `scroll_motion` before this runs;
        // `Comment`/`Confirm` belong to other contexts. Kept exhaustive
        // over `Action` regardless.
        _ => {}
    }
}
