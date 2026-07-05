use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lt_runtime::{Runtime, SubId, Subscription};
use lt_types::detail::{IssueDetailData, IssueDetailQuery, IssueDetailVariables};
use lt_types::types::Issue;

use super::{App, Keymap, ScrollMotion, Unbound, View, keymap};

/// The detail pane's complete state, owned here rather than on `App`, and
/// populated by the one composed `IssueDetailQuery` subscription
/// (docs/design/operation-seam-adr.md, "Decision 3").
pub struct DetailView {
    pub issue: Issue,
    pub comments: Vec<lt_types::comments::Comment>,
    pub children: Vec<Issue>,
    /// Vertical scroll offset inside the detail pane (in lines).
    pub scroll: u16,
    /// Multiline buffer for a new comment, open in the detail pane. The
    /// cursor is always at the end (same model as the new-issue description
    /// field).
    pub comment_input: Option<String>,
    sub: Subscription<Option<IssueDetailData>>,
}

impl DetailView {
    /// A matching subscription update re-reads issue/comments/children in
    /// one shot. A fresh read of `None` (the issue vanished locally) is
    /// idempotently ignored -- the pane keeps showing its last known state
    /// rather than blanking out.
    pub(crate) fn apply_update(&mut self, id: SubId) {
        if self.sub.id() == id
            && let Some(Some(data)) = self.sub.take()
        {
            self.issue = data.issue;
            self.comments = data.comments;
            self.children = data.children;
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
    /// Open the detail pane for the currently selected issue: one
    /// `IssueDetailQuery` subscription populates issue/comments/children from
    /// its synchronous cache-first read.
    pub(crate) fn open_detail(&mut self) {
        let Some(issue) = self.selected_issue().cloned() else {
            return;
        };

        let detail = build_cached_detail(&issue, &self.runtime);
        self.push_view(View::Detail(Box::new(detail)));
    }
}

/// Build a detail view from a list `Issue`, subscribing the composed detail
/// query. A `None` initial read (the id not yet in the local cache, an
/// edge case since the pane opens from an already-listed issue) falls back
/// to the issue already in hand, with empty comments/children.
pub(crate) fn build_cached_detail(issue: &Issue, runtime: &Runtime) -> DetailView {
    let (sub, data) = runtime.subscribe::<IssueDetailQuery>(IssueDetailVariables {
        id: issue.id.inner().to_string(),
    });
    let (issue, comments, children) = match data {
        Some(data) => (data.issue, data.comments, data.children),
        None => (issue.clone(), Vec::new(), Vec::new()),
    };
    DetailView {
        issue,
        comments,
        children,
        scroll: 0,
        comment_input: None,
        sub,
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
