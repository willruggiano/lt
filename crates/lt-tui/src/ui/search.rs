use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Clear, Paragraph};

use super::table::sort_col_index;
use super::util::{TableSpec, render_issue_table};
use crate::SearchOverlay;
use lt_storage::query::SortField;
use lt_storage::text;
use lt_types::types::Issue;

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
    // The search bar is rendered in the header row (chunks[0]) by render().
    // This function only handles the results in the main content area (chunks[2]).
    let area = chunks[2];

    // When the query is empty, leave the underlying issue table visible.
    // When FTS is unavailable, show the error but still don't wipe the table.
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

    // Keep the underlying list visible when:
    // - a search is queued but hasn't fired yet (debounce pending), or
    // - the overlay was just opened and no search has run yet.
    // This avoids a flash of empty content or a spurious "No results." on entry.
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
            cells: search_row_cells,
        },
        &mut overlay.table_state,
    );
}

fn search_row_cells(issue: &Issue) -> [String; 7] {
    fn date(s: &str) -> &str {
        if s.len() >= 10 { &s[..10] } else { s }
    }
    [
        issue.identifier.clone(),
        text::truncate(&issue.title, 40),
        issue.state.name.clone(),
        issue.priority_label.clone(),
        issue
            .assignee
            .as_ref()
            .map_or_else(|| "-".to_string(), |u| u.name.clone()),
        issue.team.name.clone(),
        date(&issue.updated_at).to_string(),
    ]
}
