use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lt_runtime::db::Database;
use lt_types::types::Issue;

use super::{App, KeyFlow, StateCtx, StateEvent, Status, View};

/// The detail pane's complete state: the shared `types`/`comments` fragments
/// the TUI composes for display, plus the panel's scroll offset and comment
/// draft (owned here, not on `App`).
pub struct DetailView {
    pub issue: Issue,
    pub comments: Vec<lt_types::comments::Comment>,
    pub parent: Option<Issue>,
    pub children: Vec<Issue>,
    /// Vertical scroll offset inside the detail pane (in lines).
    pub scroll: u16,
    /// Multiline buffer for a new comment, open in the detail pane. The
    /// cursor is always at the end (same model as the new-issue description
    /// field).
    pub comment_input: Option<String>,
}

impl DetailView {
    /// This pane's `StateEvent` subscriptions: `Comments{issue_id}` matching
    /// its own issue re-reads the thread; `Issues` re-reads the displayed
    /// issue itself (a popup edit confirmed above this pane, or a sync
    /// upsert). Both are payload-free idempotent re-reads through `ctx.db`.
    pub(crate) fn consume(&mut self, ctx: &StateCtx, _focused: bool, ev: &StateEvent) {
        match ev {
            StateEvent::Comments { issue_id } if issue_id == self.issue.id.inner() => {
                if let Ok(conn) = ctx.db.connect()
                    && let Ok(comments) = lt_runtime::db::query_comments(&conn, issue_id)
                {
                    self.comments = comments;
                }
            }
            StateEvent::Issues => {
                if let Ok(conn) = ctx.db.connect()
                    && let Ok(Some(fresh)) =
                        lt_runtime::db::query_issue_by_id(&conn, self.issue.id.inner())
                {
                    self.issue = fresh;
                }
            }
            _ => {}
        }
    }

    pub(crate) fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_add(1);
    }

    pub(crate) fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
    }

    pub(crate) fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }

    pub(crate) fn scroll_to_bottom(&mut self) {
        // Ratatui clamps scroll to content length; use a large sentinel.
        self.scroll = u16::MAX;
    }

    pub(crate) fn scroll_half_page_down(&mut self, viewport_height: u16) {
        self.scroll_by((viewport_height / 2).max(1), true);
    }

    pub(crate) fn scroll_half_page_up(&mut self, viewport_height: u16) {
        self.scroll_by((viewport_height / 2).max(1), false);
    }

    pub(crate) fn scroll_page_down(&mut self, viewport_height: u16) {
        self.scroll_by(viewport_height.max(1), true);
    }

    pub(crate) fn scroll_page_up(&mut self, viewport_height: u16) {
        self.scroll_by(viewport_height.max(1), false);
    }

    /// Scroll the detail pane by `step` rows, `down` toward the bottom.
    fn scroll_by(&mut self, step: u16, down: bool) {
        self.scroll = if down {
            self.scroll.saturating_add(step)
        } else {
            self.scroll.saturating_sub(step)
        };
    }
}

impl App {
    /// Open the detail pane for the currently selected issue.
    ///
    /// The detail is populated instantly from the local SQLite cache so the
    /// pane appears without any network round-trip. `push_view` declares the
    /// pane's `Comments{issue_id}` interest, which prompts the loop to
    /// refresh it; the re-read happens at consume time, not here
    /// (`DetailView::consume`).
    pub(crate) fn open_detail(&mut self) {
        let Some(issue) = self.selected_issue().cloned() else {
            return;
        };

        // Build the detail view instantly from cached data.
        let cached_comments = self
            .db
            .connect()
            .and_then(|conn| lt_runtime::db::query_comments(&conn, issue.id.inner()))
            .unwrap_or_default();

        let mut detail = build_cached_detail(&issue, cached_comments);
        populate_relations(&self.db, &mut detail, &issue);

        self.push_view(View::Detail(Box::new(detail)));
        if let Some(list) = self.base_list_mut() {
            list.status = Status::Idle;
        }
    }

    /// Close the detail pane and return to the list.
    pub(crate) fn close_detail(&mut self) {
        self.pop_view();
        if let Some(list) = self.base_list_mut() {
            list.status = Status::Idle;
        }
    }
}

/// Build a detail view from a cached list `Issue` plus its cached comments.
/// Parent/children are left empty; `populate_relations` fills them in.
pub(crate) fn build_cached_detail(
    issue: &Issue,
    cached_comments: Vec<lt_types::comments::Comment>,
) -> DetailView {
    DetailView {
        issue: issue.clone(),
        comments: cached_comments,
        parent: None,
        children: Vec::new(),
        scroll: 0,
        comment_input: None,
    }
}

