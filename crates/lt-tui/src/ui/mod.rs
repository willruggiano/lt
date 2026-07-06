mod chrome;
mod detail;
mod help;
mod new_issue;
mod popup;
mod search;
pub(crate) mod table;
mod text_span;
mod util;

use chrome::{Footer, Header, HeaderWithSearch};
use lt_runtime::query::{SortDirection, SortField};
use new_issue::{NewIssueForm, submit_key_label};
use popup::Popup;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::Paragraph;
use search::{SearchResults, SortOrder};

use crate::list::TableGeometry;
use crate::{App, View, search_query, sync_status_label};

pub fn render(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1), // spacer
        Constraint::Length(1),
    ])
    .split(frame.area());

    // Expose visible row count to key handlers (subtract table header row).
    app.viewport_height = chunks[2].height.saturating_sub(1);

    // Always render the header with user/org context. In search mode, append
    // the search query inline so the identity is always visible.
    if let Some(View::Search(overlay)) = app.views.last() {
        frame.render_widget(
            &HeaderWithSearch {
                auth: &app.auth,
                overlay,
            },
            chunks[0],
        );
    } else {
        let context = match app.base() {
            View::List(list) => search_query::render_filter_context(&list.query.filter),
            _ => String::new(),
        };
        frame.render_widget(
            &Header {
                context: &context,
                auth: &app.auth,
            },
            chunks[0],
        );
    }

    // Render the spacer row between the issue table and the statusbar so the
    // terminal cell buffer is explicitly cleared (chunk[3]).
    frame.render_widget(Paragraph::new(""), chunks[3]);

    let (has_next, has_prev, page) = match app.base() {
        View::List(list) => (
            list.query.pagination.has_next_page,
            !list.query.pagination.cursor_stack.is_empty(),
            list.query.pagination.cursor_stack.len() + 1,
        ),
        _ => (false, false, 1),
    };

    let sync_label = sync_status_label(&app.sync, &app.auth, &app.clock);
    let footer = Footer {
        has_next,
        has_prev,
        page,
        sync_label: &sync_label,
    };
    render_status_row(frame, &chunks, app, &footer);

    render_views(frame, &chunks, app);
}

/// Render the bottom status row (chunk 4), which switches between the detail
/// footer, a transient footer message, the pending-chord indicator, and the
/// list footer, matching on the stack top.
fn render_status_row(frame: &mut Frame, chunks: &[Rect], app: &App, footer: &Footer) {
    if let Some(pending) = &app.pending_key {
        // Highest priority: a pending prefix can never coexist with the
        // comment-input hint below, since text contexts never start chords.
        frame.render_widget(Paragraph::new(format!("{pending} …")), chunks[4]);
    } else if let Some(View::Detail(d)) = app.views.last() {
        if d.comment_input.is_some() {
            frame.render_widget(
                Paragraph::new(format!(
                    "Enter newline  {} submit  Esc cancel",
                    submit_key_label(app.session.keyboard_enhanced)
                )),
                chunks[4],
            );
        } else if let Some(msg) = &app.footer_msg {
            frame.render_widget(Paragraph::new(format!("[!] {msg}")), chunks[4]);
        } else {
            frame.render_widget(Paragraph::new(detail::footer_hint()), chunks[4]);
        }
    } else if let Some(msg) = &app.footer_msg {
        frame.render_widget(Paragraph::new(format!("[!] {msg}")), chunks[4]);
    } else {
        frame.render_widget(footer, chunks[4]);
    }
}

/// Render the whole view stack, bottom to top, each drawing over what's
/// beneath. Per-view data is derived in the arm that needs it, not hoisted
/// where every view would pay for it.
fn render_views(frame: &mut Frame, chunks: &[Rect], app: &mut App) {
    let len = app.views.len();
    let mut list_geometry: Option<TableGeometry> = None;
    let mut list_order = crate::list::SortOrder {
        field: SortField::Updated,
        direction: SortDirection::Descending,
    };

    for i in 0..len {
        match &mut app.views[i] {
            View::List(list) => {
                list_order = list.query.order.clone();
                list_geometry = list.render_table(chunks[2], frame.buffer_mut());
            }
            View::Detail(detail) => frame.render_widget(detail.as_ref(), chunks[2]),
            View::Popup(popup) => {
                // Anchor to the base table's geometry only when the popup
                // sits directly on it (an exact two-view stack); otherwise
                // the popup widget centers.
                let base = (len == 2 && i == 1)
                    .then_some(list_geometry.as_ref())
                    .flatten();
                frame.render_widget(
                    &Popup {
                        base,
                        kind: &popup.kind,
                        items: &popup.items,
                        selected: popup.selected,
                    },
                    frame.area(),
                );
            }
            View::NewIssue(modal) => {
                frame.render_widget(
                    &NewIssueForm {
                        modal,
                        keyboard_enhanced: app.session.keyboard_enhanced,
                    },
                    frame.area(),
                );
            }
            View::Help(popup) => frame.render_widget(&*popup, frame.area()),
            View::Search(overlay) => {
                let sort = SortOrder {
                    field: &list_order.field,
                    direction: list_order.direction,
                };
                frame.render_widget(&mut SearchResults { overlay, sort }, chunks[2]);
            }
        }
    }
}
