use lt_runtime::query::SortDirection;
use lt_runtime::text;
use lt_types::comments::Comment;
use lt_types::types::Issue;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Row, StatefulWidget, Table, TableState};

use super::comment::CommentBlock;
use crate::markdown;

/// Saturating conversion of a length/index to a terminal coordinate.
fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

const HEADERS: [&str; 7] = [
    "IDENTIFIER",
    "TITLE",
    "STATE",
    "PRIORITY",
    "ASSIGNEE",
    "TEAM",
    "UPDATED",
];
const MIN_TITLE_WIDTH: usize = 40;

/// One issue's presentation as a row in the shared issues table.
pub(crate) struct IssueRow<'a>(pub(crate) &'a Issue);

impl IssueRow<'_> {
    fn cells(&self) -> [String; 7] {
        let issue = self.0;
        [
            issue.identifier.clone(),
            issue.title.clone(),
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
}

/// The shared issues table: the base list and the FTS search overlay's
/// results render identical columns from different issue slices, so both
/// share this widget instead of duplicating the layout.
pub(crate) struct IssueTable<'a> {
    pub(crate) issues: &'a [Issue],
    pub(crate) sort_col: Option<usize>,
    pub(crate) direction: SortDirection,
}

impl IssueTable<'_> {
    /// Column widths sized to content, with TITLE absorbing whatever
    /// horizontal space `area_width` has left over rather than sizing to its
    /// (unbounded) content. Shared by the actual render and by the base
    /// list's `TableGeometry`, so computing the popup anchor never
    /// re-derives widths through a different path.
    pub(crate) fn widths(&self, area_width: u16) -> [usize; 7] {
        let mut widths: [usize; 7] = HEADERS.each_ref().map(|h| h.len());
        for issue in self.issues {
            let row = IssueRow(issue).cells();
            for (i, cell) in row.iter().enumerate() {
                if cell.len() > widths[i] {
                    widths[i] = cell.len();
                }
            }
        }

        let other_widths: usize = widths
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .map(|(_, w)| *w)
            .sum();
        let spacing = 2 * (widths.len() - 1);
        let available = usize::from(area_width)
            .saturating_sub(other_widths)
            .saturating_sub(spacing);
        widths[1] = widths[1].min(available.max(MIN_TITLE_WIDTH.min(widths[1])));
        widths
    }
}

impl StatefulWidget for &IssueTable<'_> {
    type State = TableState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut TableState) {
        let sort_marker = if self.direction == SortDirection::Descending {
            "-"
        } else {
            "+"
        };
        let headers: [String; 7] = std::array::from_fn(|i| {
            if Some(i) == self.sort_col {
                format!("{} {}", HEADERS[i], sort_marker)
            } else {
                HEADERS[i].to_string()
            }
        });

        let widths = self.widths(area.width);
        let header =
            Row::new(headers.map(Cell::from)).style(Style::new().add_modifier(Modifier::BOLD));

        let rows: Vec<Row> = self
            .issues
            .iter()
            .map(|issue| {
                let mut cells = IssueRow(issue).cells();
                if cells[1].len() > widths[1] {
                    cells[1] = text::truncate(&cells[1], widths[1]);
                }
                Row::new(cells.map(Cell::from))
            })
            .collect();

        let constraints: Vec<Constraint> = widths
            .iter()
            .map(|w| Constraint::Length(to_u16(*w)))
            .collect();

        let table = Table::new(rows, constraints)
            .header(header)
            .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .column_spacing(2);

        StatefulWidget::render(table, area, buf, state);
    }
}

/// One issue's full detail-pane text: header, metadata, labels, sub-issues,
/// description, and comments -- independent of the pane's scroll/comment-input
/// layout, which stays with `DetailView`.
pub(crate) struct IssueDetail<'a> {
    pub(crate) issue: &'a Issue,
    pub(crate) comments: &'a [Comment],
    pub(crate) children: &'a [Issue],
}

impl IssueDetail<'_> {
    pub(crate) fn lines(&self) -> Vec<Line<'static>> {
        let issue = self.issue;
        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from(vec![
            Span::styled(
                issue.identifier.clone(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" - {}", issue.title)),
        ]));

        let assignee = issue
            .assignee
            .as_ref()
            .map_or_else(|| "unassigned".to_string(), |u| u.name.clone());
        lines.push(Line::from(format!(
            "[{}]  {}  {}  {}",
            issue.state.name, issue.priority_label, assignee, issue.team.name
        )));

        if let Some(parent) = &issue.parent {
            lines.push(Line::from(format!("Parent: {}", parent.identifier)));
        }

        if !issue.labels.nodes.is_empty() {
            let names = issue
                .labels
                .nodes
                .iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(Line::from(vec![
                Span::styled("Labels: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(names),
            ]));
        }

        if !self.children.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Sub-issues",
                Style::new().add_modifier(Modifier::UNDERLINED),
            )));
            for child in self.children {
                lines.push(Line::from(format!(
                    "  [{}] {} - {}",
                    child.state.name, child.identifier, child.title
                )));
            }
        }

        lines.push(Line::from(""));

        if let Some(desc) = &issue.description
            && !desc.is_empty()
        {
            lines.push(Line::from(Span::styled(
                "Description",
                Style::new().add_modifier(Modifier::UNDERLINED),
            )));
            lines.push(Line::from(""));
            lines.extend(markdown::render(desc));
            lines.push(Line::from(""));
        }

        if !self.comments.is_empty() {
            lines.push(Line::from(Span::styled(
                "Comments",
                Style::new().add_modifier(Modifier::UNDERLINED),
            )));
            for comment in self.comments {
                lines.push(Line::from(""));
                lines.extend(CommentBlock(comment).lines());
            }
        }

        lines
    }
}
