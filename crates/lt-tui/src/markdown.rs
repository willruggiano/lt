//! Render Markdown source into styled ratatui lines for the issue detail view.
//!
//! The detail view feeds issue descriptions and comment bodies through
//! [`render`], which walks the `CommonMark` event stream and emits one
//! [`Line`] per visual row. Block structure (paragraphs, headings, lists,
//! code blocks, block quotes) controls line breaks and indentation; inline
//! structure (emphasis, code, links) controls per-span styling.
//!
//! ```text
//! Markdown ──▶ pulldown_cmark::Parser ──▶ Renderer ──▶ Vec<Line>
//!              (Event stream)              (this file)   (Paragraph)
//! ```

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Convert a Markdown document into styled lines suitable for a `Paragraph`.
pub fn render(src: &str) -> Vec<Line<'static>> {
    let mut renderer = Renderer::default();
    let parser = Parser::new_ext(
        src,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    );
    for event in parser {
        renderer.handle(event);
    }
    renderer.finish()
}

/// Marker for an enclosing list: `None` for bullets, `Some(n)` for the next
/// number in an ordered list.
type ListMarker = Option<u64>;

#[derive(Default)]
struct Renderer {
    /// Completed lines.
    lines: Vec<Line<'static>>,
    /// Spans accumulated for the line currently being built.
    spans: Vec<Span<'static>>,
    /// Active inline style; mutated by emphasis/strong/code/link spans.
    style: Style,
    /// Saved styles, restored as inline tags close.
    style_stack: Vec<Style>,
    /// Enclosing lists, outermost first.
    lists: Vec<ListMarker>,
    /// Block-quote nesting depth.
    quote_depth: usize,
    /// Whether the current line sits inside a fenced/indented code block.
    in_code_block: bool,
}

