use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

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

/// The header identity, plus the base list's active filter context (when
/// not searching).
pub(super) struct Header<'a> {
    pub(super) context: &'a str,
    pub(super) auth: &'a AuthStatus,
}

impl Widget for &Header<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let identity = identity_label(self.auth);
        let text = if self.context.is_empty() {
            identity
        } else {
            format!("{identity}  {}", self.context)
        };
        Paragraph::new(text)
            .style(Style::new().add_modifier(Modifier::BOLD))
            .render(area, buf);
    }
}

/// The header identity with the search overlay's query bar appended inline.
pub(super) struct HeaderWithSearch<'a> {
    pub(super) auth: &'a AuthStatus,
    pub(super) overlay: &'a SearchOverlay,
}

impl Widget for &HeaderWithSearch<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut line = Line::default();
        let identity = identity_label(self.auth);

        if self.overlay.fts_unavailable {
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
            append_text_input_spans(&mut line, &self.overlay.query, &self.overlay.ast.errors);
            // Append inline ghost-text suffix hint.
            if let Some(suffix) = self.overlay.completer.hint_suffix() {
                line.spans.push(Span::styled(
                    suffix.to_owned(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }

        Paragraph::new(line).render(area, buf);
    }
}

/// Pagination and sync state shown in the list-mode footer.
pub(super) struct Footer<'a> {
    pub(super) has_next: bool,
    pub(super) has_prev: bool,
    pub(super) page: usize,
    pub(super) sync_label: &'a str,
}

impl Widget for &Footer<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut parts: Vec<&str> = vec![
            "q quit",
            "/ search",
            "ctrl+/ help",
            "j/k nav",
            "<space> detail",
            "c new",
        ];
        if self.has_prev {
            parts.push("ctrl+p prev");
        }
        if self.has_next {
            parts.push("ctrl+n next");
        }

        let page_str = format!("[{}]", self.page);
        // Show sync status on the right side, separated from page indicator.
        let sync_str = format!("  {}  {page_str}", self.sync_label);
        let chunks = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(to_u16(sync_str.len())),
        ])
        .split(area);

        Paragraph::new(parts.join("  ")).render(chunks[0], buf);
        Paragraph::new(sync_str).render(chunks[1], buf);
    }
}
