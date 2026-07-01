mod chrome;
mod detail;
mod help;
mod new_issue;
mod popup;
mod search;
mod table;
mod text_span;
mod util;

use chrome::{
    FooterState, Identity, render_footer, render_header, render_header_with_search, render_input,
};
use detail::{render_detail_footer, render_detail_overlay};
use help::render_help_popup;
use new_issue::{render_new_issue_modal, submit_key_label};
use popup::{Popup, render_popup};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::Paragraph;
use search::{SortOrder, render_search_overlay};
use table::render_table;

use crate::{App, Mode, search_query};

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
    let has_next = app.pagination.has_next_page;
    let has_prev = !app.pagination.cursor_stack.is_empty();
    let page = app.pagination.cursor_stack.len() + 1;

    // Always render the header with user/org context. In search mode, append
    // the search query inline so the identity is always visible.
    let identity = Identity {
        viewer_name: app.viewer_name.as_deref(),
        org_name: app.org_name.as_deref(),
    };
    if let Mode::Search = app.mode
        && let Some(ref overlay) = app.search_overlay
    {
        render_header_with_search(frame, chunks[0], &identity, overlay);
    } else {
        render_header(frame, chunks[0], &context, &identity);
    }

    // Always render the full-width table so column widths never change.
    render_table(frame, chunks[2], app);

    // Render the spacer row between the issue table and the statusbar so the
    // terminal cell buffer is explicitly cleared (chunk[3]).
    frame.render_widget(Paragraph::new(""), chunks[3]);

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
/// footer, the filter input, a transient footer message, and the list footer.
fn render_status_row(frame: &mut Frame, chunks: &[Rect], app: &App, footer: &FooterState) {
    if let Mode::Detail = app.mode {
        if app.comment_input.is_some() {
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
    } else if app.input_mode {
        render_input(frame, chunks[4], &app.input_buf);
    } else if let Some(msg) = &app.footer_msg {
        frame.render_widget(Paragraph::new(format!("[!] {msg}")), chunks[4]);
    } else {
        render_footer(frame, chunks[4], footer);
    }
}

/// Render any active mode overlay on top of the base list/header/footer.
fn render_overlays(frame: &mut Frame, chunks: &[Rect], app: &mut App) {
    // Render detail overlay on top if active.
    if let Mode::Detail = app.mode {
        render_detail_overlay(frame, chunks[2], app);
    }

    // Render popup on top if active.
    if let Mode::Popup(ref kind) = app.mode {
        render_popup(
            frame,
            frame.area(),
            &Popup {
                anchor: app.popup_anchor,
                kind,
                items: &app.popup_items,
                selected: app.popup_selected,
            },
        );
    }

    // Render new-issue modal on top if active.
    if let Mode::NewIssue = app.mode
        && let Some(ref modal) = app.new_issue_modal
    {
        render_new_issue_modal(frame, frame.area(), modal, app.session.keyboard_enhanced);
    }

    // Render help popup on top if active.
    if let Mode::Help = app.mode
        && let Some(ref popup) = app.help_popup
    {
        render_help_popup(frame, frame.area(), popup);
    }

    // Render FTS search overlay.
    if let Mode::Search = app.mode
        && let Some(ref mut overlay) = app.search_overlay
    {
        render_search_overlay(
            frame,
            chunks,
            overlay,
            &SortOrder {
                field: &app.args.sort,
                desc: app.args.desc,
            },
        );
    }
}
