mod chrome;
mod detail;
mod help;
mod new_issue;
mod popup;
mod search;
mod table;
mod text_span;
mod util;

use chrome::{FooterState, render_footer, render_header, render_header_with_search};
use detail::{render_detail_footer, render_detail_overlay};
use help::render_help_popup;
use lt_runtime::query::SortField;
use new_issue::{render_new_issue_modal, submit_key_label};
use popup::{Popup, render_popup};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::Paragraph;
use search::{SortOrder, render_search_overlay};
use table::{popup_anchor, render_table};

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
        render_header_with_search(frame, chunks[0], &app.auth, overlay);
    } else {
        let context = match app.base() {
            View::List(list) => search_query::render_filter_context(&list.filter),
            _ => String::new(),
        };
        render_header(frame, chunks[0], &context, &app.auth);
    }

    // Render the spacer row between the issue table and the statusbar so the
    // terminal cell buffer is explicitly cleared (chunk[3]).
    frame.render_widget(Paragraph::new(""), chunks[3]);

    let (has_next, has_prev, page) = match app.base() {
        View::List(list) => (
            list.pagination.has_next_page,
            !list.pagination.cursor_stack.is_empty(),
            list.pagination.cursor_stack.len() + 1,
        ),
        _ => (false, false, 1),
    };

    let sync_label = sync_status_label(&app.sync, &app.auth, &app.clock);
    let footer = FooterState {
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
    } else if let Some(pending) = &app.pending_key {
        // Takes priority over the plain footer/footer_msg below: pending can
        // only be set from keymap contexts, and the Detail/comment branches
        // above already cover the Detail top-of-stack case.
        frame.render_widget(Paragraph::new(format!("{pending} …")), chunks[4]);
    } else if let Some(msg) = &app.footer_msg {
        frame.render_widget(Paragraph::new(format!("[!] {msg}")), chunks[4]);
    } else {
        render_footer(frame, chunks[4], footer);
    }
}

/// Render the whole view stack, bottom to top: the `List` arm fills the
/// full-frame table wherever it sits (the base is not special to the
/// renderer, only to the stack's never-empty invariant); each view above it
/// draws over what is beneath. Per-view render data the walk doesn't already
/// have -- the popup anchor, the search overlay's sort marker, the modal's
/// keyboard-enhanced flag -- is read/derived in the arm that needs it, not
/// hoisted above the walk where every other view would pay for it.
fn render_views(frame: &mut Frame, chunks: &[Rect], app: &mut App) {
    let len = app.views.len();
    let mut list_widths: Option<[usize; 7]> = None;
    let mut list_selected = 0usize;
    let mut list_sort_field = SortField::Updated;
    let mut list_sort_desc = true;

    for i in 0..len {
        match &mut app.views[i] {
            View::List(list) => {
                list_selected = list.table_state.selected().unwrap_or(0);
                list_sort_field = list.args.sort.clone();
                list_sort_desc = list.args.desc;
                list_widths = render_table(frame, chunks[2], list);
            }
            View::Detail(detail) => render_detail_overlay(frame, chunks[2], detail),
            View::Popup(popup) => {
                // The popup anchor rule: only when the popup sits directly on
                // the base list (an exact two-view stack) does the base
                // table's geometry anchor it; otherwise `render_popup`
                // centers.
                if len == 2
                    && i == 1
                    && let Some(widths) = &list_widths
                {
                    popup.anchor =
                        Some(popup_anchor(chunks[2], widths, list_selected, &popup.kind));
                }
                render_popup(
                    frame,
                    frame.area(),
                    &Popup {
                        anchor: popup.anchor,
                        kind: &popup.kind,
                        items: &popup.items,
                        selected: popup.selected,
                    },
                );
            }
            View::NewIssue(modal) => {
                render_new_issue_modal(frame, frame.area(), modal, app.session.keyboard_enhanced);
            }
            View::Help(popup) => render_help_popup(frame, frame.area(), popup),
            View::Search(overlay) => {
                let sort_order = SortOrder {
                    field: &list_sort_field,
                    desc: list_sort_desc,
                };
                render_search_overlay(frame, chunks, overlay, &sort_order);
            }
        }
    }
}
