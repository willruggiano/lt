use std::sync::{Arc, mpsc};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lt_runtime::db::Database;
use lt_types::types::Issue;

use super::{App, CommentSyncEvent, KeyFlow, Status, View};

/// The detail pane's complete state: the shared `types`/`comments` fragments
/// the TUI composes for display, plus the panel's scroll offset and comment
/// draft (owned here, not on `App`) and the background comment-sync
/// receiver, carried here for the interim between the view-stack restructure
/// and the app-event queue.
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
    /// Receiver for background comment-sync events.
    pub detail_comment_rx: Option<mpsc::Receiver<CommentSyncEvent>>,
}

impl DetailView {
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
    /// pane appears without any network round-trip.  A background thread then
    /// calls `sync_comments` via the Linear API and sends the refreshed comment
    /// list back through `detail_comment_rx`.
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

        // Spawn background thread to refresh comments through the sync service,
        // then re-read them from the local DB.
        let issue_id = issue.id.into_inner();
        let service = Arc::clone(&self.service);
        let (tx, rx) = mpsc::channel::<CommentSyncEvent>();
        detail.detail_comment_rx = Some(rx);

        std::thread::spawn(move || match service.sync_comments(&issue_id) {
            Ok(()) => {
                let fresh = lt_runtime::db::db_path()
                    .and_then(lt_runtime::db::open_db)
                    .and_then(|conn| lt_runtime::db::query_comments(&conn, &issue_id))
                    .unwrap_or_default();
                let _ = tx.send(CommentSyncEvent::Done(fresh));
            }
            Err(e) => {
                let _ = tx.send(CommentSyncEvent::Error(e.to_string()));
            }
        });

        self.views.push(View::Detail(Box::new(detail)));
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
        detail_comment_rx: None,
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

/// Non-blocking poll of the background comment-sync channel. The receiver
/// lives on whichever `DetailView` is in the stack -- there is at most one.
///
/// When the background thread finishes syncing comments from the Linear API,
/// the refreshed list replaces the cached comments shown in the detail pane.
pub(crate) fn poll_detail_comment_events(app: &mut App) {
    let Some(detail) = app.views.iter_mut().find_map(|v| match v {
        View::Detail(d) => Some(d),
        _ => None,
    }) else {
        return;
    };
    let Some(rx) = detail.detail_comment_rx.take() else {
        return;
    };

    let finished = match rx.try_recv() {
        Ok(CommentSyncEvent::Done(comments)) => {
            detail.comments = comments;
            true
        }
        Ok(CommentSyncEvent::Error(_msg)) => {
            // Non-fatal: keep whatever cached comments are already shown.
            true
        }
        Err(mpsc::TryRecvError::Empty) => false,
        Err(mpsc::TryRecvError::Disconnected) => true,
    };

    if !finished {
        detail.detail_comment_rx = Some(rx);
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

/// Enqueue the comment buffer as a local create.
///
/// The comment is appended to the detail pane optimistically and written to
/// the local DB (an optimistic `local:` row plus a `commentCreate` outbox
/// command) in one transaction. No network: the sync drainer posts it and
/// reconciles the temp row with the server copy.
fn submit_comment(app: &mut App, i: usize) {
    // Copied out before borrowing `detail`: `detail_view_mut` takes `&mut
    // App`, so the returned borrow covers all of `app` for its lifetime.
    let viewer_name = app.viewer_name.clone();

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

    // Optimistic: show the comment immediately in the open detail pane.
    let now = lt_types::scalars::DateTime(chrono::Utc::now());
    detail.comments.push(lt_types::comments::Comment {
        id: lt_runtime::db::outbox::temp_id().into(),
        body: body.clone(),
        created_at: now,
        updated_at: now,
        user: viewer_name.map(|name| lt_types::types::User {
            id: String::new().into(),
            name,
        }),
        issue_id: Some(issue_id.clone()),
    });

    let input = lt_types::inputs::CommentCreateInput {
        issue_id: issue_id.clone(),
        body: body.clone(),
    };
    if let Ok(conn) = lt_runtime::db::db_path().and_then(lt_runtime::db::open_db) {
        let _ = lt_runtime::db::outbox::enqueue_comment_create(
            &conn,
            &lt_runtime::db::outbox::temp_id(),
            &input,
        );
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
