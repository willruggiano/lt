use lt_runtime::query::SortField;
use lt_runtime::text;
use lt_types::types::Issue;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

use super::util::{TableSpec, render_issue_table, to_u16};
use crate::{App, PopupKind, Status, View};

pub(super) fn render_table(frame: &mut Frame, area: Rect, app: &mut App) {
    let sort_col = sort_col_index(&app.args.sort);
    let desc = app.args.desc;

    let Some(View::List(list)) = app.views.first_mut() else {
        return;
    };

    let overlay: Option<String> = match &list.status {
        Status::Error(msg) => Some(format!("Error: {msg}")),
        Status::Loading => Some("Loading...".to_string()),
        Status::Idle => None,
    };
    if let Some(msg) = overlay {
        frame.render_widget(Paragraph::new(msg), area);
        return;
    }

    if list.issues.is_empty() {
        frame.render_widget(Paragraph::new("No issues found."), area);
        return;
    }

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
    let selected = list.table_state.selected().unwrap_or(0);

    // Popup anchor: computed from column geometry, but only written when the
    // popup sits directly on the base list -- a future popup over another
    // base view leaves it `None`, and `render_popup` centers.
    if let [View::List(_), View::Popup(popup)] = app.views.as_mut_slice() {
        let col_idx: usize = match &popup.kind {
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
        popup.anchor = Some(ratatui::layout::Rect::new(col_x, row_y, col_w, 1));
    }
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
