use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Widget, Wrap,
};

use super::text_span::append_text_input_spans;
use super::util::pct;
use crate::{NewIssueField, NewIssueModal, PopupItem};

/// Submit-key hint: Ctrl-Enter needs the kitty keyboard protocol; legacy
/// terminals can only encode Alt-Enter.
pub(super) fn submit_key_label(keyboard_enhanced: bool) -> &'static str {
    if keyboard_enhanced {
        "Ctrl-Enter"
    } else {
        "Alt-Enter"
    }
}

/// Render input for the new-issue modal: its own state plus the session's
/// keyboard-enhancement capability, which the submit-key hint depends on.
pub(super) struct NewIssueForm<'a> {
    pub(super) modal: &'a NewIssueModal,
    pub(super) keyboard_enhanced: bool,
}

fn render_modal_title(modal: &NewIssueModal, area: Rect, buf: &mut Buffer) {
    let active = modal.focused_field == NewIssueField::Title;
    let label = Span::styled(
        if active { "[Title]" } else { " Title " },
        if active {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().add_modifier(Modifier::BOLD)
        },
    );
    let mut line = Line::from(vec![label, Span::raw("  ")]);
    if active {
        append_text_input_spans(&mut line, &modal.title, &[]);
    } else {
        line.spans.push(Span::raw(modal.title.value.clone()));
    }
    Paragraph::new(line).render(area, buf);
}

fn render_modal_description(modal: &NewIssueModal, area: Rect, buf: &mut Buffer) {
    let active = modal.focused_field == NewIssueField::Description;
    let label = Span::styled(
        if active {
            "[Description]"
        } else {
            " Description "
        },
        if active {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().add_modifier(Modifier::BOLD)
        },
    );
    // Description cursor is always at end (no cursor tracking for multiline).
    let text = if active {
        format!("{}_", modal.description)
    } else {
        modal.description.clone()
    };
    let block = Block::default()
        .title(Line::from(label))
        .borders(Borders::NONE);
    let inner = block.inner(area);
    block.render(area, buf);
    Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .render(inner, buf);
}

impl Widget for &NewIssueForm<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let modal = self.modal;
        let width = pct(area.width, 70);
        let height = 22_u16.min(area.height.saturating_sub(2));
        let x = area.x + area.width.saturating_sub(width) / 2;
        let y = area.y + area.height.saturating_sub(height) / 2;
        let modal_area = Rect::new(x, y, width, height);

        Clear.render(modal_area, buf);

        let block = Block::default()
            .title(format!(
                " New Issue  [Tab next]  [Shift-Tab prev]  [{} submit]  [Esc cancel] ",
                submit_key_label(self.keyboard_enhanced)
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);
        let inner = block.inner(modal_area);
        block.render(modal_area, buf);

        let picker_height = 5_u16;
        let constraints = [
            Constraint::Length(2),                 // 0: Title label+input
            Constraint::Length(picker_height + 1), // 1: Team
            Constraint::Length(picker_height + 1), // 2: Priority
            Constraint::Length(picker_height + 1), // 3: State
            Constraint::Length(picker_height + 1), // 4: Assignee
            Constraint::Min(2),                    // 5: Description
            Constraint::Length(1),                 // 6: error / hint
        ];
        let chunks = Layout::vertical(constraints).split(inner);

        render_modal_title(modal, chunks[0], buf);

        FieldPicker {
            label: "Team",
            items: &modal.teams,
            selected: modal.team_selected,
            active: modal.focused_field == NewIssueField::Team,
            visible_rows: picker_height,
        }
        .render(chunks[1], buf);

        FieldPicker {
            label: "Priority",
            items: &modal.priorities,
            selected: modal.priority_selected,
            active: modal.focused_field == NewIssueField::Priority,
            visible_rows: picker_height,
        }
        .render(chunks[2], buf);

        FieldPicker {
            label: "State",
            items: &modal.states,
            selected: modal.state_selected,
            active: modal.focused_field == NewIssueField::State,
            visible_rows: picker_height,
        }
        .render(chunks[3], buf);

        FieldPicker {
            label: "Assignee",
            items: &modal.assignees,
            selected: modal.assignee_selected,
            active: modal.focused_field == NewIssueField::Assignee,
            visible_rows: picker_height,
        }
        .render(chunks[4], buf);

        render_modal_description(modal, chunks[5], buf);

        let status_text = if modal.loading {
            "Loading...".to_string()
        } else if !modal.error.is_empty() {
            format!("[!] {}", modal.error)
        } else {
            String::new()
        };
        Paragraph::new(status_text).render(chunks[6], buf);
    }
}

/// A single labelled inline list-picker field within the new-issue modal.
struct FieldPicker<'a> {
    label: &'a str,
    items: &'a [PopupItem],
    selected: usize,
    active: bool,
    visible_rows: u16,
}

impl Widget for &FieldPicker<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let FieldPicker {
            label,
            items,
            selected,
            active,
            visible_rows,
        } = *self;
        let label_style_active = Style::new().add_modifier(Modifier::REVERSED);
        let label_style_normal = Style::new().add_modifier(Modifier::BOLD);

        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);

        let label_span = Span::styled(
            if active {
                format!("[{label}]")
            } else {
                format!(" {label} ")
            },
            if active {
                label_style_active
            } else {
                label_style_normal
            },
        );
        // Show currently selected value next to label when not active.
        let selected_preview = if active {
            String::new()
        } else {
            items
                .get(selected)
                .map(|i| format!("  {}", i.label))
                .unwrap_or_default()
        };
        let label_line = Line::from(vec![label_span, Span::raw(selected_preview)]);
        Paragraph::new(label_line).render(chunks[0], buf);

        if !active || items.is_empty() {
            return;
        }

        let visible = (chunks[1].height as usize).min(visible_rows as usize);
        let scroll_offset = if selected >= visible {
            selected - visible + 1
        } else {
            0
        };

        let list_items: Vec<ListItem> = items
            .iter()
            .skip(scroll_offset)
            .take(visible)
            .enumerate()
            .map(|(i, item)| {
                let real_idx = i + scroll_offset;
                let style = if real_idx == selected {
                    Style::new().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                ListItem::new(format!(" {} ", item.label)).style(style)
            })
            .collect();

        List::new(list_items).render(chunks[1], buf);
    }
}
