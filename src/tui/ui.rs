use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, Wrap,
};

use super::{
    ALL_KEYBINDINGS, App, HelpPopup, Mode, NewIssueField, NewIssueModal, PopupKind, SearchOverlay,
    Status, TextInput,
};
use crate::issues::SortField;
use crate::issues::list::Issue;
use crate::linear::types::IssueDetail;

pub fn render(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1), // spacer
        Constraint::Length(1),
    ])
    .split(frame.area());

    // Expose visible row count to key handlers (subtract table header row).
    app.viewport_height = chunks[2].height.saturating_sub(1);

    let context = super::search_query::render_filter_context(&app.active_filter);
    let has_next = app.has_next_page;
    let has_prev = !app.cursor_stack.is_empty();
    let page = app.cursor_stack.len() + 1;
    let input_mode = app.input_mode;
    let input_buf = app.input_buf.clone();

    // Always render the header with user/org context. In search mode, append
    // the search query inline so the identity is always visible (bd-1l9).
    if let Mode::Search = app.mode
        && let Some(ref overlay) = app.search_overlay
    {
        render_header_with_search(
            frame,
            chunks[0],
            app.viewer_name.as_deref(),
            app.org_name.as_deref(),
            overlay,
        );
    } else {
        render_header(
            frame,
            chunks[0],
            &context,
            app.viewer_name.as_deref(),
            app.org_name.as_deref(),
        );
    }

    // Always render the full-width table so column widths never change.
    render_table(frame, chunks[2], app);

    // Render the spacer row between the issue table and the statusbar so the
    // terminal cell buffer is explicitly cleared (chunk[3]).
    frame.render_widget(Paragraph::new(""), chunks[3]);

    match app.mode {
        Mode::Detail => {
            render_detail_footer(frame, chunks[4]);
        }
        _ => {
            if input_mode {
                render_input(frame, chunks[4], &input_buf);
            } else if let Some(msg) = &app.footer_msg {
                frame.render_widget(Paragraph::new(format!("[!] {}", msg)), chunks[4]);
            } else {
                let sync_label = app.sync_status_label.clone();
                render_footer(frame, chunks[4], has_next, has_prev, page, &sync_label);
            }
        }
    }

    // Render detail overlay on top if active (bd-2ek).
    if let Mode::Detail = app.mode {
        render_detail_overlay(frame, chunks[2], app);
    }

    // Render popup on top if active.
    if let Mode::Popup(ref kind) = app.mode {
        render_popup(
            frame,
            frame.area(),
            app.popup_anchor,
            kind,
            &app.popup_items,
            app.popup_selected,
        );
    }

    // Render new-issue modal on top if active.
    if let Mode::NewIssue = app.mode
        && let Some(ref modal) = app.new_issue_modal
    {
        render_new_issue_modal(frame, frame.area(), modal);
    }

    // Render help popup on top if active (bd-5lz).
    if let Mode::Help = app.mode
        && let Some(ref popup) = app.help_popup
    {
        render_help_popup(frame, frame.area(), popup);
    }

    // Render FTS search overlay (bd-2g4).
    if let Mode::Search = app.mode
        && let Some(ref mut overlay) = app.search_overlay
    {
        render_search_overlay(frame, chunks, overlay, &app.args.sort, app.args.desc);
    }
}

// -- header ------------------------------------------------------------------

fn render_header(
    frame: &mut Frame,
    area: Rect,
    context: &str,
    viewer_name: Option<&str>,
    org_name: Option<&str>,
) {
    let mut parts: Vec<String> = Vec::new();
    if let Some(u) = viewer_name {
        parts.push(format!("user:{}", u));
    }
    if let Some(o) = org_name {
        parts.push(format!("org:{}", o));
    }
    // When no identity info is available the user is not authenticated.
    // Show a clear placeholder so the header is never silently blank.
    let identity = if parts.is_empty() {
        "(not authenticated)".to_string()
    } else {
        parts.join("  ")
    };
    let text = if context.is_empty() {
        identity
    } else {
        format!("{}  {}", identity, context)
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::new().add_modifier(Modifier::BOLD)),
        area,
    );
}

