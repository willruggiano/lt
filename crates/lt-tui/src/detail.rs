use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lt_runtime::db::Database;
use lt_types::types::Issue;

use super::{App, FetchStatus, Scroll, StateCtx, StateEvent, View, keymap};

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
                match ctx
                    .db
                    .connect()
                    .and_then(|conn| lt_runtime::db::query_comments(&conn, issue_id))
                {
                    Ok(comments) => self.comments = comments,
                    Err(e) => {
                        tracing::warn!(error = %e, issue_id, "detail pane: failed to re-read comments");
                    }
                }
            }
            StateEvent::Issues => {
                match ctx.db.connect().and_then(|conn| {
                    lt_runtime::db::query_issue_by_id(&conn, self.issue.id.inner())
                }) {
                    Ok(Some(fresh)) => self.issue = fresh,
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, issue_id = self.issue.id.inner(), "detail pane: failed to re-read issue");
                    }
                }
            }
            _ => {}
        }
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

/// This view's scroll override: offset scrolling (Decision 6).
impl Scroll for DetailView {
    fn move_down(&mut self) {
        self.scroll = self.scroll.saturating_add(1);
    }
    fn move_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
    }
    fn move_top(&mut self) {
        self.scroll = 0;
    }
    fn move_bottom(&mut self) {
        // Ratatui clamps scroll to content length; use a large sentinel.
        self.scroll = u16::MAX;
    }
    fn half_page_down(&mut self, viewport_height: u16) {
        self.scroll_by((viewport_height / 2).max(1), true);
    }
    fn half_page_up(&mut self, viewport_height: u16) {
        self.scroll_by((viewport_height / 2).max(1), false);
    }
    fn page_down(&mut self, viewport_height: u16) {
        self.scroll_by(viewport_height.max(1), true);
    }
    fn page_up(&mut self, viewport_height: u16) {
        self.scroll_by(viewport_height.max(1), false);
    }
}

impl App {
    /// Open the detail pane for the currently selected issue.
    ///
    /// The detail pane is populated instantly from local data so it appears
    /// without waiting on the network. `push_view` declares the pane's
    /// `Comments{issue_id}` interest, which prompts the loop to refresh it;
    /// the re-read happens at consume time, not here (`DetailView::consume`).
    pub(crate) fn open_detail(&mut self) {
        let Some(issue) = self.selected_issue().cloned() else {
            return;
        };

        // Build the detail view instantly from local data.
        let cached_comments = self
            .db
            .connect()
            .and_then(|conn| lt_runtime::db::query_comments(&conn, issue.id.inner()))
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, issue_id = issue.id.inner(), "detail pane: failed to load cached comments");
                Vec::new()
            });

        let mut detail = build_cached_detail(&issue, cached_comments);
        populate_relations(&self.db, &mut detail, &issue);

        self.push_view(View::Detail(Box::new(detail)));
        if let View::List(list) = self.base_mut() {
            list.status = FetchStatus::Idle;
        }
    }
}

/// Build a detail view from a list `Issue` plus its comments. Parent/children
/// are left empty; `populate_relations` fills them in.
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

/// Populate a detail's parent/children fields from the local database.
pub(crate) fn populate_relations(db: &Database, detail: &mut DetailView, issue: &Issue) {
    let conn = match db.connect() {
        Ok(conn) => conn,
        Err(e) => {
            tracing::warn!(error = %e, issue_id = issue.id.inner(), "detail pane: failed to open db connection");
            return;
        }
    };
    // Look up children.
    match lt_runtime::db::query_children(&conn, issue.id.inner()) {
        Ok(children) => detail.children = children,
        Err(e) => {
            tracing::warn!(error = %e, issue_id = issue.id.inner(), "detail pane: failed to query children");
        }
    }
    // Look up parent.
    if let Some(ref parent) = issue.parent {
        match lt_runtime::db::query_issue_by_id(&conn, parent.id.inner()) {
            Ok(Some(row)) => detail.parent = Some(row),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %e, parent_id = parent.id.inner(), "detail pane: failed to query parent");
            }
        }
    }
}

// -- Detail pane keybindings (docs/design/keybinds.md, "Detail") ------------

/// The `Detail` context's non-navigation actions. Navigation actions never
/// reach here: `resolve_and_apply` maps them to `ScrollMotion` and applies
/// them through `View::scroll` instead.
pub(crate) fn apply_detail(app: &mut App, i: usize, action: keymap::Action) {
    match action {
        keymap::Action::Comment => {
            if let Some(d) = detail_view_mut(app, i) {
                d.comment_input = Some(String::new());
            }
            app.footer_msg = None;
        }
        keymap::Action::OpenInBrowser => {
            if let Some(d) = detail_view_mut(app, i) {
                super::open_in_browser(&d.issue.identifier);
            }
        }
        // Navigation and other contexts' actions never resolve to `Detail`'s
        // table; the match stays exhaustive over `Action` regardless.
        _ => {}
    }
}

/// The `CommentInput` context's actions: `Submit`/`Back` -- `Back` is the
/// keymap's one `esc` row, cancelling the draft without popping the `Detail`
/// view beneath it (narrower than the floor's pop).
pub(crate) fn apply_comment_input(app: &mut App, i: usize, action: keymap::Action) {
    match action {
        keymap::Action::Submit => submit_comment(app, i),
        keymap::Action::Back => {
            if let Some(d) = detail_view_mut(app, i) {
                d.comment_input = None;
            }
        }
        // Navigation and other contexts' actions never resolve to
        // `CommentInput`'s table; the match stays exhaustive over `Action`
        // regardless.
        _ => {}
    }
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

/// Forward an unbound key to the comment buffer (same editing model as the
/// new-issue description field: cursor always at the end). `Submit`/`Back`
/// are the keymap's (`apply_comment_input`); everything else lands here
/// verbatim, using the original crossterm event so the widget sees its exact
/// `KeyCode`/`KeyModifiers`.
pub(crate) fn forward_comment_input(app: &mut App, i: usize, ev: KeyEvent) {
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let Some(detail) = detail_view_mut(app, i) else {
        return;
    };
    let Some(buf) = detail.comment_input.as_mut() else {
        return;
    };
    match ev.code {
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
