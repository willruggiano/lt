use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table, Wrap};

use super::{App, Mode, PopupKind, Status};
use crate::issues::list::Issue;
use crate::issues::{IssueArgs, SortField};
use crate::linear::types::IssueDetail;

pub fn render(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(frame.area());

    // Expose visible row count to key handlers (subtract table header row).
    app.viewport_height = chunks[1].height.saturating_sub(1);

    let context = filter_context(&app.args);
    let has_next = app.has_next_page;
    let has_prev = !app.cursor_stack.is_empty();
    let page = app.cursor_stack.len() + 1;
    let input_mode = app.input_mode;
    let input_buf = app.input_buf.clone();

    render_header(frame, chunks[0], &context);

    match app.mode {
        Mode::Detail => {
            // Vertical split: list (~40%) | detail (~60%).
            let split = Layout::horizontal([
                Constraint::Percentage(40),
                Constraint::Percentage(60),
            ])
            .split(chunks[1]);

            render_table(frame, split[0], app);
            render_detail(frame, split[1], app);
            render_detail_footer(frame, chunks[2]);
        }
        _ => {
            render_table(frame, chunks[1], app);
            if input_mode {
                render_input(frame, chunks[2], &input_buf);
            } else if let Some(msg) = &app.footer_msg {
                frame.render_widget(Paragraph::new(format!("[!] {}", msg)), chunks[2]);
            } else {
                render_footer(frame, chunks[2], has_next, has_prev, page);
            }
        }
    }

    // Render popup on top if active.
    if let Mode::Popup(ref kind) = app.mode {
        render_popup(frame, frame.area(), kind, &app.popup_items, app.popup_selected);
    }
}

// -- header ------------------------------------------------------------------

fn render_header(frame: &mut Frame, area: Rect, context: &str) {
    let text = if context.is_empty() {
        "lt issues".to_string()
    } else {
        format!("lt issues  {}", context)
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::new().add_modifier(Modifier::BOLD)),
        area,
    );
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
    if let Some(t) = &args.title {
        parts.push(format!("title:{}", t));
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
    let dir = if args.desc { "desc" } else { "asc" };
    parts.push(format!("sort:{} ({})", args.sort.label(), dir));
    parts.join("  ")
}

// -- footer / input overlay --------------------------------------------------

fn render_footer(frame: &mut Frame, area: Rect, has_next: bool, has_prev: bool, page: usize) {
    let mut parts: Vec<&str> = vec![
        "j/k navigate",
        "ctrl+d/u half page",
        "/ filter",
        "S sort field",
        "d toggle desc",
        "s state",
        "p priority",
        "a assignee",
        "o open",
        "r refresh",
        "q quit",
    ];
    if has_prev {
        parts.push("ctrl+p prev page");
    }
    if has_next {
        parts.push("ctrl+n next page");
    }

    let page_str = format!("[{}]", page);
    let chunks = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(page_str.len() as u16),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(parts.join("  ")), chunks[0]);
    frame.render_widget(Paragraph::new(page_str), chunks[1]);
}

fn render_input(frame: &mut Frame, area: Rect, buf: &str) {
    frame.render_widget(Paragraph::new(format!("/ {}_", buf)), area);
}

// -- table -------------------------------------------------------------------

fn render_table(frame: &mut Frame, area: Rect, app: &mut App) {
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

    let sort_col = sort_col_index(&app.args.sort);
    let sort_marker = if app.args.desc { "v" } else { "^" };
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
        if Some(i) == sort_col {
            format!("{} {}", base_headers[i], sort_marker)
        } else {
            base_headers[i].to_string()
        }
    });

    let mut widths: [usize; 7] = headers.each_ref().map(|h| h.len());
    for issue in &app.issues {
        let row = row_cells(issue);
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    let header = Row::new(headers.map(Cell::from)).style(Style::new().add_modifier(Modifier::BOLD));

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
        issue
            .assignee
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
    if s.len() >= 10 { &s[..10] } else { s }
}

// Returns the column index (0-6) that corresponds to the active sort field, if any.
fn sort_col_index(field: &SortField) -> Option<usize> {
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

// -- Detail pane (bd-2g8) ----------------------------------------------------

fn render_detail(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::LEFT)
        .title(" Detail ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Show loading / error overlay if applicable.
    match &app.status {
        Status::Loading => {
            frame.render_widget(Paragraph::new("Loading..."), inner);
            return;
        }
        Status::Error(msg) => {
            frame.render_widget(Paragraph::new(format!("Error: {}", msg)), inner);
            return;
        }
        Status::Idle => {}
    }

    if let Some(detail) = &app.detail {
        let lines = build_detail_lines(detail);
        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0));
        frame.render_widget(para, inner);
    }
}

fn build_detail_lines(d: &IssueDetail) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Header line: IDENTIFIER - Title
    lines.push(Line::from(vec![
        Span::styled(
            d.identifier.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" - {}", d.title)),
    ]));

    // Meta line: state, priority, assignee, team
    let assignee = d
        .assignee
        .as_ref()
        .map(|u| u.name.clone())
        .unwrap_or_else(|| "unassigned".to_string());
    lines.push(Line::from(format!(
        "[{}]  {}  {}  {}",
        d.state.name, d.priority_label, assignee, d.team.name
    )));

    lines.push(Line::from(""));

    // Description
    if let Some(desc) = &d.description {
        if !desc.is_empty() {
            lines.push(Line::from(Span::styled(
                "Description",
                Style::new().add_modifier(Modifier::UNDERLINED),
            )));
            lines.push(Line::from(""));
            for raw_line in desc.lines() {
                lines.push(Line::from(strip_markdown(raw_line)));
            }
            lines.push(Line::from(""));
        }
    }

    // Comments
    if !d.comments.nodes.is_empty() {
        lines.push(Line::from(Span::styled(
            "Comments",
            Style::new().add_modifier(Modifier::UNDERLINED),
        )));
        for comment in &d.comments.nodes {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(
                    "{} on {}",
                    comment.author(),
                    date(&comment.created_at)
                ),
                Style::new().add_modifier(Modifier::BOLD),
            )));
            for raw_line in comment.body.lines() {
                lines.push(Line::from(strip_markdown(raw_line)));
            }
        }
    }

    lines
}

/// Minimal markdown stripping: remove **bold** markers and __underline__ markers.
fn strip_markdown(s: &str) -> String {
    let s = s.replace("**", "");
    let s = s.replace("__", "");
    s
}

fn render_detail_footer(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new("j/k scroll  o open in browser  Esc/q close"),
        area,
    );
}

// -- Generic list-picker popup (bd-3dz) --------------------------------------

fn render_popup(
    frame: &mut Frame,
    area: Rect,
    kind: &PopupKind,
    items: &[super::PopupItem],
    selected: usize,
) {
    let title = match kind {
        PopupKind::State => " Set State ",
        PopupKind::Priority => " Set Priority ",
        PopupKind::Assignee => " Reassign ",
    };

    // Centre a box that is wide enough for the items.
    let max_label = items.iter().map(|i| i.label.len()).max().unwrap_or(10);
    let width = (max_label + 4).max(title.len() + 2).min(area.width as usize) as u16;
    let height = (items.len() + 2).min(area.height as usize) as u16;
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let popup_area = Rect::new(x, y, width, height);

    let list_items: Vec<ListItem> = items
        .iter()
        .map(|i| ListItem::new(format!(" {} ", i.label)))
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(selected));

    let list = List::new(list_items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded),
        )
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));

    frame.render_stateful_widget(list, popup_area, &mut list_state);
}
