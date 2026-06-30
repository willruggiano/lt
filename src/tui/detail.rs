use std::sync::mpsc;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers};

use super::{App, CommentSyncEvent, Issue, Mode, Status};
use crate::db::Database;
use crate::linear::client::HttpTransport;

impl App {
    /// Open the detail pane for the currently selected issue.
    ///
    /// The detail is populated instantly from the local SQLite cache so the
    /// pane appears without any network round-trip.  A background thread then
    /// calls `sync_comments` via the Linear API and sends the refreshed comment
    /// list back through `detail_comment_rx`.
    pub(crate) fn open_detail(&mut self) {
        let issue = match self.selected_issue() {
            Some(i) => i.clone(),
            None => return,
        };

        self.mode = Mode::Detail;
        self.detail_scroll = 0;
        self.detail_comment_rx = None;

        // Build an IssueDetail immediately from cached data.
        let cached_comments: Vec<crate::linear::types::Comment> = self
            .db
            .connect()
            .and_then(|conn| crate::db::query_comments(&conn, &issue.id))
            .unwrap_or_default()
            .into_iter()
            .map(Into::into)
            .collect();

        self.detail = Some(build_cached_detail(&issue, cached_comments));

        // Populate parent and children from the local DB cache.
        if let Some(ref mut detail) = self.detail {
            populate_relations(&self.db, detail, &issue);
        }

        self.status = Status::Idle;

        // Spawn background thread to refresh comments from the Linear API.
        let issue_id = issue.id.clone();
        let (tx, rx) = std::sync::mpsc::channel::<CommentSyncEvent>();
        self.detail_comment_rx = Some(rx);

        std::thread::spawn(move || {
            let Ok(Some(token)) = crate::config::load_token() else {
                let _ = tx.send(CommentSyncEvent::Error("not logged in".to_string()));
                return;
            };
            let conn = match crate::db::db_path().and_then(crate::db::open_db) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(CommentSyncEvent::Error(e.to_string()));
                    return;
                }
            };
            match crate::sync::comments::sync_comments(
                &conn,
                &HttpTransport::new(token.access_token),
                &issue_id,
            ) {
                Ok(()) => {
                    // Read the freshly-synced comments back from the DB.
                    let fresh = crate::db::query_comments(&conn, &issue_id)
                        .unwrap_or_default()
                        .into_iter()
                        .map(Into::into)
                        .collect();
                    let _ = tx.send(CommentSyncEvent::Done(fresh));
                }
                Err(e) => {
                    let _ = tx.send(CommentSyncEvent::Error(e.to_string()));
                }
            }
        });
    }

    /// Close the detail pane and return to the list.
    pub(crate) fn close_detail(&mut self) {
        self.mode = Mode::List;
        self.detail = None;
        self.detail_scroll = 0;
        self.comment_input = None;
        self.status = Status::Idle;
        // Drop the background comment-sync receiver so the thread stops being
        // polled and will be GC'd once it finishes its network request.
        self.detail_comment_rx = None;
    }

    pub(crate) fn detail_scroll_down(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_add(1);
    }

    pub(crate) fn detail_scroll_up(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_sub(1);
    }

    pub(crate) fn detail_scroll_to_top(&mut self) {
        self.detail_scroll = 0;
    }

    pub(crate) fn detail_scroll_to_bottom(&mut self) {
        // Ratatui clamps scroll to content length; use a large sentinel.
        self.detail_scroll = u16::MAX;
    }

    pub(crate) fn detail_scroll_half_page_down(&mut self) {
        self.detail_scroll_by((self.viewport_height / 2).max(1), true);
    }

    pub(crate) fn detail_scroll_half_page_up(&mut self) {
        self.detail_scroll_by((self.viewport_height / 2).max(1), false);
    }

    pub(crate) fn detail_scroll_page_down(&mut self) {
        self.detail_scroll_by(self.viewport_height.max(1), true);
    }

    pub(crate) fn detail_scroll_page_up(&mut self) {
        self.detail_scroll_by(self.viewport_height.max(1), false);
    }

    /// Scroll the detail pane by `step` rows, `down` toward the bottom.
    pub(crate) fn detail_scroll_by(&mut self, step: u16, down: bool) {
        self.detail_scroll = if down {
            self.detail_scroll.saturating_add(step)
        } else {
            self.detail_scroll.saturating_sub(step)
        };
    }

    // -- Comment input ---------------------------------------------------------

    /// Submit the comment buffer to the Linear API.
    ///
    /// The comment is appended to the detail pane optimistically; a background
    /// thread runs the commentCreate mutation, re-syncs the issue's comments,
    /// and delivers the authoritative list via `detail_comment_rx`.  On error
    /// the optimistic comment is dropped (see `poll_detail_comment_events`).
    pub(crate) fn submit_comment(&mut self) {
        let body = match self.comment_input.as_ref() {
            Some(b) => b.trim().to_string(),
            None => return,
        };
        if body.is_empty() {
            self.comment_input = None;
            return;
        }
        let issue_id = match self.selected_issue() {
            Some(i) => i.id.clone(),
            None => return,
        };
        let Ok(Some(token)) = crate::config::load_token() else {
            self.footer_msg = Some("Not logged in".to_string());
            return;
        };
        self.comment_input = None;

        // Optimistic: show the comment immediately.
        if let Some(ref mut detail) = self.detail {
            detail.comments.nodes.push(crate::linear::types::Comment {
                body: body.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
                user: self
                    .viewer_name
                    .clone()
                    .map(|name| crate::linear::types::CommentUser { name }),
            });
        }

        let (tx, rx) = mpsc::channel::<CommentSyncEvent>();
        self.detail_comment_rx = Some(rx);

        std::thread::spawn(move || {
            let result = (|| -> Result<Vec<crate::linear::types::Comment>> {
                let transport = HttpTransport::new(token.access_token);
                crate::linear::mutations::create_comment(&transport, &issue_id, &body)?;
                let conn = crate::db::open_db(crate::db::db_path()?)?;
                crate::sync::comments::sync_comments(&conn, &transport, &issue_id)?;
                Ok(crate::db::query_comments(&conn, &issue_id)?
                    .into_iter()
                    .map(Into::into)
                    .collect())
            })();
            match result {
                Ok(fresh) => {
                    let _ = tx.send(CommentSyncEvent::Done(fresh));
                }
                Err(e) => {
                    let _ = tx.send(CommentSyncEvent::PostError(e.to_string()));
                }
            }
        });
    }
}

