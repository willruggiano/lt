use lt_runtime::query::SortField;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Clear, Paragraph};

use super::table::{row_cells, sort_col_index};
use super::util::{TableSpec, render_issue_table};
use crate::SearchOverlay;

/// Active sort field and direction.
pub(super) struct SortOrder<'a> {
    pub(super) field: &'a SortField,
    pub(super) desc: bool,
}

pub(super) fn render_search_overlay(
    frame: &mut Frame,
    chunks: &[Rect],
    overlay: &mut SearchOverlay,
    sort: &SortOrder,
) {
    // This function only handles the results in the main content area (chunks[2]).
    let area = chunks[2];

    if overlay.fts_unavailable {
        // Show an error overlay without hiding the table entirely.
        frame.render_widget(
            Paragraph::new("Search unavailable: run lt sync first"),
            area,
        );
        return;
    }

    if overlay.query.value.trim().is_empty() {
        // No query yet -- keep the underlying list visible.
        return;
    }

    // Keep the underlying list visible while a search is queued (debounce
    // pending) or hasn't run yet, avoiding a flash of empty content.
    if overlay.results.is_empty() && (overlay.last_changed.is_some() || !overlay.has_searched) {
        return;
    }

    frame.render_widget(Clear, area);

    if overlay.results.is_empty() {
        frame.render_widget(Paragraph::new("No results."), area);
        return;
    }

    // Render results as a table identical in style to the main list.
    let sort_col = sort_col_index(sort.field);
    render_issue_table(
        frame,
        area,
        &TableSpec {
            issues: &overlay.results,
            sort_col,
            desc: sort.desc,
            cells: row_cells,
        },
        &mut overlay.table_state,
    );
}
