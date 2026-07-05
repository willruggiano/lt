use lt_runtime::query::SortField;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Clear, Paragraph, StatefulWidget, Widget};

use super::table::sort_col_index;
use crate::SearchOverlay;
use crate::present::issue::IssueTable;

/// Active sort field and direction, threaded in from the base list so the
/// search overlay's results table marks the same sorted column -- cross-view
/// data as a render parameter, never stored on either view.
pub(super) struct SortOrder<'a> {
    pub(super) field: &'a SortField,
    pub(super) desc: bool,
}

/// Render input for the FTS search overlay's results: its own rows/table
/// state, plus the base list's active sort order.
pub(super) struct SearchResults<'a> {
    pub(super) overlay: &'a mut SearchOverlay,
    pub(super) sort: SortOrder<'a>,
}

impl Widget for &mut SearchResults<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let overlay = &mut *self.overlay;

        if overlay.fts_unavailable {
            // Show an error overlay without hiding the table entirely.
            Paragraph::new("Search unavailable: run lt sync first").render(area, buf);
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

        Clear.render(area, buf);

        if overlay.results.is_empty() {
            Paragraph::new("No results.").render(area, buf);
            return;
        }

        // Render results as a table identical in style to the main list.
        let table = IssueTable {
            issues: &overlay.results,
            sort_col: sort_col_index(self.sort.field),
            desc: self.sort.desc,
        };
        StatefulWidget::render(&table, area, buf, &mut overlay.table_state);
    }
}
