use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lt_runtime::Runtime;
use lt_types::comments::{CommentCreateMutation, CommentCreateVariables};
use lt_types::detail::{IssueDetailQuery, IssueDetailVariables};
use lt_types::types::Issue;

use super::{App, Keymap, ScrollMotion, Unbound, View, keymap};

/// The detail pane's complete state, owned here rather than on `App`, and
/// populated by re-executing the one composed `IssueDetailQuery`
/// (docs/design/unified-execute-adr.md, "Decision 3") -- `vars` is the whole
/// data contract; there is no live subscription slot.
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
    vars: IssueDetailVariables,
}

impl DetailView {
    /// Re-execute issue/comments/children in one shot. A fresh read of
    /// `None` (the issue vanished locally) is idempotently ignored -- the
    /// pane keeps showing its last known state rather than blanking out.
    pub(crate) fn apply_update(&mut self, runtime: &Runtime) {
        match runtime.execute::<IssueDetailQuery>(self.vars.clone()) {
            Ok(Some(data)) => {
                self.issue = data.issue;
                self.comments = data.comments;
                self.children = data.children;
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "issue detail re-execute failed"),
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
    /// Open the detail pane for the currently selected issue: a cache-first
    /// `IssueDetailQuery` read populates issue/comments/children, and a
    /// one-shot background refresh brings the composed view's data up to
    /// date from upstream (docs/design/unified-execute-adr.md, "Decision 3").
    pub(crate) fn open_detail(&mut self) {
        let Some(issue) = self.selected_issue().cloned() else {
            return;
        };

        let detail = build_cached_detail(&issue, &self.runtime);
        self.push_view(View::Detail(Box::new(detail)));
    }
}

/// Build a detail view from a list `Issue`: a cache-first read of the
/// composed detail query, plus a one-shot background freshness refresh. A
/// `None` initial read (the id not yet in the local cache, an edge case
/// since the pane opens from an already-listed issue) falls back to the
/// issue already in hand, with empty comments/children.
pub(crate) fn build_cached_detail(issue: &Issue, runtime: &Runtime) -> DetailView {
    let vars = IssueDetailVariables {
        id: issue.id.inner().to_string(),
    };
    let data = runtime
        .execute::<IssueDetailQuery>(vars.clone())
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "issue detail initial read failed");
            None
        });
    let (issue, comments, children) = match data {
        Some(data) => (data.issue, data.comments, data.children),
        None => (issue.clone(), Vec::new(), Vec::new()),
    };
    runtime.refresh::<IssueDetailQuery>(vars.clone());
    DetailView {
        issue,
        comments,
        children,
        scroll: 0,
        comment_input: None,
        vars,
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
    if let Err(e) = app
        .runtime
        .execute::<CommentCreateMutation>(CommentCreateVariables { input })
    {
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