impl Renderer {
    fn handle(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => self.inline_code(&code),
            Event::SoftBreak | Event::HardBreak => self.flush_line(),
            Event::Rule => self.rule(),
            Event::TaskListMarker(checked) => {
                self.push_span(if checked { "[x] " } else { "[ ] " });
            }
            // Inline HTML, footnote references, and math render as raw text.
            Event::Html(text)
            | Event::InlineHtml(text)
            | Event::FootnoteReference(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => {
                self.text(&text);
            }
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            // A paragraph at the top level is its own block; inside a list it
            // continues the current item without an extra blank line.
            Tag::Paragraph => {
                if self.lists.is_empty() {
                    self.blank_separator();
                }
                self.begin_line();
            }
            Tag::Item => self.begin_line(),
            Tag::Heading { level, .. } => self.start_heading(level),
            Tag::CodeBlock(_) => {
                self.blank_separator();
                self.in_code_block = true;
                self.push_style(Style::new().fg(Color::Cyan));
                self.begin_line();
            }
            Tag::BlockQuote(_) => self.quote_depth += 1,
            Tag::List(first) => {
                // Separate a top-level list from preceding text; a nested list
                // simply continues on the next line.
                if self.lists.is_empty() {
                    self.blank_separator();
                }
                self.lists.push(first);
            }
            Tag::Emphasis => self.push_style(self.style.add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(self.style.add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.push_style(self.style.add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => self.start_link(&dest_url),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Item => self.flush_line(),
            TagEnd::Heading(_) => {
                self.pop_style();
                self.flush_line();
                self.blank_separator();
            }
            TagEnd::CodeBlock => {
                self.flush_line();
                self.in_code_block = false;
                self.pop_style();
                self.blank_separator();
            }
            TagEnd::BlockQuote(_) => self.quote_depth = self.quote_depth.saturating_sub(1),
            TagEnd::List(_) => {
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blank_separator();
                }
            }
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Link
            | TagEnd::Image => self.pop_style(),
            _ => {}
        }
    }

    fn start_heading(&mut self, level: HeadingLevel) {
        self.blank_separator();
        let style = match level {
            HeadingLevel::H1 | HeadingLevel::H2 => Style::new()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            _ => Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        };
        self.push_style(style);
        self.begin_line();
    }

    fn start_link(&mut self, dest: &str) {
        self.push_style(
            self.style
                .fg(Color::Blue)
                .add_modifier(Modifier::UNDERLINED),
        );
        // Surface the destination so a non-clickable terminal still shows it.
        if !dest.is_empty() && !dest.starts_with('#') {
            self.spans
                .push(Span::styled(format!("{dest} "), self.style));
        }
    }

    /// Push the leading indent and list/quote markers for a fresh line.
    fn begin_line(&mut self) {
        if !self.spans.is_empty() {
            self.flush_line();
        }
        for _ in 0..self.quote_depth {
            self.spans
                .push(Span::styled("\u{2502} ", Style::new().fg(Color::DarkGray)));
        }
        let depth = self.lists.len().saturating_sub(1);
        if let Some(marker) = self.lists.last_mut() {
            let indent = "  ".repeat(depth);
            let bullet = match marker {
                Some(n) => {
                    let label = format!("{n}. ");
                    *n += 1;
                    label
                }
                None => "- ".to_string(),
            };
            self.spans.push(Span::raw(format!("{indent}{bullet}")));
        }
    }

    fn text(&mut self, text: &str) {
        if self.in_code_block {
            self.code_block_text(text);
        } else {
            self.push_span(text);
        }
    }

    /// Code-block text arrives with embedded newlines; emit one line per row.
    fn code_block_text(&mut self, text: &str) {
        let mut rows = text.split('\n').peekable();
        while let Some(row) = rows.next() {
            self.spans.push(Span::styled(row.to_string(), self.style));
            if rows.peek().is_some() {
                self.flush_line();
            }
        }
    }

    fn inline_code(&mut self, code: &str) {
        self.spans
            .push(Span::styled(code.to_string(), self.style.fg(Color::Cyan)));
    }

    fn rule(&mut self) {
        self.blank_separator();
        self.lines.push(Line::from(Span::styled(
            "\u{2500}".repeat(40),
            Style::new().fg(Color::DarkGray),
        )));
        self.blank_separator();
    }

    fn push_span(&mut self, text: &str) {
        self.spans.push(Span::styled(text.to_string(), self.style));
    }

    fn push_style(&mut self, style: Style) {
        self.style_stack.push(self.style);
        self.style = style;
    }

    fn pop_style(&mut self) {
        if let Some(style) = self.style_stack.pop() {
            self.style = style;
        }
    }

    /// Move the in-progress spans into a completed line.
    fn flush_line(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.spans);
        self.lines.push(Line::from(spans));
    }

    /// Emit a single blank line, collapsing runs and skipping a leading blank.
    fn blank_separator(&mut self) {
        self.flush_line();
        let last_is_blank = self.lines.last().is_some_and(line_is_blank);
        if !self.lines.is_empty() && !last_is_blank {
            self.lines.push(Line::from(""));
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_line();
        while self.lines.last().is_some_and(line_is_blank) {
            self.lines.pop();
        }
        self.lines
    }
}

/// A line is blank when it holds no spans, or only empty-content spans (e.g. the
/// trailing newline of a fenced code block).
fn line_is_blank(line: &Line) -> bool {
    line.spans.iter().all(|span| span.content.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Concatenate the text content of a line, ignoring styling.
    fn text_of(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn rendered(src: &str) -> Vec<String> {
        render(src).iter().map(text_of).collect()
    }

    #[test]
    fn plain_paragraph() {
        assert_eq!(rendered("hello world"), vec!["hello world"]);
    }

    #[test]
    fn soft_break_preserves_line_layout() {
        assert_eq!(rendered("line one\nline two"), vec!["line one", "line two"]);
    }

    #[test]
    fn blank_line_between_paragraphs() {
        assert_eq!(rendered("a\n\nb"), vec!["a", "", "b"]);
    }

    #[test]
    fn strong_emphasis_sets_bold_modifier() {
        let lines = render("**bold**");
        let span = &lines[0].spans[0];
        assert_eq!(span.content.as_ref(), "bold");
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn nested_emphasis_combines_modifiers() {
        // Bold wrapping italic: the inner text carries both modifiers.
        let lines = render("**bold _and italic_**");
        let inner = lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains("and italic"))
            .expect("italic span present");
        assert!(inner.style.add_modifier.contains(Modifier::BOLD));
        assert!(inner.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn heading_renders_text_and_blank_below() {
        let lines = render("# Title\n\nbody");
        assert_eq!(text_of(&lines[0]), "Title");
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(text_of(&lines[1]), "");
        assert_eq!(text_of(&lines[2]), "body");
    }

    #[test]
    fn bullet_list_marks_each_item() {
        assert_eq!(rendered("- one\n- two"), vec!["- one", "- two"]);
    }

    #[test]
    fn ordered_list_numbers_each_item() {
        assert_eq!(rendered("1. one\n2. two"), vec!["1. one", "2. two"]);
    }

    #[test]
    fn nested_list_indents() {
        let out = rendered("- outer\n  - inner");
        assert_eq!(out, vec!["- outer", "  - inner"]);
    }

    #[test]
    fn code_block_keeps_rows() {
        let out = rendered("```\nfn main() {}\nlet x = 1;\n```");
        assert_eq!(out, vec!["fn main() {}", "let x = 1;"]);
    }

    #[test]
    fn inline_code_is_styled() {
        let lines = render("call `foo()` now");
        let code = lines[0]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "foo()")
            .expect("code span present");
        assert_eq!(code.style.fg, Some(Color::Cyan));
    }

    #[test]
    fn link_surfaces_destination() {
        let out = rendered("[text](https://example.com)");
        assert_eq!(out, vec!["https://example.com text"]);
    }

    #[test]
    fn block_quote_gains_prefix() {
        assert_eq!(rendered("> quoted"), vec!["\u{2502} quoted"]);
    }

    #[test]
    fn task_list_renders_checkbox() {
        let out = rendered("- [x] done\n- [ ] todo");
        assert_eq!(out, vec!["- [x] done", "- [ ] todo"]);
    }

    #[test]
    fn trailing_blank_lines_trimmed() {
        assert_eq!(rendered("text\n\n\n"), vec!["text"]);
    }

    #[test]
    fn empty_input_yields_no_lines() {
        assert!(render("").is_empty());
    }
}
