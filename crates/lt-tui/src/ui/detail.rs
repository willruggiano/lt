use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

use crate::detail::IssueDetailView;
use crate::{App, Status, markdown};

/// Render the issue detail as a floating overlay over the right ~60% of the
/// content area. The underlying issue list is drawn at full width first, so
/// column widths are never affected by opening the detail view.
pub(super) fn render_detail_overlay(frame: &mut Frame, area: Rect, app: &App) {
    // Overlay covers the right 60% of the content area.
    let overlay_width = area.width * 3 / 5;
    let overlay_x = area.x + area.width - overlay_width;
    let overlay_area = Rect::new(overlay_x, area.y, overlay_width, area.height);

    // Clear the background so the list does not bleed through.
    frame.render_widget(Clear, overlay_area);
    render_detail(frame, overlay_area, app);
}

fn render_detail(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::LEFT);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Show loading / error overlay if applicable.
    match &app.status {
        Status::Loading => {
            frame.render_widget(Paragraph::new("Loading..."), inner);
            return;
        }
        Status::Error(msg) => {
            frame.render_widget(Paragraph::new(format!("Error: {msg}")), inner);
            return;
        }
        Status::Idle => {}
    }

    // Reserve the bottom rows for the comment input box when it is open.
    let (content_area, comment_area) = if app.comment_input.is_some() {
        let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(6)]).split(inner);
        (chunks[0], Some(chunks[1]))
    } else {
        (inner, None)
    };

    if let Some(detail) = &app.detail {
        let lines = build_detail_lines(detail);
        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0));
        frame.render_widget(para, content_area);
    }

    if let (Some(buf), Some(area)) = (&app.comment_input, comment_area) {
        let block = Block::default()
            .title(" New Comment ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);
        let box_inner = block.inner(area);
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);
        // Cursor is always at the end (same model as the description field).
        frame.render_widget(
            Paragraph::new(format!("{buf}_")).wrap(Wrap { trim: false }),
            box_inner,
        );
    }
}

fn build_detail_lines(d: &IssueDetailView) -> Vec<Line<'static>> {
    let issue = &d.issue;
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Header line: IDENTIFIER - Title
    lines.push(Line::from(vec![
        Span::styled(
            issue.identifier.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" - {}", issue.title)),
    ]));

    // Meta line: state, priority, assignee, team
    let assignee = issue
        .assignee
        .as_ref()
        .map_or_else(|| "unassigned".to_string(), |u| u.name.clone());
    lines.push(Line::from(format!(
        "[{}]  {}  {}  {}",
        issue.state.name, issue.priority_label, assignee, issue.team.name
    )));

    // Parent issue reference
    if let Some(ref parent) = d.parent {
        lines.push(Line::from(format!(
            "Parent: {} - {}",
            parent.identifier, parent.title
        )));
    }

    // Labels, shown directly below the meta line and above Sub-issues.
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

    // Sub-issues
    if !d.children.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Sub-issues",
            Style::new().add_modifier(Modifier::UNDERLINED),
        )));
        for child in &d.children {
            lines.push(Line::from(format!(
                "  [{}] {} - {}",
                child.state.name, child.identifier, child.title
            )));
        }
    }

    lines.push(Line::from(""));

    // Description
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

    // Comments
    if !d.comments.is_empty() {
        lines.push(Line::from(Span::styled(
            "Comments",
            Style::new().add_modifier(Modifier::UNDERLINED),
        )));
        for comment in &d.comments {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("{} on {}", comment.author(), comment.created_at.date()),
                Style::new().add_modifier(Modifier::BOLD),
            )));
            lines.extend(markdown::render(&comment.body));
        }
    }

    lines
}

pub(super) fn render_detail_footer(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new("j/k scroll  c comment  o open in browser  Esc/q close"),
        area,
    );
}
