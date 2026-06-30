use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::TextInput;
use crate::tui::search_query::ParseError;

/// Return true when `byte_offset` falls inside any of the given error spans.
fn byte_in_error(byte_offset: usize, errors: &[ParseError]) -> bool {
    errors
        .iter()
        .any(|e| byte_offset >= e.span.start && byte_offset < e.span.end)
}

/// Split `text` (whose first byte is at `offset` in the original input string)
/// into contiguous sub-slices, each tagged with whether it overlaps a parse
/// error.  Adjacent bytes with the same error status are grouped together.
///
/// Returns `Vec<(&str, bool)>` where the bool is `true` when the slice is in
/// error territory.
fn error_segments<'a>(text: &'a str, offset: usize, errors: &[ParseError]) -> Vec<(&'a str, bool)> {
    if errors.is_empty() || text.is_empty() {
        return vec![(text, false)];
    }
    let mut result: Vec<(&'a str, bool)> = Vec::new();
    let mut seg_start = 0usize; // byte index within `text`
    let mut seg_is_err = byte_in_error(offset, errors);

    for (i, _ch) in text.char_indices().skip(1) {
        let is_err = byte_in_error(offset + i, errors);
        if is_err != seg_is_err {
            result.push((&text[seg_start..i], seg_is_err));
            seg_start = i;
            seg_is_err = is_err;
        }
    }
    // Push the final segment.
    result.push((&text[seg_start..], seg_is_err));
    result
}

/// Append styled spans for a non-cursor text segment.
///
/// Bytes overlapping a parse-error span are rendered with `Color::Red`; all
/// other bytes use the default (terminal-inherited) style.
fn push_text_spans(line: &mut Line, text: &str, offset: usize, errors: &[ParseError]) {
    for (seg, is_err) in error_segments(text, offset, errors) {
        if seg.is_empty() {
            continue;
        }
        if is_err {
            line.spans.push(Span::styled(
                seg.to_owned(),
                Style::default().fg(Color::Red),
            ));
        } else {
            line.spans.push(Span::raw(seg.to_owned()));
        }
    }
}

/// Render the text after the block cursor, underlining any active selection.
fn push_after_cursor_spans(
    line: &mut Line,
    input: &TextInput,
    after_offset: usize,
    errors: &[ParseError],
) {
    let after = &input.value[after_offset..];
    let Some(sel_end) = input.selection_end else {
        push_text_spans(line, after, after_offset, errors);
        return;
    };
    let sel_end = sel_end.min(input.value.len());
    if sel_end <= after_offset {
        push_text_spans(line, after, after_offset, errors);
        return;
    }
    // Selected portion: [after_offset, sel_end)
    let sel_text = &input.value[after_offset..sel_end];
    if !sel_text.is_empty() {
        line.spans.push(Span::styled(
            sel_text.to_owned(),
            Style::new().add_modifier(Modifier::UNDERLINED),
        ));
    }
    // Rest: [sel_end, end)
    let rest = &input.value[sel_end..];
    if !rest.is_empty() {
        push_text_spans(line, rest, sel_end, errors);
    }
}

/// Append spans representing a `TextInput` to an existing `Line`.
///
/// The character at the cursor position is rendered with a reversed
/// (block-cursor) style.  If the cursor is at the end of the string, a
/// space with reversed style is appended to show the cursor position.
///
/// When `input.selection_end` is set, the range `cursor..selection_end` is
/// rendered with UNDERLINED style (in addition to the block cursor char).
///
/// `errors` is the list of parse errors from the current `QueryAst`.  Any
/// text whose byte range overlaps an error span is rendered with
/// `Color::Red` to give the user a visual signal that a stem was not
/// recognised.  Pass an empty slice when no error highlighting is needed
/// (e.g. modal title input).
pub(super) fn append_text_input_spans(line: &mut Line, input: &TextInput, errors: &[ParseError]) {
    let (before, ch_at_cursor, after) = input.display_parts();
    // `before` occupies bytes [0, cursor).
    if !before.is_empty() {
        push_text_spans(line, before, 0, errors);
    }
    match ch_at_cursor {
        Some(ch) => {
            // Cursor is on an existing character -- highlight it with REVERSED.
            // Cursor style takes priority over error colour.
            let mut s = String::new();
            s.push(ch);
            line.spans.push(Span::styled(
                s,
                Style::new().add_modifier(Modifier::REVERSED),
            ));
            // `after` occupies bytes [cursor + ch.len_utf8(), end).
            if !after.is_empty() {
                let after_offset = input.cursor + ch.len_utf8();
                push_after_cursor_spans(line, input, after_offset, errors);
            }
        }
        None => {
            // Cursor is past the end -- show a block cursor placeholder.
            line.spans.push(Span::styled(
                " ",
                Style::new().add_modifier(Modifier::REVERSED),
            ));
        }
    }
}
