use crossterm::event::KeyCode;
use lt_runtime::query::SortField;
use lt_runtime::{Runtime, SubId, Subscription, search_query};
use lt_types::issues::{IssueConnection, IssueFilter, IssueSort, IssuesQuery, IssuesVariables};
use lt_types::types::Issue;
use ratatui::widgets::TableState;

use super::{
    App, AuthStatus, HelpPopup, Keymap, ScrollMotion, Unbound, View, keymap, open_in_browser,
};

/// Forward/backward pagination state.
pub struct Pagination {
    pub has_next_page: bool,
    pub current_cursor: Option<String>,
    pub cursor_stack: Vec<Option<String>>,
    pub end_cursor: Option<String>,
}

/// The active filter's sort field/direction, derived from its `sort:` token
/// (or `Updated`/desc if absent).
fn sort_from_filter(filter: &search_query::QueryAst) -> (SortField, bool) {
    match search_query::lower_ast(filter).1 {
        Some((field, dir)) => (field, dir == search_query::SortDir::Desc),
        None => (SortField::Updated, true),
    }
}

/// The base list view's initial spec: the starting filter/sort state and
/// page size, built by the caller (`lt-cli`, from `IssueArgs`) and passed to
/// [`crate::run`].
pub struct ListLaunch {
    pub filter: search_query::QueryAst,
    pub limit: u32,
}

/// The issue-list query: the active filter, sort, pagination cursor state,
/// and the launch snapshot the double-esc reset restores.
pub struct ListQuery {
    /// Single source of truth for the active filter/search state. Updated on
    /// Enter (confirm search), double-esc (reset), and sort shortcuts.
    pub filter: search_query::QueryAst,
    /// Kept in sync with `filter`'s `sort:` token.
    pub sort: SortField,
    pub desc: bool,
    /// Page size, fixed for the view's lifetime.
    pub limit: u32,
    pub pagination: Pagination,
    initial_filter: search_query::QueryAst,
}

impl ListQuery {
    pub fn new(filter: search_query::QueryAst, limit: u32) -> Self {
        let (sort, desc) = sort_from_filter(&filter);
        Self {
            initial_filter: filter.clone(),
            filter,
            sort,
            desc,
            limit,
            pagination: Pagination {
                has_next_page: false,
                current_cursor: None,
                cursor_stack: Vec::new(),
                end_cursor: None,
            },
        }
    }

    /// The active filter's `team:` value, if set -- used to pre-fill the
    /// new-issue modal's team field.
    pub(crate) fn team_filter(&self) -> Option<String> {
        search_query::lower_ast(&self.filter).0.team
    }

    /// Lower the active filter (resolving `assignee:me` against the viewer)
    /// and sort into `IssuesVariables` -- the vars the base list subscribes
    /// with. A filter/sort/pagination change is a new vars value: the caller
    /// drops the old subscription and subscribes anew with it.
    fn build_vars(&self, viewer_name: Option<&str>) -> IssuesVariables {
        let (mut filter, _) = search_query::lower_ast(&self.filter);
        search_query::resolve_me(&mut filter, viewer_name);
        let filter = (filter != IssueFilter::default()).then_some(filter);

        IssuesVariables {
            filter,
            sort: Some(IssueSort {
                field: self.sort.clone(),
                desc: self.desc,
            }),
            first: Some(i32::try_from(self.limit.min(250)).unwrap_or(250)),
            after: self.pagination.current_cursor.clone(),
        }
    }

    /// Advance the cursor stack to the next page. Returns whether a
    /// resubscribe is needed (`false` when there is no next page).
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
    /// resubscribe is needed (`false` when already at the first page).
    pub(crate) fn prev_page(&mut self) -> bool {
        let Some(cursor) = self.pagination.cursor_stack.pop() else {
            return false;
        };
        self.pagination.current_cursor = cursor;
        true
    }

    /// `d`: toggle sort direction and rewrite `filter`'s `sort:` token to
    /// match, resetting pagination cursors; the caller resubscribes.
    fn toggle_desc(&mut self) {
        self.desc = !self.desc;
        self.filter = self.replace_sort_in_filter();
        self.pagination.cursor_stack.clear();
        self.pagination.current_cursor = None;
    }

    /// Keep `sort`/`desc` in sync with `filter`.
    pub(crate) fn sync_sort_from_filter(&mut self) {
        let (sort, desc) = sort_from_filter(&self.filter);
        self.sort = sort;
        self.desc = desc;
    }

