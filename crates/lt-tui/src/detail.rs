use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lt_runtime::db::Database;
use lt_runtime::{Runtime, SubId, Subscription};
use lt_types::comments::{CommentConnection, CommentsQuery, CommentsVariables};
use lt_types::types::Issue;

use super::{App, Keymap, ScrollMotion, Unbound, View, keymap};

/// The detail pane's complete state, owned here rather than on `App`.
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
    comments_sub: Subscription<CommentConnection>,
}

impl DetailView {
    /// A matching subscription update re-reads the comment thread. The
    /// issue/parent/children stay direct cache-first reads until the
    /// composed detail operation (Task 4) exists.
    pub(crate) fn apply_update(&mut self, id: SubId) {
        if self.comments_sub.id() == id
            && let Some(page) = self.comments_sub.take()
        {
            self.comments = page.nodes;
        }
    }

    /// Offset scrolling over the shared motion set.
    pub(crate) fn scroll(&mut self, motion: ScrollMotion, viewport_height: u16) {
        self.scroll = motion.apply_offset(self.scroll, viewport_height);
    }

    /// This pane's declared keymap: the open comment input narrows to its
    /// own keymap (`esc` cancels the draft rather than popping the pane);
    /// otherwise the pane's own keymap.
    pub(crate) fn keymap(&self) -> &'static Keymap {
        if self.comment_input.is_some() {
            &COMMENT_INPUT_KEYMAP
        } else {
            &DETAIL_KEYMAP
        }
    }
}

impl App {
    /// Open the detail pane for the currently selected issue, populated
    /// instantly from local data; the comment thread subscribes live.
    pub(crate) fn open_detail(&mut self) {
        let Some(issue) = self.selected_issue().cloned() else {
            return;
        };

        let mut detail = build_cached_detail(&issue, &self.runtime);
        populate_relations(&self.db, &mut detail, &issue);

        self.push_view(View::Detail(Box::new(detail)));
    }
}

/// Build a detail view from a list `Issue`, subscribing its comment thread
/// (cache-first: the subscription's synchronous initial read populates
/// `comments`). Parent/children are left empty; `populate_relations` fills
/// them in.
pub(crate) fn build_cached_detail(issue: &Issue, runtime: &Runtime) -> DetailView {
    let (comments_sub, page) = runtime.subscribe::<CommentsQuery>(CommentsVariables {
        id: issue.id.inner().to_string(),
        after: None,
    });
    DetailView {
        issue: issue.clone(),
        comments: page.nodes,
        parent: None,
        children: Vec::new(),
        scroll: 0,
        comment_input: None,
        comments_sub,
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
    match lt_runtime::db::query_children(&conn, issue.id.inner()) {
        Ok(children) => detail.children = children,
        Err(e) => {
            tracing::warn!(error = %e, issue_id = issue.id.inner(), "detail pane: failed to query children");
        }
    }
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

// -- Detail pane keybindings -------------------------------------------------

pub(crate) static DETAIL_BINDINGS: keymap::Table = &[
    (
        keymap::Binding::Single(keymap::Key::char('c')),
        keymap::Action::Comment,
    ),
    (
        keymap::Binding::Chord(keymap::Key::char('o'), keymap::Key::char('b')),
        keymap::Action::OpenInBrowser,
    ),
];

pub(crate) static DETAIL_KEYMAP: Keymap = Keymap {
    layers: &[DETAIL_BINDINGS, keymap::GLOBAL],
    apply: Some(apply_detail),
    unbound: Unbound::Cascade,
};

/// The detail pane's comment box: the one keymap that binds `esc` -- narrower
/// than the floor's pop (cancels the draft, keeps the pane open).
pub(crate) static COMMENT_INPUT_BINDINGS: keymap::Table = &[
    (
        keymap::Binding::Single(keymap::Key::ctrl_code(KeyCode::Enter)),
        keymap::Action::Submit,
    ),
    (
        keymap::Binding::Single(keymap::Key::alt(KeyCode::Enter)),
        keymap::Action::Submit,
    ),
    (
        keymap::Binding::Single(keymap::Key::plain(KeyCode::Esc)),
        keymap::Action::Back,
    ),
];

pub(crate) static COMMENT_INPUT_KEYMAP: Keymap = Keymap {
    layers: &[COMMENT_INPUT_BINDINGS],
    apply: Some(apply_comment_input),
    unbound: Unbound::Forward(forward_comment_input),
};

/// The detail pane's non-navigation actions.
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
        // Other keymaps' actions never resolve here; kept exhaustive over
        // `Action` regardless.
        _ => {}
    }
}

/// The comment box's actions: `Back` cancels the draft without popping the
/// pane beneath it.
pub(crate) fn apply_comment_input(app: &mut App, i: usize, action: keymap::Action) {
    match action {
        keymap::Action::Submit => submit_comment(app, i),
        keymap::Action::Back => {
            if let Some(d) = detail_view_mut(app, i) {
                d.comment_input = None;
            }
        }
        // Other keymaps' actions never resolve here; kept exhaustive over
        // `Action` regardless.
        _ => {}
    }
}

fn detail_view_mut(app: &mut App, i: usize) -> Option<&mut DetailView> {
    app.view_at_mut(i, |v| match v {
        View::Detail(d) => Some(d.as_mut()),
        _ => None,
    })
}

/// Enqueue the comment buffer as a local create: the optimistic `local:` row
/// plus a `commentCreate` outbox command, in one transaction. A failure
/// surfaces in the footer.
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
    if let Err(e) = app.runtime.create_comment(&input) {
        app.footer_msg = Some(format!("Failed to save comment: {e}"));
    }
}

/// Forward an unbound key to the comment buffer verbatim, using the raw
/// crossterm event so the widget sees the exact `KeyCode`/`KeyModifiers`.
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
