use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, StatefulWidget, Widget,
};

use super::table::TableGeometry;
use super::util::to_u16;
use crate::{PopupItem, PopupKind};

/// Render input for the generic list-picker popup: its own items/selection,
/// plus the base table's layout when the popup sits directly on it (an exact
/// two-view stack) -- `None` centers instead of anchoring
/// (docs/design/operation-seam-adr.md, Decision 9).
pub(super) struct Popup<'a> {
    pub(super) base: Option<&'a TableGeometry>,
    pub(super) kind: &'a PopupKind,
    pub(super) items: &'a [PopupItem],
    pub(super) selected: usize,
}

impl Widget for &Popup<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title = match self.kind {
            PopupKind::State => " Set State ",
            PopupKind::Priority => " Set Priority ",
            PopupKind::Assignee => " Reassign ",
        };

        let max_label = self.items.iter().map(|i| i.label.len()).max().unwrap_or(10);
        let width = to_u16(
            (max_label + 4)
                .max(title.len() + 2)
                .min(area.width as usize),
        );
        let height = to_u16((self.items.len() + 2).min(area.height as usize));

        let anchor = self.base.map(|geometry| popup_anchor(geometry, self.kind));
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
        Clear.render(popup_area, buf);

        let list_items: Vec<ListItem> = self
            .items
            .iter()
            .map(|i| ListItem::new(format!(" {} ", i.label)))
            .collect();

        let mut list_state = ListState::default();
        list_state.select(Some(self.selected));

        let list = List::new(list_items)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded),
            )
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));

        StatefulWidget::render(list, popup_area, buf, &mut list_state);
    }
}

/// The state/priority/assignee popup's anchor when it sits directly on the
/// base list: the target column's x offset plus the row below the selected
/// issue, from the base table's rendered geometry.
fn popup_anchor(geometry: &TableGeometry, kind: &PopupKind) -> Rect {
    let col_idx: usize = match kind {
        PopupKind::State => 2,
        PopupKind::Priority => 3,
        PopupKind::Assignee => 4,
    };
    // Compute x offset of the target column (each column is widths[i] + 2 spacing).
    let col_x: u16 = geometry.widths[..col_idx]
        .iter()
        .map(|w| to_u16(*w) + 2)
        .sum::<u16>()
        + geometry.area.x;
    let col_w = to_u16(geometry.widths[col_idx]);
    // Row y: area.y + 1 (header) + selected index + 1 (below row).
    let row_y = geometry.area.y + 1 + to_u16(geometry.selected_row) + 1;
    Rect::new(col_x, row_y, col_w, 1)
}