    /// Produce a new `QueryAst` with the `sort:` token replaced to match
    /// `sort`/`desc`.
    pub(crate) fn replace_sort_in_filter(&self) -> search_query::QueryAst {
        let dir = if self.desc { "-" } else { "+" };
        let new_sort = format!("sort:{}{}", self.sort.label(), dir);
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
        self.filter = self.initial_filter.clone();
        let (sort, desc) = sort_from_filter(&self.filter);
        self.sort = sort;
        self.desc = desc;
        self.pagination.cursor_stack.clear();
        self.pagination.current_cursor = None;
    }
}

/// The issue-list view: the base-list fields, owned, plus its live
/// `IssuesQuery` subscription.
pub struct ListView {
    pub issues: Vec<Issue>,
    pub table_state: TableState,
    /// An identifier to seek on the next subscription update; one-shot,
    /// cleared whether or not that update finds a match.
    pub pending_select: Option<String>,
    pub query: ListQuery,
    sub: Subscription<IssueConnection>,
}

impl ListView {
    pub(crate) fn new(
        issues: Vec<Issue>,
        query: ListQuery,
        sub: Subscription<IssueConnection>,
    ) -> Self {
        let mut table_state = TableState::default();
        if !issues.is_empty() {
            table_state.select(Some(0));
        }
        Self {
            issues,
            table_state,
            pending_select: None,
            query,
            sub,
        }
    }

    /// Subscribe to `query`'s vars and populate the view from the
    /// subscription's synchronous initial read -- the query defines the
    /// view's initial data, same as every later resubscribe.
    pub(crate) fn open(mut query: ListQuery, runtime: &Runtime, viewer_name: Option<&str>) -> Self {
        let vars = query.build_vars(viewer_name);
        let (sub, page) = runtime.subscribe::<IssuesQuery>(vars);
        query.pagination.has_next_page = page.page_info.has_next_page;
        query.pagination.end_cursor = page.page_info.end_cursor;
        Self::new(page.nodes, query, sub)
    }

    pub(crate) fn selected_issue(&self) -> Option<&Issue> {
        self.table_state.selected().and_then(|i| self.issues.get(i))
    }

    /// Selection movement over the shared motion set.
    pub(crate) fn scroll(&mut self, motion: ScrollMotion, viewport_height: u16) {
        motion.apply_table(&mut self.table_state, self.issues.len(), viewport_height);
    }

    /// Consume the subscription's latest and re-apply ui-state policy, if a
    /// newer result has arrived. A no-op when nothing has -- safe to call
    /// unconditionally on focus return.
    fn sync_from_subscription(&mut self, reset_selection: bool) {
        if let Some(page) = self.sub.take() {
            self.issues = page.nodes;
            self.query.pagination.has_next_page = page.page_info.has_next_page;
            self.query.pagination.end_cursor = page.page_info.end_cursor;
            self.apply_fetched_selection(reset_selection);
            self.seek_pending_select();
        }
    }

    /// Only consume while focused: a refresh must not swap the rows a popup
    /// is anchored to or a search overlay covers. The slot holds the latest
    /// for focus return (`resume_focus`).
    pub(crate) fn apply_update(&mut self, focused: bool, id: SubId) {
        if focused && self.sub.id() == id {
            self.sync_from_subscription(false); // offset- and selection-preserving
        }
    }

    /// Called when the list regains focus (an overlay above it popped):
    /// pick up whatever the subscription accumulated while unfocused.
    pub(crate) fn resume_focus(&mut self) {
        self.sync_from_subscription(false);
    }

    /// Drop the current subscription and subscribe anew with `query`'s
    /// current vars: a filter/sort/pagination change is a new vars value,
    /// not a refetch.
    pub(crate) fn resubscribe(
        &mut self,
        runtime: &Runtime,
        viewer_name: Option<&str>,
        reset_selection: bool,
    ) {
        let vars = self.query.build_vars(viewer_name);
        let (sub, page) = runtime.subscribe::<IssuesQuery>(vars);
        self.sub = sub;
        self.issues = page.nodes;
        self.query.pagination.has_next_page = page.page_info.has_next_page;
        self.query.pagination.end_cursor = page.page_info.end_cursor;
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
            let viewer_name = app.auth.viewer_name().map(str::to_string);
            let runtime = app.runtime.clone();
            if let Some(View::List(list)) = app.views.get_mut(i) {
                let resubscribe = if action == Action::ToggleSortDirection {
                    list.query.toggle_desc();
                    true
                } else if action == Action::NextPage {
                    list.query.next_page()
                } else {
                    list.query.prev_page()
                };
                if resubscribe {
                    list.resubscribe(&runtime, viewer_name.as_deref(), true);
                }
            }
        }
        // Re-authenticate: background OAuth login.
        Action::Login if !matches!(app.auth, AuthStatus::Authenticating) => {
            app.auth = AuthStatus::Authenticating;
            app.runtime.login();
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
