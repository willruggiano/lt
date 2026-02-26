use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Cell, Paragraph, Row, Table};
use ratatui::Frame;

use super::{App, Status};
use crate::issues::list::Issue;
use crate::issues::IssueArgs;

pub fn render(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(frame.area());

    // Compute header content before mutable borrow for table.
    let context = filter_context(&app.args);
    let has_next = app.has_next_page;

    render_header(frame, chunks[0], &context);
    render_table(frame, chunks[1], app);
    render_footer(frame, chunks[2], has_next);
}

// -- header ------------------------------------------------------------------

fn render_header(frame: &mut Frame, area: Rect, context: &str) {
    let text = if context.is_empty() {
        "lt issues".to_string()
    } else {
        format!("lt issues  {}", context)
    };
    let para = Paragraph::new(text).style(Style::new().add_modifier(Modifier::BOLD));
    frame.render_widget(para, area);
}

fn filter_context(args: &IssueArgs) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = &args.team {
        parts.push(format!("team:{}", t));
    }
    if let Some(a) = &args.assignee {
        parts.push(format!("assignee:{}", a));
    }
    if args.no_assignee {
        parts.push("no-assignee".to_string());
    }
    if let Some(s) = &args.state {
        parts.push(format!("state:{}", s));
    }
    if let Some(p) = &args.priority {
        parts.push(format!("priority:{}", p));
    }
    if let Some(d) = &args.created_after {
        parts.push(format!("created>={}", d));
    }
    if let Some(d) = &args.created_before {
        parts.push(format!("created<{}", d));
    }
    if let Some(d) = &args.updated_after {
        parts.push(format!("updated>={}", d));
    }
    if let Some(d) = &args.updated_before {
        parts.push(format!("updated<{}", d));
    }
    parts.join("  ")
}

// -- footer ------------------------------------------------------------------

fn render_footer(frame: &mut Frame, area: Rect, has_next: bool) {
    let mut text = "j/k navigate  o open  r refresh  g/G top/bottom  q quit".to_string();
    if has_next {
        text.push_str("  +more issues");
    }
    frame.render_widget(Paragraph::new(text), area);
}

// -- table -------------------------------------------------------------------

fn render_table(frame: &mut Frame, area: Rect, app: &mut App) {
    // Show status overlays before borrowing table_state.
    let overlay: Option<String> = match &app.status {
        Status::Error(msg) => Some(format!("Error: {}", msg)),
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

    const HEADERS: [&str; 7] = [
        "IDENTIFIER", "TITLE", "STATE", "PRIORITY", "ASSIGNEE", "TEAM", "UPDATED",
    ];

    // Dynamic column widths.
    let mut widths: [usize; 7] = HEADERS.map(|h| h.len());
    for issue in &app.issues {
        let row = row_cells(issue);
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    let header = Row::new(HEADERS.map(Cell::from))
        .style(Style::new().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app
        .issues
        .iter()
        .map(|issue| Row::new(row_cells(issue).map(Cell::from)))
        .collect();

    let constraints: Vec<Constraint> = widths
        .iter()
        .map(|w| Constraint::Length(*w as u16))
        .collect();

    let table = Table::new(rows, constraints)
        .header(header)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .column_spacing(2);

    frame.render_stateful_widget(table, area, &mut app.table_state);
}

fn row_cells(issue: &Issue) -> [String; 7] {
    [
        issue.identifier.clone(),
        truncate(&issue.title, 40),
        issue.state.name.clone(),
        issue.priority_label.clone(),
        issue.assignee
            .as_ref()
            .map(|u| u.name.clone())
            .unwrap_or_else(|| "-".to_string()),
        issue.team.name.clone(),
        date(&issue.updated_at).to_string(),
    ]
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

fn date(s: &str) -> &str {
    if s.len() >= 10 {
        &s[..10]
    } else {
        s
    }
}
