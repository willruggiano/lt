use lt_upstream::query::comments::Comment;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::markdown;

/// A single comment's detail-pane presentation: the author/date header line
/// plus its rendered Markdown body.
pub(crate) struct CommentBlock<'a>(pub(crate) &'a Comment);

impl CommentBlock<'_> {
    pub(crate) fn lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(Span::styled(
            format!("{} on {}", self.0.author(), self.0.created_at.date()),
            Style::new().add_modifier(Modifier::BOLD),
        ))];
        lines.extend(markdown::render(&self.0.body));
        lines
    }
}
