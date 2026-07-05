use lt_runtime::query::SortField;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, StatefulWidget, Widget};

use crate::ListView;
use crate::present::issue::IssueTable;

/// The base issue table's rendered layout: the popup widget's anchor point
/// derives from these without the renderer writing anchor state onto either
/// view (docs/design/operation-seam-adr.md, Decision 9).
pub(super) struct TableGeometry {
    pub(super) area: Rect,
    pub(super) widths: [usize; 7],
    pub(super) selected_row: usize,
}

impl ListView {
    /// Render the base issue table into `area`, returning its layout for the
    /// popup widget's anchor -- `None` when the empty-list message was shown
    /// instead, since `TableGeometry` only applies over a rendered table.
    pub(super) fn render_table(&mut self, area: Rect, buf: &mut Buffer) -> Option<TableGeometry> {
        if self.issues.is_empty() {
            Paragraph::new("No issues found.").render(area, buf);
            return None;
        }

        let table = IssueTable {
            issues: &self.issues,
            sort_col: sort_col_index(&self.query.sort),
            desc: self.query.desc,
        };
        let widths = table.widths(area.width);
        StatefulWidget::render(&table, area, buf, &mut self.table_state);

        Some(TableGeometry {
            area,
            widths,
            selected_row: self.table_state.selected().unwrap_or(0),
        })
    }
}

impl Widget for &mut ListView {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.render_table(area, buf);
    }
}

pub(super) fn sort_col_index(field: &SortField) -> Option<usize> {
    match field {
        SortField::Title => Some(1),
        SortField::State => Some(2),
        SortField::Priority => Some(3),
        SortField::Assignee => Some(4),
        SortField::Team => Some(5),
        SortField::Updated => Some(6),
        SortField::Created => None,
    }
}