fn render_header_with_search(
    frame: &mut Frame,
    area: Rect,
    viewer_name: Option<&str>,
    org_name: Option<&str>,
    overlay: &super::SearchOverlay,
) {
    let mut line = Line::default();

    // Build identity prefix spans.
    let mut identity_parts: Vec<String> = Vec::new();
    if let Some(u) = viewer_name {
        identity_parts.push(format!("user:{}", u));
    }
    if let Some(o) = org_name {
        identity_parts.push(format!("org:{}", o));
    }
    // When no identity info is available, show the unauthenticated placeholder.
    let identity = if identity_parts.is_empty() {
        "(not authenticated)".to_string()
    } else {
        identity_parts.join("  ")
    };

    if overlay.fts_unavailable {
        let prefix = format!("{}  ", identity);
        line.spans.push(Span::styled(
            format!("{}Search unavailable: run lt sync first", prefix),
            Style::new().add_modifier(Modifier::BOLD),
        ));
    } else {
        line.spans.push(Span::styled(
            format!("{}  ", identity),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        append_text_input_spans(&mut line, &overlay.query, &overlay.ast.errors);
        // Append inline ghost-text suffix hint (bd-22l).
        if let Some(suffix) = overlay.completer.hint_suffix() {
            line.spans.push(Span::styled(
                suffix.to_owned(),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    frame.render_widget(Paragraph::new(line), area);
}

// -- footer / input overlay --------------------------------------------------

fn render_footer(
    frame: &mut Frame,
    area: Rect,
    has_next: bool,
    has_prev: bool,
    page: usize,
    sync_label: &str,
) {
    let mut parts: Vec<&str> = vec![
        "q quit",
        "/ filter",
        "? help",
        "j/k nav",
        "<space> detail",
        "n new",
    ];
    if has_prev {
        parts.push("ctrl+p prev");
    }
    if has_next {
        parts.push("ctrl+n next");
    }

    let page_str = format!("[{}]", page);
    // Show sync status on the right side, separated from page indicator.
    let sync_str = format!("  {}  {}", sync_label, page_str);
    let chunks = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(sync_str.len() as u16),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(parts.join("  ")), chunks[0]);
    frame.render_widget(Paragraph::new(sync_str), chunks[1]);
}

fn render_input(frame: &mut Frame, area: Rect, buf: &str) {
    frame.render_widget(Paragraph::new(format!("/ {}_", buf)), area);
}

// -- table -------------------------------------------------------------------

fn render_table(frame: &mut Frame, area: Rect, app: &mut App) {
    let overlay: Option<String> = match &app.status {
        Status::Error(msg) => Some(format!("Error: {}", msg)),
        Status::Loading => Some("Loading...".to_string()),
        Status::Idle => None,
    };
    if let Some(msg) = overlay {
        frame.render_widget(Paragraph::new(msg), area);
        return;
    }

    if app.issues.is_empty() {
        frame.render_widget(Paragraph::new("No issues found."), area);
        return;
    }

    let sort_col = sort_col_index(&app.args.sort);
    let sort_marker = if app.args.desc { "-" } else { "+" };
    let base_headers: [&str; 7] = [
        "IDENTIFIER",
        "TITLE",
        "STATE",
        "PRIORITY",
        "ASSIGNEE",
        "TEAM",
        "UPDATED",
    ];
    let headers: [String; 7] = std::array::from_fn(|i| {
        if Some(i) == sort_col {
            format!("{} {}", base_headers[i], sort_marker)
        } else {
            base_headers[i].to_string()
        }
    });

    let mut widths: [usize; 7] = headers.each_ref().map(|h| h.len());
    for issue in &app.issues {
        let row = row_cells(issue);
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    let header = Row::new(headers.map(Cell::from)).style(Style::new().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = app
        .issues
        .iter()
        .map(|issue| Row::new(row_cells(issue).map(Cell::from)))
        .collect();

    let constraints: Vec<Constraint> = widths
        .iter()
        .map(|w| Constraint::Length(*w as u16))
        .collect();

    let table = Table::new(rows, constraints)
        .header(header)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .column_spacing(2);

    frame.render_stateful_widget(table, area, &mut app.table_state);

    // Compute anchor rect for the popup (bd-116).
    // Column mapping: 2=State, 3=Priority, 4=Assignee.
    // We position the anchor below the selected row at the relevant column x.
    if let super::Mode::Popup(ref kind) = app.mode {
        let col_idx: usize = match kind {
            super::PopupKind::State => 2,
            super::PopupKind::Priority => 3,
            super::PopupKind::Assignee => 4,
        };
        // Compute x offset of the target column (each column is widths[i] + 2 spacing).
        let col_x: u16 = widths[..col_idx].iter().map(|w| *w as u16 + 2).sum::<u16>() + area.x;
        let col_w = widths[col_idx] as u16;
        // Row y: area.y + 1 (header) + selected index + 1 (below row).
        let sel = app.table_state.selected().unwrap_or(0) as u16;
        let row_y = area.y + 1 + sel + 1;
        app.popup_anchor = Some(ratatui::layout::Rect::new(col_x, row_y, col_w, 1));
    }
}

fn row_cells(issue: &Issue) -> [String; 7] {
    [
        issue.identifier.clone(),
        truncate(&issue.title, 40),
        issue.state.name.clone(),
        issue.priority_label.clone(),
        issue
            .assignee
            .as_ref()
            .map(|u| u.name.clone())
            .unwrap_or_else(|| "-".to_string()),
        issue.team.name.clone(),
        date(&issue.updated_at).to_string(),
    ]
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

fn date(s: &str) -> &str {
    if s.len() >= 10 { &s[..10] } else { s }
}

// Returns the column index (0-6) that corresponds to the active sort field, if any.
fn sort_col_index(field: &SortField) -> Option<usize> {
    match field {
        SortField::Title => Some(1),
        SortField::State => Some(2),
        SortField::Priority => Some(3),
        SortField::Assignee => Some(4),
        SortField::Team => Some(5),
        SortField::Updated => Some(6),
        SortField::Created => None,
    }
}

// -- Detail overlay (bd-2ek) -------------------------------------------------

/// Render the issue detail as a floating overlay over the right ~60% of the
/// content area. The underlying issue list is drawn at full width first, so
/// column widths are never affected by opening the detail view.
fn render_detail_overlay(frame: &mut Frame, area: Rect, app: &App) {
    // Overlay covers the right 60% of the content area.
    let overlay_width = area.width * 3 / 5;
    let overlay_x = area.x + area.width - overlay_width;
    let overlay_area = Rect::new(overlay_x, area.y, overlay_width, area.height);

    // Clear the background so the list does not bleed through.
    frame.render_widget(Clear, overlay_area);
    render_detail(frame, overlay_area, app);
}

// -- Detail pane (bd-2g8) ----------------------------------------------------

fn render_detail(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::LEFT).title(" Detail ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Show loading / error overlay if applicable.
    match &app.status {
        Status::Loading => {
            frame.render_widget(Paragraph::new("Loading..."), inner);
            return;
        }
        Status::Error(msg) => {
            frame.render_widget(Paragraph::new(format!("Error: {}", msg)), inner);
            return;
        }
        Status::Idle => {}
    }

    if let Some(detail) = &app.detail {
        let lines = build_detail_lines(detail);
        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0));
        frame.render_widget(para, inner);
    }
}

fn build_detail_lines(d: &IssueDetail) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Header line: IDENTIFIER - Title
    lines.push(Line::from(vec![
        Span::styled(
            d.identifier.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" - {}", d.title)),
    ]));

    // Meta line: state, priority, assignee, team
    let assignee = d
        .assignee
        .as_ref()
        .map(|u| u.name.clone())
        .unwrap_or_else(|| "unassigned".to_string());
    lines.push(Line::from(format!(
        "[{}]  {}  {}  {}",
        d.state.name, d.priority_label, assignee, d.team.name
    )));

    // Parent issue reference
    if let Some(ref parent) = d.parent {
        lines.push(Line::from(format!(
            "Parent: {} - {}",
            parent.identifier, parent.title
        )));
    }

    // Sub-issues
    if !d.children.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Sub-issues",
            Style::new().add_modifier(Modifier::UNDERLINED),
        )));
        for child in &d.children {
            lines.push(Line::from(format!(
                "  [{}] {} - {}",
                child.state_name, child.identifier, child.title
            )));
        }
    }

    lines.push(Line::from(""));

    // Description
    if let Some(desc) = &d.description
        && !desc.is_empty()
    {
        lines.push(Line::from(Span::styled(
            "Description",
            Style::new().add_modifier(Modifier::UNDERLINED),
        )));
        lines.push(Line::from(""));
        for raw_line in desc.lines() {
            lines.push(Line::from(strip_markdown(raw_line)));
        }
        lines.push(Line::from(""));
    }

    // Comments
    if !d.comments.nodes.is_empty() {
        lines.push(Line::from(Span::styled(
            "Comments",
            Style::new().add_modifier(Modifier::UNDERLINED),
        )));
        for comment in &d.comments.nodes {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("{} on {}", comment.author(), date(&comment.created_at)),
                Style::new().add_modifier(Modifier::BOLD),
            )));
            for raw_line in comment.body.lines() {
                lines.push(Line::from(strip_markdown(raw_line)));
            }
        }
    }

    lines
}

