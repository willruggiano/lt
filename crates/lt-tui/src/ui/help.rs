use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph};

use super::text_span::append_text_input_spans;
use super::util::{pct, to_u16};
use crate::{ALL_KEYBINDINGS, HelpPopup};

pub(super) fn render_help_popup(frame: &mut Frame, area: Rect, popup: &HelpPopup) {
    // Size: 60% wide, up to 80% tall, centred.
    let width = pct(area.width, 60).max(50).min(area.width);
    let max_rows = to_u16(ALL_KEYBINDINGS.len() + 4); // header + search + border
    let height = max_rows.min(pct(area.height, 80)).max(6);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let popup_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(" Help  (type to search, Esc/q to close) ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    // Split inner: search bar (1 row) + list (rest).
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);

    // Search bar.
    let mut search_line = Line::from(vec![Span::raw("/ ")]);
    append_text_input_spans(&mut search_line, &popup.search, &[]);
    frame.render_widget(Paragraph::new(search_line), chunks[0]);

    // Keybinding list.
    let list_height = chunks[1].height as usize;
    let total = popup.filtered.len();

    // Compute scroll so selected row stays visible.
    let scroll_offset = if popup.selected >= list_height {
        popup.selected - list_height + 1
    } else {
        0
    };

    let key_col_width = ALL_KEYBINDINGS
        .iter()
        .map(|e| e.key.len())
        .max()
        .unwrap_or(10);

    let items: Vec<ListItem> = popup
        .filtered
        .iter()
        .skip(scroll_offset)
        .take(list_height)
        .enumerate()
        .map(|(vis_idx, &real_idx)| {
            let entry = &ALL_KEYBINDINGS[real_idx];
            let abs_idx = vis_idx + scroll_offset;
            let line = format!(
                " {:<kw$}  {} ",
                entry.key,
                entry.description,
                kw = key_col_width
            );
            let style = if abs_idx == popup.selected {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();

    // Show count hint at bottom if list is truncated.
    let count_hint = if total > list_height {
        format!(" [{}/{}] ", popup.selected + 1, total)
    } else {
        String::new()
    };
    // Render hint in the last row of the list area if needed.
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
