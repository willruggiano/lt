use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Cell, Row, Table, TableState};

use lt_types::types::Issue;

/// Saturating conversion of a length/index to a terminal coordinate.
pub(super) fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// `percent`% of a terminal dimension, computed in integer arithmetic. The
/// result never exceeds `dim`, so it always fits back in `u16`.
pub(super) fn pct(dim: u16, percent: u32) -> u16 {
    u16::try_from(u32::from(dim) * percent / 100).unwrap_or(dim)
}

/// The data backing a rendered issue table: the rows, which column is sorted
/// (and in which direction), and how to turn an issue into its 7 cell strings.
pub(super) struct TableSpec<'a> {
    pub(super) issues: &'a [Issue],
    pub(super) sort_col: Option<usize>,
    pub(super) desc: bool,
    pub(super) cells: fn(&Issue) -> [String; 7],
}

/// Render the shared issue table (header with sort marker, column widths sized
/// to content, highlighted selection).
/// Returns the computed per-column widths so callers can position overlays.
pub(super) fn render_issue_table(
    frame: &mut Frame,
    area: Rect,
    spec: &TableSpec,
    table_state: &mut TableState,
) -> [usize; 7] {
    let sort_marker = if spec.desc { "-" } else { "+" };
    let base_headers: [&str; 7] = [
        "IDENTIFIER",
        "TITLE",
        "STATE",
        "PRIORITY",
        "ASSIGNEE",
        "TEAM",
        "UPDATED",
    ];
    let headers: [String; 7] = std::array::from_fn(|i| {
        if Some(i) == spec.sort_col {
            format!("{} {}", base_headers[i], sort_marker)
        } else {
            base_headers[i].to_string()
        }
    });

    let mut widths: [usize; 7] = headers.each_ref().map(std::string::String::len);
    for issue in spec.issues {
        let row = (spec.cells)(issue);
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    let header = Row::new(headers.map(Cell::from)).style(Style::new().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = spec
        .issues
        .iter()
        .map(|issue| Row::new((spec.cells)(issue).map(Cell::from)))
        .collect();

    let constraints: Vec<Constraint> = widths
        .iter()
        .map(|w| Constraint::Length(to_u16(*w)))
        .collect();

    let table = Table::new(rows, constraints)
        .header(header)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .column_spacing(2);

    frame.render_stateful_widget(table, area, table_state);

    widths
}
