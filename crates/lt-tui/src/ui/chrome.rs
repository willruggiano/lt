use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::text_span::append_text_input_spans;
use super::util::to_u16;
use crate::{AuthStatus, SearchOverlay};

/// Render the identity as a single `user:..  org:..` string, falling back to
/// an explicit unauthenticated placeholder when neither part is present.
fn identity_label(auth: &AuthStatus) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(u) = auth.viewer_name() {
        parts.push(format!("user:{u}"));
    }
    if let Some(o) = auth.org_name() {
        parts.push(format!("org:{o}"));
    }
    if parts.is_empty() {
        "(not authenticated)".to_string()
    } else {
        parts.join("  ")
    }
}

pub(super) fn render_header(frame: &mut Frame, area: Rect, context: &str, auth: &AuthStatus) {
    let identity = identity_label(auth);
    let text = if context.is_empty() {
        identity
    } else {
        format!("{identity}  {context}")
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::new().add_modifier(Modifier::BOLD)),
        area,
    );
}

pub(super) fn render_header_with_search(
    frame: &mut Frame,
    area: Rect,
    auth: &AuthStatus,
    overlay: &SearchOverlay,
) {
    let mut line = Line::default();

    let identity = identity_label(auth);

    if overlay.fts_unavailable {
        let prefix = format!("{identity}  ");
        line.spans.push(Span::styled(
            format!("{prefix}Search unavailable: run lt sync first"),
            Style::new().add_modifier(Modifier::BOLD),
        ));
    } else {
        line.spans.push(Span::styled(
            format!("{identity}  "),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        append_text_input_spans(&mut line, &overlay.query, &overlay.ast.errors);
        // Append inline ghost-text suffix hint.
        if let Some(suffix) = overlay.completer.hint_suffix() {
            line.spans.push(Span::styled(
                suffix.to_owned(),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    frame.render_widget(Paragraph::new(line), area);
}

/// Pagination and sync state shown in the list-mode footer.
pub(super) struct FooterState<'a> {
    pub(super) has_next: bool,
    pub(super) has_prev: bool,
    pub(super) page: usize,
    pub(super) sync_label: &'a str,
}

pub(super) fn render_footer(frame: &mut Frame, area: Rect, state: &FooterState) {
    let mut parts: Vec<&str> = vec![
        "q quit",
        "/ filter",
        "? help",
        "j/k nav",
        "<space> detail",
        "n new",
    ];
    if state.has_prev {
        parts.push("ctrl+p prev");
    }
    if state.has_next {
        parts.push("ctrl+n next");
    }

    let page_str = format!("[{}]", state.page);
    // Show sync status on the right side, separated from page indicator.
    let sync_str = format!("  {}  {page_str}", state.sync_label);
    let chunks = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(to_u16(sync_str.len())),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(parts.join("  ")), chunks[0]);
    frame.render_widget(Paragraph::new(sync_str), chunks[1]);
}