/// Build an `IssueDetail` from a cached list `Issue` plus its cached comments.
pub(crate) fn build_cached_detail(
    issue: &Issue,
    cached_comments: Vec<crate::linear::types::Comment>,
) -> crate::linear::types::IssueDetail {
    crate::linear::types::IssueDetail {
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        description: issue.description.clone(),
        priority_label: issue.priority_label.clone(),
        state: crate::linear::types::IssueDetailState {
            name: issue.state.name.clone(),
        },
        assignee: issue
            .assignee
            .as_ref()
            .map(|a| crate::linear::types::IssueDetailUser {
                name: a.name.clone(),
            }),
        team: crate::linear::types::IssueDetailTeam {
            name: issue.team.name.clone(),
        },
        labels: crate::linear::types::LabelConnection {
            nodes: issue
                .labels
                .nodes
                .iter()
                .map(|l| crate::linear::types::Label {
                    id: l.id.clone(),
                    name: l.name.clone(),
                })
                .collect(),
        },
        created_at: issue.created_at.clone(),
        updated_at: issue.updated_at.clone(),
        comments: crate::linear::types::CommentConnection {
            nodes: cached_comments,
        },
        parent: None,
        children: Vec::new(),
    }
}

/// Populate a detail's parent/children fields from the local DB cache.
pub(crate) fn populate_relations(
    db: &Database,
    detail: &mut crate::linear::types::IssueDetail,
    issue: &Issue,
) {
    let Ok(conn) = db.connect() else {
        return;
    };
    // Look up children.
    if let Ok(children) = crate::db::query_children(&conn, &issue.id) {
        detail.children = children
            .into_iter()
            .map(|c| crate::linear::types::IssueRef {
                identifier: c.identifier,
                title: c.title,
                state_name: c.state_name,
            })
            .collect();
    }
    // Look up parent.
    if let Some(ref parent) = issue.parent {
        let parent_sql = "SELECT identifier, title, state_name FROM issues WHERE id = ?1";
        if let Ok(mut stmt) = conn.prepare(parent_sql)
            && let Ok(row) = stmt.query_row(rusqlite::params![parent.id], |row| {
                Ok(crate::linear::types::IssueRef {
                    identifier: row.get(0)?,
                    title: row.get(1)?,
                    state_name: row.get(2)?,
                })
            })
        {
            detail.parent = Some(row);
        }
    }
}

