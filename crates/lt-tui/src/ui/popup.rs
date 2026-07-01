use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState};

use super::util::to_u16;
use crate::{PopupItem, PopupKind};

/// Contents and placement of the generic list-picker popup.
pub(super) struct Popup<'a> {
    pub(super) anchor: Option<Rect>,
    pub(super) kind: &'a PopupKind,
    pub(super) items: &'a [PopupItem],
    pub(super) selected: usize,
}

pub(super) fn render_popup(frame: &mut Frame, area: Rect, popup: &Popup) {
    let Popup {
        anchor,
        kind,
        items,
        selected,
    } = *popup;
    let title = match kind {
        PopupKind::State => " Set State ",
        PopupKind::Priority => " Set Priority ",
        PopupKind::Assignee => " Reassign ",
    };

    // Size the popup to fit its contents.
    let max_label = items.iter().map(|i| i.label.len()).max().unwrap_or(10);
    let width = to_u16(
        (max_label + 4)
            .max(title.len() + 2)
            .min(area.width as usize),
    );
    let height = to_u16((items.len() + 2).min(area.height as usize));

    // Position: if we have an anchor, open directly below the cell; otherwise centre.
    let (x, y) = if let Some(anch) = anchor {
        // Prefer opening below the anchor row, clamp so the popup stays on screen.
        let px = anch.x.min(area.x + area.width.saturating_sub(width));
        let py = if anch.y + height <= area.y + area.height {
            anch.y
        } else {
            // Not enough space below -- open above the anchor row instead.
            anch.y.saturating_sub(height + 1)
        };
        (px, py)
    } else {
        let px = area.x + area.width.saturating_sub(width) / 2;
        let py = area.y + area.height.saturating_sub(height) / 2;
        (px, py)
    };
    let popup_area = Rect::new(x, y, width, height);

    // Clear the area under the popup to prevent the list from bleeding through.
    frame.render_widget(Clear, popup_area);

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
