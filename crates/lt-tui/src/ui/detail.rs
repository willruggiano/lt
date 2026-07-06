use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget, Wrap};

use crate::DetailView;
use crate::present::issue::IssueDetail;

/// Render the issue detail as a floating overlay over the right ~60% of the
/// content area. The underlying issue list is drawn at full width first, so
/// column widths are never affected by opening the detail view.
impl Widget for &DetailView {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let overlay_width = area.width * 3 / 5;
        let overlay_x = area.x + area.width - overlay_width;
        let overlay_area = Rect::new(overlay_x, area.y, overlay_width, area.height);

        // Clear the background so the list does not bleed through.
        Clear.render(overlay_area, buf);
        render_pane(self, overlay_area, buf);
    }
}

fn render_pane(detail: &DetailView, area: Rect, buf: &mut Buffer) {
    let block = Block::default().borders(Borders::LEFT);
    let inner = block.inner(area);
    block.render(area, buf);

    // Reserve the bottom rows for the comment input box when it is open.
    let (content_area, comment_area) = if detail.comment_input.is_some() {
        let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(6)]).split(inner);
        (chunks[0], Some(chunks[1]))
    } else {
        (inner, None)
    };

    let lines = IssueDetail {
        issue: &detail.issue,
        comments: &detail.comments,
        children: &detail.children,
    }
    .lines();
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((detail.scroll, 0))
        .render(content_area, buf);

    if let (Some(draft), Some(area)) = (&detail.comment_input, comment_area) {
        let block = Block::default()
            .title(" New Comment ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);
        let box_inner = block.inner(area);
        Clear.render(area, buf);
        block.render(area, buf);
        // Cursor is always at the end.
        Paragraph::new(format!("{draft}_"))
            .wrap(Wrap { trim: false })
            .render(box_inner, buf);
    }
}

/// The detail pane's status-row hint text.
pub(super) fn footer_hint() -> &'static str {
    "j/k scroll  c comment  o b open in browser  Esc/q close"
}