/// Non-blocking poll of the background comment-sync channel.
///
/// When the background thread finishes syncing comments from the Linear API,
/// the refreshed list replaces the cached comments shown in the detail pane.
pub(crate) fn poll_detail_comment_events(app: &mut App) {
    let Some(rx) = app.detail_comment_rx.take() else {
        return;
    };

    let finished = match rx.try_recv() {
        Ok(CommentSyncEvent::Done(comments)) => {
            if let Some(ref mut detail) = app.detail {
                detail.comments.nodes = comments;
            }
            true
        }
        Ok(CommentSyncEvent::Error(_msg)) => {
            // Non-fatal: keep whatever cached comments are already shown.
            true
        }
        Ok(CommentSyncEvent::PostError(msg)) => {
            // Posting failed: drop the optimistic comment by reloading the
            // cached set, and surface the error in the footer.
            let cached = app.selected_issue().map(|i| i.id.clone()).and_then(|id| {
                crate::db::db_path()
                    .and_then(crate::db::open_db)
                    .and_then(|conn| crate::db::query_comments(&conn, &id))
                    .ok()
            });
            if let (Some(detail), Some(comments)) = (app.detail.as_mut(), cached) {
                detail.comments.nodes = comments.into_iter().map(Into::into).collect();
            }
            app.footer_msg = Some(format!("Failed to post comment: {msg}"));
            true
        }
        Err(mpsc::TryRecvError::Empty) => false,
        Err(mpsc::TryRecvError::Disconnected) => true,
    };

    if !finished {
        app.detail_comment_rx = Some(rx);
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

pub(crate) fn handle_detail_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);

    // When the comment input is open, all keys go to it.
    if app.comment_input.is_some() {
        handle_comment_input_key(app, code, modifiers);
        return;
    }

    match code {
        KeyCode::Esc | KeyCode::Char('q') => app.close_detail(),
        // Open the comment input.
        KeyCode::Char('c') => {
            app.comment_input = Some(String::new());
            app.footer_msg = None;
        }
        KeyCode::Char('j') | KeyCode::Down => app.detail_scroll_down(),
        KeyCode::Char('k') | KeyCode::Up => app.detail_scroll_up(),
        KeyCode::Char('g') => app.detail_scroll_to_top(),
        KeyCode::Char('G') => app.detail_scroll_to_bottom(),
        KeyCode::Char('d') if ctrl => app.detail_scroll_half_page_down(),
        KeyCode::Char('u') if ctrl => app.detail_scroll_half_page_up(),
        KeyCode::PageDown => app.detail_scroll_page_down(),
        KeyCode::PageUp => app.detail_scroll_page_up(),
        KeyCode::Char('o') => {
            if let Some(detail) = &app.detail {
                let url = format!("https://linear.app/issue/{}", detail.identifier);
                let _ = open::that(url);
            }
        }
        _ => {}
    }
}

/// Key handling for the comment input box (same editing model as the
/// new-issue description field: cursor always at the end).
fn handle_comment_input_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);

    // Ctrl-Enter submits (Alt-Enter on terminals that cannot distinguish
    // Ctrl-Enter from Enter).
    if (ctrl || alt) && code == KeyCode::Enter {
        app.submit_comment();
        return;
    }
    // Esc cancels.
    if code == KeyCode::Esc {
        app.comment_input = None;
        return;
    }

    let Some(buf) = app.comment_input.as_mut() else {
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