/// Minimal markdown stripping: remove **bold** markers and __underline__ markers.
fn strip_markdown(s: &str) -> String {
    let s = s.replace("**", "");

    s.replace("__", "")
}

fn render_detail_footer(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new("j/k scroll  o open in browser  Esc/q close"),
        area,
    );
}

// -- Generic list-picker popup (bd-3dz) --------------------------------------

fn render_popup(
    frame: &mut Frame,
    area: Rect,
    anchor: Option<Rect>,
    kind: &PopupKind,
    items: &[super::PopupItem],
    selected: usize,
) {
    let title = match kind {
        PopupKind::State => " Set State ",
        PopupKind::Priority => " Set Priority ",
        PopupKind::Assignee => " Reassign ",
    };

    // Size the popup to fit its contents.
    let max_label = items.iter().map(|i| i.label.len()).max().unwrap_or(10);
    let width = (max_label + 4)
        .max(title.len() + 2)
        .min(area.width as usize) as u16;
    let height = (items.len() + 2).min(area.height as usize) as u16;

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

// -- New-issue modal (bd-l6r) ------------------------------------------------

fn render_new_issue_modal(frame: &mut Frame, area: Rect, modal: &NewIssueModal) {
    // Modal dimensions: 70% wide, 22 rows tall, centred.
    let width = (area.width as f32 * 0.70) as u16;
    let height = 22_u16.min(area.height.saturating_sub(2));
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let modal_area = Rect::new(x, y, width, height);

    // Clear the area under the modal.
    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .title(" New Issue  [Tab next]  [Shift-Tab prev]  [Ctrl-Enter submit]  [Esc cancel] ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    // Layout: fields stacked vertically.
    // Title (1), Team (picker rows up to 5), Priority (picker), State (picker),
    // Assignee (picker), Description (remaining), error line (1).
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

    // Helper: field label style.
    let label_style_active = Style::new().add_modifier(Modifier::REVERSED);
    let label_style_normal = Style::new().add_modifier(Modifier::BOLD);

    // ---- Title ----
    let title_active = modal.focused_field == NewIssueField::Title;
    let title_label = Span::styled(
        if title_active { "[Title]" } else { " Title " },
        if title_active {
            label_style_active
        } else {
            label_style_normal
        },
    );
    let mut title_line = Line::from(vec![title_label, Span::raw("  ")]);
    if title_active {
        append_text_input_spans(&mut title_line, &modal.title, &[]);
    } else {
        title_line.spans.push(Span::raw(modal.title.value.clone()));
    }
    frame.render_widget(Paragraph::new(title_line), chunks[0]);

    // ---- Team picker ----
    render_field_picker(
        frame,
        chunks[1],
        "Team",
        &modal.teams,
        modal.team_selected,
        modal.focused_field == NewIssueField::Team,
        picker_height,
    );

    // ---- Priority picker ----
    render_field_picker(
        frame,
        chunks[2],
        "Priority",
        &modal.priorities,
        modal.priority_selected,
        modal.focused_field == NewIssueField::Priority,
        picker_height,
    );

    // ---- State picker ----
    render_field_picker(
        frame,
        chunks[3],
        "State",
        &modal.states,
        modal.state_selected,
        modal.focused_field == NewIssueField::State,
        picker_height,
    );

    // ---- Assignee picker ----
    render_field_picker(
        frame,
        chunks[4],
        "Assignee",
        &modal.assignees,
        modal.assignee_selected,
        modal.focused_field == NewIssueField::Assignee,
        picker_height,
    );

    // ---- Description ----
    let desc_active = modal.focused_field == NewIssueField::Description;
    let desc_label = Span::styled(
        if desc_active {
            "[Description]"
        } else {
            " Description "
        },
        if desc_active {
            label_style_active
        } else {
            label_style_normal
        },
    );
    // Description cursor is always at end (no cursor tracking for multiline).
    let desc_text = if desc_active {
        format!("{}_", modal.description)
    } else {
        modal.description.clone()
    };
    let desc_block = Block::default()
        .title(Line::from(desc_label))
        .borders(Borders::NONE);
    let desc_inner = desc_block.inner(chunks[5]);
    frame.render_widget(desc_block, chunks[5]);
    frame.render_widget(
        Paragraph::new(desc_text).wrap(Wrap { trim: false }),
        desc_inner,
    );

    // ---- Error / loading line ----
    let status_text = if modal.loading {
        "Loading...".to_string()
    } else if !modal.error.is_empty() {
        format!("[!] {}", modal.error)
    } else {
        String::new()
    };
    frame.render_widget(Paragraph::new(status_text), chunks[6]);
}

// -- Help popup (bd-5lz) -----------------------------------------------------

fn render_help_popup(frame: &mut Frame, area: Rect, popup: &HelpPopup) {
    // Size: 60% wide, up to 80% tall, centred.
    let width = ((area.width as f32 * 0.60) as u16).max(50).min(area.width);
    let max_rows = (ALL_KEYBINDINGS.len() + 4) as u16; // header + search + border
    let height = max_rows.min((area.height as f32 * 0.80) as u16).max(6);
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

/// Render a labelled inline list-picker for a single form field.
fn render_field_picker(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    items: &[super::PopupItem],
    selected: usize,
    active: bool,
    visible_rows: u16,
) {
    let label_style_active = Style::new().add_modifier(Modifier::REVERSED);
    let label_style_normal = Style::new().add_modifier(Modifier::BOLD);

    // Split: 1 row for label, rest for list.
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);

    let label_span = Span::styled(
        if active {
            format!("[{}]", label)
        } else {
            format!(" {} ", label)
        },
        if active {
            label_style_active
        } else {
            label_style_normal
        },
    );
    // Show currently selected value next to label when not active.
    let selected_preview = if !active {
        items
            .get(selected)
            .map(|i| format!("  {}", i.label))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let label_line = Line::from(vec![label_span, Span::raw(selected_preview)]);
    frame.render_widget(Paragraph::new(label_line), chunks[0]);

    if !active || items.is_empty() {
        return;
    }

    // Compute scroll offset so the selected item is always visible.
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

// -- FTS search overlay (bd-2g4) ---------------------------------------------

fn render_search_overlay(
    frame: &mut Frame,
    chunks: std::rc::Rc<[Rect]>,
    overlay: &mut SearchOverlay,
    sort_field: &SortField,
    sort_desc: bool,
) {
    // The search bar is rendered in the header row (chunks[0]) by render().
    // This function only handles the results in the main content area (chunks[2]).
    let area = chunks[2];

    // When the query is empty, leave the underlying issue table visible (bd-1pe).
    // When FTS is unavailable, show the error but still don't wipe the table.
    if overlay.fts_unavailable {
        // Show an error overlay without hiding the table entirely.
        frame.render_widget(
            Paragraph::new("Search unavailable: run lt sync first"),
            area,
        );
        return;
    }

    if overlay.query.value.trim().is_empty() {
        // No query yet -- keep the underlying list visible.
        return;
    }

    // Keep the underlying list visible when:
    // - a search is queued but hasn't fired yet (debounce pending), or
    // - the overlay was just opened and no search has run yet (bd-zjy).
    // This avoids a flash of empty content or a spurious "No results." on entry.
    if overlay.results.is_empty() && (overlay.last_changed.is_some() || !overlay.has_searched) {
        return;
    }

    frame.render_widget(Clear, area);

    if overlay.results.is_empty() {
        frame.render_widget(Paragraph::new("No results."), area);
        return;
    }

    // Render results as a table identical in style to the main list.
    let sort_col = sort_col_index(sort_field);
    let sort_marker = if sort_desc { "-" } else { "+" };
    let base_headers: [&str; 7] = [
        "IDENTIFIER",
        "TITLE",
        "STATE",
        "PRIORITY",
        "ASSIGNEE",
        "TEAM",
        "UPDATED",
    ];
    let headers: [String; 7] = std::array::from_fn(|i| {
        if Some(i) == sort_col {
            format!("{} {}", base_headers[i], sort_marker)
        } else {
            base_headers[i].to_string()
        }
    });

    let mut widths: [usize; 7] = headers.each_ref().map(|h| h.len());
    for issue in &overlay.results {
        let row = search_row_cells(issue);
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    let header = Row::new(headers.map(Cell::from)).style(Style::new().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = overlay
        .results
        .iter()
        .map(|issue| Row::new(search_row_cells(issue).map(Cell::from)))
        .collect();

    let constraints: Vec<Constraint> = widths
        .iter()
        .map(|w| Constraint::Length(*w as u16))
        .collect();

    let table = Table::new(rows, constraints)
        .header(header)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .column_spacing(2);

    frame.render_stateful_widget(table, area, &mut overlay.table_state);
}

fn search_row_cells(issue: &Issue) -> [String; 7] {
    fn truncate(s: &str, max: usize) -> String {
        if s.len() <= max {
            s.to_string()
        } else {
            format!("{}...", &s[..max.saturating_sub(3)])
        }
    }
    fn date(s: &str) -> &str {
        if s.len() >= 10 { &s[..10] } else { s }
    }
    [
        issue.identifier.clone(),
        truncate(&issue.title, 40),
        issue.state.name.clone(),
        issue.priority_label.clone(),
        issue
            .assignee
            .as_ref()
            .map(|u| u.name.clone())
            .unwrap_or_else(|| "-".to_string()),
        issue.team.name.clone(),
        date(&issue.updated_at).to_string(),
    ]
}

// ---------------------------------------------------------------------------
// TextInput rendering helper
// ---------------------------------------------------------------------------

/// Return true when `byte_offset` falls inside any of the given error spans.
fn byte_in_error(byte_offset: usize, errors: &[super::search_query::ParseError]) -> bool {
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
fn error_segments<'a>(
    text: &'a str,
    offset: usize,
    errors: &[super::search_query::ParseError],
) -> Vec<(&'a str, bool)> {
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
fn push_text_spans(
    line: &mut Line,
    text: &str,
    offset: usize,
    errors: &[super::search_query::ParseError],
) {
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

/// Append spans representing a `TextInput` to an existing `Line`.
///
/// The character at the cursor position is rendered with a reversed
/// (block-cursor) style.  If the cursor is at the end of the string, a
/// space with reversed style is appended to show the cursor position.
///
/// When `input.selection_end` is set, the range cursor..selection_end is
/// rendered with UNDERLINED style (in addition to the block cursor char).
///
/// `errors` is the list of parse errors from the current `QueryAst`.  Any
/// text whose byte range overlaps an error span is rendered with
/// `Color::Red` to give the user a visual signal that a stem was not
/// recognised.  Pass an empty slice when no error highlighting is needed
/// (e.g. modal title input).
pub fn append_text_input_spans(
    line: &mut Line,
    input: &TextInput,
    errors: &[super::search_query::ParseError],
) {
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
                // If there is an active selection, render cursor..selection_end
                // with UNDERLINED style and the remainder normally.
                if let Some(sel_end) = input.selection_end {
                    let sel_end = sel_end.min(input.value.len());
                    let cursor_end = input.cursor + ch.len_utf8();
                    if sel_end > cursor_end {
                        // Selected portion: [cursor+ch_len, sel_end)
                        let sel_text = &input.value[cursor_end..sel_end];
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
                    } else {
                        push_text_spans(line, after, after_offset, errors);
                    }
                } else {
                    push_text_spans(line, after, after_offset, errors);
                }
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
