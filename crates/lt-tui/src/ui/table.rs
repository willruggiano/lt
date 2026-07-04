use lt_runtime::query::SortField;
use lt_runtime::text;
use lt_types::types::Issue;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

use super::util::{TableSpec, render_issue_table, to_u16};
use crate::{FetchStatus, ListView, PopupKind};

/// Render the base issue table into `area`. Returns the rendered column
/// widths, or `None` when a loading/error overlay or the empty-list message
/// was shown instead -- `popup_anchor` only applies over a rendered table.
pub(super) fn render_table(
    frame: &mut Frame,
    area: Rect,
    list: &mut ListView,
) -> Option<[usize; 7]> {
    let overlay: Option<String> = match &list.status {
        FetchStatus::Error(msg) => Some(format!("Error: {msg}")),
        FetchStatus::Loading => Some("Loading...".to_string()),
        FetchStatus::Idle => None,
    };
    if let Some(msg) = overlay {
        frame.render_widget(Paragraph::new(msg), area);
        return None;
    }

    if list.issues.is_empty() {
        frame.render_widget(Paragraph::new("No issues found."), area);
        return None;
    }

    let sort_col = sort_col_index(&list.args.sort);
    let desc = list.args.desc;
    let widths = render_issue_table(
        frame,
        area,
        &TableSpec {
            issues: &list.issues,
            sort_col,
            desc,
            cells: row_cells,
        },
        &mut list.table_state,
    );
    Some(widths)
}

/// The state/priority/assignee popup's anchor when it sits directly on the
/// base list (an exact two-view stack): the target column's x offset plus
/// the row below the selected issue, from the base table's rendered column
/// widths. A popup over any other stack shape leaves `anchor` `None`, and
/// `render_popup` centers instead.
pub(super) fn popup_anchor(
    area: Rect,
    widths: &[usize],
    selected: usize,
    kind: &PopupKind,
) -> Rect {
    let col_idx: usize = match kind {
        PopupKind::State => 2,
        PopupKind::Priority => 3,
        PopupKind::Assignee => 4,
    };
    // Compute x offset of the target column (each column is widths[i] + 2 spacing).
    let col_x: u16 = widths[..col_idx]
        .iter()
        .map(|w| to_u16(*w) + 2)
        .sum::<u16>()
        + area.x;
    let col_w = to_u16(widths[col_idx]);
    // Row y: area.y + 1 (header) + selected index + 1 (below row).
    let row_y = area.y + 1 + to_u16(selected) + 1;
    Rect::new(col_x, row_y, col_w, 1)
}

pub(super) fn row_cells(issue: &Issue) -> [String; 7] {
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
        issue.updated_at.date(),
    ]
}

// Returns the column index (0-6) that corresponds to the active sort field, if any.
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
