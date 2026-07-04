use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap};

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

fn render_modal_title(frame: &mut Frame, area: Rect, modal: &NewIssueModal) {
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
    frame.render_widget(Paragraph::new(line), area);
}

fn render_modal_description(frame: &mut Frame, area: Rect, modal: &NewIssueModal) {
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
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
}

pub(super) fn render_new_issue_modal(
    frame: &mut Frame,
    area: Rect,
    modal: &NewIssueModal,
    keyboard_enhanced: bool,
) {
    let width = pct(area.width, 70);
    let height = 22_u16.min(area.height.saturating_sub(2));
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let modal_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .title(format!(
            " New Issue  [Tab next]  [Shift-Tab prev]  [{} submit]  [Esc cancel] ",
            submit_key_label(keyboard_enhanced)
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

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

    render_modal_title(frame, chunks[0], modal);

    render_field_picker(
        frame,
        chunks[1],
        &FieldPicker {
            label: "Team",
            items: &modal.teams,
            selected: modal.team_selected,
            active: modal.focused_field == NewIssueField::Team,
            visible_rows: picker_height,
        },
    );

    render_field_picker(
        frame,
        chunks[2],
        &FieldPicker {
            label: "Priority",
            items: &modal.priorities,
            selected: modal.priority_selected,
            active: modal.focused_field == NewIssueField::Priority,
            visible_rows: picker_height,
        },
    );

    render_field_picker(
        frame,
        chunks[3],
        &FieldPicker {
            label: "State",
            items: &modal.states,
            selected: modal.state_selected,
            active: modal.focused_field == NewIssueField::State,
            visible_rows: picker_height,
        },
    );

    render_field_picker(
        frame,
        chunks[4],
        &FieldPicker {
            label: "Assignee",
            items: &modal.assignees,
            selected: modal.assignee_selected,
            active: modal.focused_field == NewIssueField::Assignee,
            visible_rows: picker_height,
        },
    );

    render_modal_description(frame, chunks[5], modal);

    let status_text = if modal.loading {
        "Loading...".to_string()
    } else if !modal.error.is_empty() {
        format!("[!] {}", modal.error)
    } else {
        String::new()
    };
    frame.render_widget(Paragraph::new(status_text), chunks[6]);
}

/// A single labelled inline list-picker field within the new-issue modal.
struct FieldPicker<'a> {
    label: &'a str,
    items: &'a [PopupItem],
    selected: usize,
    active: bool,
    visible_rows: u16,
}

fn render_field_picker(frame: &mut Frame, area: Rect, picker: &FieldPicker) {
    let FieldPicker {
        label,
        items,
        selected,
        active,
        visible_rows,
    } = *picker;
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
    frame.render_widget(Paragraph::new(label_line), chunks[0]);

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

    frame.render_widget(List::new(list_items), chunks[1]);
}
