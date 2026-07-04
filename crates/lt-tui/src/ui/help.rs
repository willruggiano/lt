use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph};

use super::text_span::append_text_input_spans;
use super::util::{pct, to_u16};
use crate::HelpPopup;

/// The popup's frame sizing, computed once per frame from `popup`'s
/// already-cached column widths.
struct HelpLayout {
    popup_area: Rect,
    gap_str: String,
}

/// Size the popup wide enough for every row's key/context/label columns --
/// one leading and one trailing space plus a gap between each column,
/// capped to `area`. If `area` is too narrow even for that, shrink the
/// inter-column gap from 2 spaces to 1 rather than truncate a label. Height
/// is up to 80% of `area`, centred.
fn help_layout(area: Rect, popup: &HelpPopup) -> HelpLayout {
    let borders = 2;
    let inner_max = area.width.saturating_sub(borders);
    let row_width = |gap: u16| {
        1 + to_u16(popup.key_col_width)
            + gap
            + to_u16(popup.context_col_width)
            + gap
            + to_u16(popup.label_col_width)
            + 1
    };
    let (inner_width, gap): (u16, u16) = if row_width(2) <= inner_max {
        (row_width(2), 2)
    } else {
        (row_width(1).min(inner_max), 1)
    };
    let gap_str = " ".repeat(usize::from(gap));
    let width = (inner_width + borders).max(50).min(area.width);

    let max_rows = to_u16(popup.rows.len() + 4); // header + search + border
    let height = max_rows.min(pct(area.height, 80)).max(6);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;

    HelpLayout {
        popup_area: Rect::new(x, y, width, height),
        gap_str,
    }
}

pub(super) fn render_help_popup(frame: &mut Frame, area: Rect, popup: &HelpPopup) {
    let layout = help_layout(area, popup);
    let popup_area = layout.popup_area;

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(" Help  (type to search, Esc to close) ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);

    let mut search_line = Line::from(vec![Span::raw("/ ")]);
    append_text_input_spans(&mut search_line, &popup.search, &[]);
    frame.render_widget(Paragraph::new(search_line), chunks[0]);

    let list_height = chunks[1].height as usize;
    let total = popup.filtered.len();

    let scroll_offset = if popup.selected >= list_height {
        popup.selected - list_height + 1
    } else {
        0
    };

    let items: Vec<ListItem> = popup
        .filtered
        .iter()
        .skip(scroll_offset)
        .take(list_height)
        .enumerate()
        .map(|(vis_idx, &real_idx)| {
            let row = &popup.rows[real_idx];
            let abs_idx = vis_idx + scroll_offset;
            let line = format!(
                " {binding:<kw$}{gap_str}{context:<cw$}{gap_str}{label} ",
                binding = row.binding_form,
                context = row.context,
                label = row.label,
                kw = popup.key_col_width,
                cw = popup.context_col_width,
                gap_str = layout.gap_str,
            );
            let style = if abs_idx == popup.selected {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();

    let count_hint = if total > list_height {
        format!(" [{}/{}] ", popup.selected + 1, total)
    } else {
        String::new()
    };
    if !count_hint.is_empty() && chunks[1].height > 0 {
        let hint_area = Rect::new(
            chunks[1].x,
            chunks[1].y + chunks[1].height - 1,
            chunks[1].width,
            1,
        );
        frame.render_widget(Paragraph::new(count_hint), hint_area);
    }

    frame.render_widget(List::new(items), chunks[1]);
}
