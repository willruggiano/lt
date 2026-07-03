mod chrome;
mod detail;
mod help;
mod new_issue;
mod popup;
mod search;
mod table;
mod text_span;
mod util;

use chrome::{FooterState, Identity, render_footer, render_header, render_header_with_search};
use detail::{render_detail_footer, render_detail_overlay};
use help::render_help_popup;
use new_issue::{render_new_issue_modal, submit_key_label};
use popup::{Popup, render_popup};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::Paragraph;
use search::{SortOrder, render_search_overlay};
use table::render_table;

use crate::{App, View, search_query};

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

    let context = search_query::render_filter_context(&app.active_filter);

    // Always render the header with user/org context. In search mode, append
    // the search query inline so the identity is always visible.
    let identity = Identity {
        viewer_name: app.viewer_name.as_deref(),
        org_name: app.org_name.as_deref(),
    };
    if let Some(View::Search(overlay)) = app.views.last() {
        render_header_with_search(frame, chunks[0], &identity, overlay);
    } else {
        render_header(frame, chunks[0], &context, &identity);
    }

    // Always render the full-width base view (today: the table) so column
    // widths never change.
    render_table(frame, chunks[2], app);

    // Render the spacer row between the issue table and the statusbar so the
    // terminal cell buffer is explicitly cleared (chunk[3]).
    frame.render_widget(Paragraph::new(""), chunks[3]);

    let base_list = app.base_list();
    let has_next = base_list.is_some_and(|l| l.pagination.has_next_page);
    let has_prev = base_list.is_some_and(|l| !l.pagination.cursor_stack.is_empty());
    let page = base_list.map_or(1, |l| l.pagination.cursor_stack.len() + 1);

    let sync_label = app.sync.sync_status_label.clone();
    let footer = FooterState {
        has_next,
        has_prev,
        page,
        sync_label: &sync_label,
    };
    render_status_row(frame, &chunks, app, &footer);

    render_overlays(frame, &chunks, app);
}

/// Render the bottom status row (chunk 4), which switches between the detail
/// footer, a transient footer message, and the list footer, matching on the
/// stack top.
fn render_status_row(frame: &mut Frame, chunks: &[Rect], app: &App, footer: &FooterState) {
    if let Some(View::Detail(d)) = app.views.last() {
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
            render_detail_footer(frame, chunks[4]);
        }
    } else if let Some(msg) = &app.footer_msg {
        frame.render_widget(Paragraph::new(format!("[!] {msg}")), chunks[4]);
    } else {
        render_footer(frame, chunks[4], footer);
    }
}

/// Render every view above the base, bottom to top.
fn render_overlays(frame: &mut Frame, chunks: &[Rect], app: &mut App) {
    let keyboard_enhanced = app.session.keyboard_enhanced;
    let sort_order = SortOrder {
        field: &app.args.sort,
        desc: app.args.desc,
    };

    for view in app.views.iter_mut().skip(1) {
        match view {
            View::List(_) => {} // unreachable above the base in this stage
            View::Detail(detail) => render_detail_overlay(frame, chunks[2], detail),
            View::Popup(popup) => render_popup(
                frame,
                frame.area(),
                &Popup {
                    anchor: popup.anchor,
                    kind: &popup.kind,
                    items: &popup.items,
                    selected: popup.selected,
                },
            ),
            View::NewIssue(modal) => {
                render_new_issue_modal(frame, frame.area(), modal, keyboard_enhanced);
            }
            View::Help(popup) => render_help_popup(frame, frame.area(), popup),
            View::Search(overlay) => render_search_overlay(frame, chunks, overlay, &sort_order),
        }
    }
}
