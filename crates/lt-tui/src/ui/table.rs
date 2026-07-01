use lt_storage::query::SortField;
use lt_storage::text;
use lt_types::types::Issue;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

use super::util::{TableSpec, render_issue_table, to_u16};
use crate::{App, Mode, PopupKind, Status};

pub(super) fn render_table(frame: &mut Frame, area: Rect, app: &mut App) {
    let overlay: Option<String> = match &app.status {
        Status::Error(msg) => Some(format!("Error: {msg}")),
        Status::Loading => Some("Loading...".to_string()),
        Status::Idle => None,
    };
    if let Some(msg) = overlay {
        frame.render_widget(Paragraph::new(msg), area);
        return;
    }

    if app.issues.is_empty() {
        frame.render_widget(Paragraph::new("No issues found."), area);
        return;
    }

    let sort_col = sort_col_index(&app.args.sort);
    let widths = render_issue_table(
        frame,
        area,
        &TableSpec {
            issues: &app.issues,
            sort_col,
            desc: app.args.desc,
            cells: row_cells,
        },
        &mut app.table_state,
    );

    // Compute anchor rect for the popup.
    // Column mapping: 2=State, 3=Priority, 4=Assignee.
    // We position the anchor below the selected row at the relevant column x.
    if let Mode::Popup(ref kind) = app.mode {
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
        let sel = to_u16(app.table_state.selected().unwrap_or(0));
        let row_y = area.y + 1 + sel + 1;
        app.popup_anchor = Some(ratatui::layout::Rect::new(col_x, row_y, col_w, 1));
    }
}

fn row_cells(issue: &Issue) -> [String; 7] {
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

pub(super) fn date(s: &str) -> &str {
    if s.len() >= 10 { &s[..10] } else { s }
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