/// Populate a detail's parent/children fields from the local DB cache.
pub(crate) fn populate_relations(db: &Database, detail: &mut DetailView, issue: &Issue) {
    let Ok(conn) = db.connect() else {
        return;
    };
    // Look up children.
    if let Ok(children) = lt_runtime::db::query_children(&conn, issue.id.inner()) {
        detail.children = children;
    }
    // Look up parent.
    if let Some(ref parent) = issue.parent
        && let Ok(Some(row)) = lt_runtime::db::query_issue_by_id(&conn, parent.id.inner())
    {
        detail.parent = Some(row);
    }
}

// -- Detail pane keybindings --------------------------------
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

pub(crate) fn handle_key(app: &mut App, i: usize, key: KeyEvent) -> KeyFlow {
    let code = key.code;
    let modifiers = key.modifiers;
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);

    // When the comment input is open, all keys go to it.
    let comment_open = detail_view_mut(app, i).is_some_and(|d| d.comment_input.is_some());
    if comment_open {
        handle_comment_input_key(app, i, code, modifiers);
        return KeyFlow::Consumed;
    }

    let viewport_height = app.viewport_height;
    match code {
        KeyCode::Esc | KeyCode::Char('q') => app.close_detail(),
        // Open the comment input.
        KeyCode::Char('c') => {
            if let Some(d) = detail_view_mut(app, i) {
                d.comment_input = Some(String::new());
            }
            app.footer_msg = None;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if let Some(d) = detail_view_mut(app, i) {
                d.scroll_down();
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if let Some(d) = detail_view_mut(app, i) {
                d.scroll_up();
            }
        }
        KeyCode::Char('g') => {
            if let Some(d) = detail_view_mut(app, i) {
                d.scroll_to_top();
            }
        }
        KeyCode::Char('G') => {
            if let Some(d) = detail_view_mut(app, i) {
                d.scroll_to_bottom();
            }
        }
        KeyCode::Char('d') if ctrl => {
            if let Some(d) = detail_view_mut(app, i) {
                d.scroll_half_page_down(viewport_height);
            }
        }
        KeyCode::Char('u') if ctrl => {
            if let Some(d) = detail_view_mut(app, i) {
                d.scroll_half_page_up(viewport_height);
            }
        }
        KeyCode::PageDown => {
            if let Some(d) = detail_view_mut(app, i) {
                d.scroll_page_down(viewport_height);
            }
        }
        KeyCode::PageUp => {
            if let Some(d) = detail_view_mut(app, i) {
                d.scroll_page_up(viewport_height);
            }
        }
        KeyCode::Char('o') => {
            if let Some(d) = detail_view_mut(app, i) {
                let url = format!("https://linear.app/issue/{}", d.issue.identifier);
                let _ = open::that(url);
            }
        }
        _ => {}
    }
    KeyFlow::Consumed
}

fn detail_view_mut(app: &mut App, i: usize) -> Option<&mut DetailView> {
    app.view_at_mut(i, |v| match v {
        View::Detail(d) => Some(d.as_mut()),
        _ => None,
    })
}

/// Enqueue the comment buffer as a local create through the sync service: the
/// optimistic `local:` row plus a `commentCreate` outbox command, in one
/// transaction, followed by the matching `State(Comments)` event on the
/// queue -- the sync drainer later posts the command and reconciles the temp
/// row with the server copy. A failure surfaces in the footer.
fn submit_comment(app: &mut App, i: usize) {
    let Some(detail) = detail_view_mut(app, i) else {
        return;
    };
    let Some(body) = detail.comment_input.as_ref().map(|b| b.trim().to_string()) else {
        return;
    };
    if body.is_empty() {
        detail.comment_input = None;
        return;
    }
    let issue_id = detail.issue.id.inner().to_string();
    detail.comment_input = None;

    let input = lt_types::inputs::CommentCreateInput { issue_id, body };
    if let Err(e) = app.service.create_comment(&input) {
        app.footer_msg = Some(format!("Failed to save comment: {e}"));
    }
}

/// Key handling for the comment input box (same editing model as the
/// new-issue description field: cursor always at the end).
fn handle_comment_input_key(app: &mut App, i: usize, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);

    // Ctrl-Enter submits (Alt-Enter on terminals that cannot distinguish
    // Ctrl-Enter from Enter).
    if (ctrl || alt) && code == KeyCode::Enter {
        submit_comment(app, i);
        return;
    }
    // Esc cancels.
    if code == KeyCode::Esc {
        if let Some(d) = detail_view_mut(app, i) {
            d.comment_input = None;
        }
        return;
    }

    let Some(detail) = detail_view_mut(app, i) else {
        return;
    };
    let Some(buf) = detail.comment_input.as_mut() else {
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
