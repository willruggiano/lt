mod ui;

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;

use crate::issues::IssueArgs;
use crate::issues::detail::fetch_issue_detail_with_config;
use crate::issues::list::{Issue, fetch};
use crate::linear::types::IssueDetail;

pub enum Status {
    Idle,
    Loading,
    Error(String),
}

// ---------------------------------------------------------------------------
// Popup support (bd-3dz)
// ---------------------------------------------------------------------------

/// Identifies which field a popup is editing.
#[derive(Clone)]
pub enum PopupKind {
    State,
    Priority,
    Assignee,
}

/// A single selectable item shown in the generic list-picker popup.
#[derive(Clone)]
pub struct PopupItem {
    /// Human-readable label.
    pub label: String,
    /// Opaque ID sent to the Linear API (state id, assignee id, etc.).
    /// None means "unassign" for the assignee popup.
    pub id: Option<String>,
}

/// Application mode -- only one active at a time.
pub enum Mode {
    /// Normal list browsing mode.
    List,
    /// Filter input overlay is active.
    InputFilter,
    /// Detail pane showing full issue content (bd-2g8).
    Detail,
    /// A generic list-picker popup is open (bd-3dz).
    Popup(PopupKind),
}

pub struct App {
    pub issues: Vec<Issue>,
    pub table_state: TableState,
    pub args: IssueArgs,
    pub has_next_page: bool,
    // Pagination cursors.
    pub current_cursor: Option<String>,
    pub cursor_stack: Vec<Option<String>>,
    pub end_cursor: Option<String>,
    pub status: Status,
    pub quit: bool,
    // Filter overlay (input_mode mirrors Mode::InputFilter for compatibility).
    pub input_mode: bool,
    pub input_buf: String,
    // Set by ui::render each frame so key handlers know page size.
    pub viewport_height: u16,

    // -- mode -----------------------------------------------------------------
    pub mode: Mode,

    // -- detail pane (bd-2g8) -------------------------------------------------
    /// Loaded detail for the currently-open issue.
    pub detail: Option<IssueDetail>,
    /// Vertical scroll offset inside the detail pane (in lines).
    pub detail_scroll: u16,

    // -- popup state (bd-3dz) -------------------------------------------------
    pub popup_items: Vec<PopupItem>,
    pub popup_selected: usize,

    // -- footer message (bd-3dz) ----------------------------------------------
    pub footer_msg: Option<String>,
}

impl App {
    fn new(
        issues: Vec<Issue>,
        has_next_page: bool,
        end_cursor: Option<String>,
        args: IssueArgs,
    ) -> Self {
        let mut table_state = TableState::default();
        if !issues.is_empty() {
            table_state.select(Some(0));
        }
        Self {
            issues,
            table_state,
            args,
            has_next_page,
            current_cursor: None,
            cursor_stack: Vec::new(),
            end_cursor,
            status: Status::Idle,
            quit: false,
            input_mode: false,
            input_buf: String::new(),
            viewport_height: 0,
            mode: Mode::List,
            detail: None,
            detail_scroll: 0,
            popup_items: Vec::new(),
            popup_selected: 0,
            footer_msg: None,
        }
    }

    fn selected_issue(&self) -> Option<&Issue> {
        self.table_state.selected().and_then(|i| self.issues.get(i))
    }

    fn selected_issue_mut(&mut self) -> Option<&mut Issue> {
        self.table_state
            .selected()
            .and_then(|i| self.issues.get_mut(i))
    }

    fn move_by(&mut self, delta: i32) {
        let n = self.issues.len();
        if n == 0 {
            return;
        }
        let i = self.table_state.selected().unwrap_or(0) as i32;
        let new_i = (i + delta).clamp(0, n as i32 - 1) as usize;
        self.table_state.select(Some(new_i));
    }

    fn move_down(&mut self) {
        self.move_by(1);
    }
    fn move_up(&mut self) {
        self.move_by(-1);
    }
    fn move_top(&mut self) {
        self.move_by(i32::MIN / 2);
    }
    fn move_bottom(&mut self) {
        self.move_by(i32::MAX / 2);
    }
    fn page_down(&mut self) {
        self.move_by(self.viewport_height as i32);
    }
    fn page_up(&mut self) {
        self.move_by(-(self.viewport_height as i32));
    }
    fn half_page_down(&mut self) {
        self.move_by(self.viewport_height as i32 / 2);
    }
    fn half_page_up(&mut self) {
        self.move_by(-(self.viewport_height as i32 / 2));
    }

    fn do_fetch(&mut self, reset_selection: bool) {
        self.status = Status::Loading;
        let cursor = self.current_cursor.as_deref();
        match fetch(&self.args, cursor) {
            Ok((issues, has_next_page, end_cursor)) => {
                self.issues = issues;
                self.has_next_page = has_next_page;
                self.end_cursor = end_cursor;
                let n = self.issues.len();
                let sel = if reset_selection {
                    0
                } else {
                    self.table_state
                        .selected()
                        .unwrap_or(0)
                        .min(n.saturating_sub(1))
                };
                self.table_state
                    .select(if n > 0 { Some(sel) } else { None });
                self.status = Status::Idle;
            }
            Err(e) => {
                self.status = Status::Error(e.to_string());
            }
        }
    }

    fn refresh(&mut self) {
        self.do_fetch(false);
    }

    fn cycle_sort(&mut self) {
        self.args.sort = self.args.sort.next();
        self.cursor_stack.clear();
        self.current_cursor = None;
        self.do_fetch(true);
    }

    fn toggle_desc(&mut self) {
        self.args.desc = !self.args.desc;
        self.cursor_stack.clear();
        self.current_cursor = None;
        self.do_fetch(true);
    }

    fn next_page(&mut self) {
        if !self.has_next_page {
            return;
        }
        let end = self.end_cursor.clone();
        self.cursor_stack.push(self.current_cursor.clone());
        self.current_cursor = end;
        self.do_fetch(true);
    }

    fn prev_page(&mut self) {
        if self.cursor_stack.is_empty() {
            return;
        }
        self.current_cursor = self.cursor_stack.pop().unwrap();
        self.do_fetch(true);
    }

    // -- Detail pane (bd-2g8) -------------------------------------------------

    /// Open the detail pane for the currently selected issue.
    fn open_detail(&mut self) {
        let id = match self.selected_issue() {
            Some(issue) => issue.identifier.clone(),
            None => return,
        };
        self.mode = Mode::Detail;
        self.detail = None;
        self.detail_scroll = 0;
        self.status = Status::Loading;
        match fetch_issue_detail_with_config(&id) {
            Ok(detail) => {
                self.detail = Some(detail);
                self.status = Status::Idle;
            }
            Err(e) => {
                self.status = Status::Error(e.to_string());
            }
        }
    }

    /// Close the detail pane and return to the list.
    fn close_detail(&mut self) {
        self.mode = Mode::List;
        self.detail = None;
        self.detail_scroll = 0;
        self.status = Status::Idle;
    }

    fn detail_scroll_down(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_add(1);
    }

    fn detail_scroll_up(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_sub(1);
    }

    // -- Popup helpers (bd-3dz) -----------------------------------------------

    fn open_state_popup(&mut self) {
        let issue = match self.selected_issue() {
            Some(i) => i.clone(),
            None => return,
        };
        let token = match crate::config::load_token() {
            Ok(Some(t)) => t,
            _ => {
                self.footer_msg = Some("Not logged in".to_string());
                return;
            }
        };
        let current_state_name = issue.state.name.clone();
        match crate::linear::mutations::fetch_workflow_states(&token.access_token, &issue.team.id) {
            Ok(states) => {
                self.popup_items = states
                    .into_iter()
                    .map(|s| PopupItem {
                        label: s.name,
                        id: Some(s.id),
                    })
                    .collect();
                self.popup_selected = self
                    .popup_items
                    .iter()
                    .position(|item| item.label == current_state_name)
                    .unwrap_or(0);
                self.mode = Mode::Popup(PopupKind::State);
                self.footer_msg = None;
            }
            Err(e) => {
                self.footer_msg = Some(format!("Failed to fetch states: {}", e));
            }
        }
    }

    fn open_priority_popup(&mut self) {
        if self.selected_issue().is_none() {
            return;
        }
        let priority = self.selected_issue().unwrap().priority;
        // Linear priority: 0=No priority, 1=Urgent, 2=High, 3=Normal, 4=Low
        self.popup_items = vec![
            PopupItem {
                label: "No priority".to_string(),
                id: Some("0".to_string()),
            },
            PopupItem {
                label: "Urgent".to_string(),
                id: Some("1".to_string()),
            },
            PopupItem {
                label: "High".to_string(),
                id: Some("2".to_string()),
            },
            PopupItem {
                label: "Normal".to_string(),
                id: Some("3".to_string()),
            },
            PopupItem {
                label: "Low".to_string(),
                id: Some("4".to_string()),
            },
        ];
        self.popup_selected = priority as usize;
        self.mode = Mode::Popup(PopupKind::Priority);
        self.footer_msg = None;
    }

    fn open_assignee_popup(&mut self) {
        let issue = match self.selected_issue() {
            Some(i) => i.clone(),
            None => return,
        };
        let token = match crate::config::load_token() {
            Ok(Some(t)) => t,
            _ => {
                self.footer_msg = Some("Not logged in".to_string());
                return;
            }
        };
        let mut items: Vec<PopupItem> = vec![PopupItem {
            label: "Unassign".to_string(),
            id: None,
        }];
        match fetch_team_members(&token.access_token, &issue.team.id) {
            Ok(members) => {
                for m in members {
                    items.push(PopupItem {
                        label: m.name,
                        id: Some(m.id),
                    });
                }
            }
            Err(e) => {
                self.footer_msg = Some(format!("Failed to fetch members: {}", e));
                return;
            }
        }
        self.popup_selected = issue
            .assignee
            .as_ref()
            .and_then(|a| {
                items
                    .iter()
                    .position(|item| item.id.as_deref() == Some(a.id.as_str()))
            })
            .unwrap_or(0);
        self.popup_items = items;
        self.mode = Mode::Popup(PopupKind::Assignee);
        self.footer_msg = None;
    }

    fn popup_move(&mut self, delta: i32) {
        let n = self.popup_items.len();
        if n == 0 {
            return;
        }
        let i = self.popup_selected as i32;
        self.popup_selected = (i + delta).clamp(0, n as i32 - 1) as usize;
    }

    fn popup_confirm(&mut self) {
        let kind = match &self.mode {
            Mode::Popup(k) => k.clone(),
            _ => return,
        };
        let item = match self.popup_items.get(self.popup_selected) {
            Some(i) => i.clone(),
            None => return,
        };
        let issue = match self.selected_issue() {
            Some(i) => i.clone(),
            None => return,
        };

        // 1. Optimistic SQLite update.
        optimistic_update_sqlite(&issue, &kind, &item);

        // 2. Update in-memory issue list for instant feedback.
        apply_optimistic_in_memory(self, &kind, &item);

        // 3. Fire mutation in background thread.
        let issue_id: String = issue.id.clone();
        let kind2: PopupKind = kind.clone();
        let item2: PopupItem = item.clone();
        let orig_issue: crate::issues::list::Issue = issue.clone();

        std::thread::spawn(move || {
            let token = match crate::config::load_token() {
                Ok(Some(t)) => t,
                _ => return,
            };
            let result: anyhow::Result<()> = match kind2 {
                PopupKind::State => {
                    if let Some(state_id) = &item2.id {
                        crate::linear::mutations::update_issue_state(
                            &token.access_token,
                            &issue_id,
                            state_id,
                        )
                        .map(|_| ())
                    } else {
                        Ok(())
                    }
                }
                PopupKind::Priority => {
                    if let Some(pstr) = &item2.id {
                        let p: u8 = pstr.parse().unwrap_or(0);
                        crate::linear::mutations::update_issue_priority(
                            &token.access_token,
                            &issue_id,
                            p,
                        )
                        .map(|_| ())
                    } else {
                        Ok(())
                    }
                }
                PopupKind::Assignee => crate::linear::mutations::update_issue_assignee(
                    &token.access_token,
                    &issue_id,
                    item2.id.clone(),
                )
                .map(|_| ()),
            };
            if let Err(_e) = result {
                // On failure: revert SQLite to the original values.
                revert_sqlite(&orig_issue, &kind2);
            }
        });

        self.mode = Mode::List;
    }

    fn popup_cancel(&mut self) {
        self.mode = Mode::List;
    }
}

// ---------------------------------------------------------------------------
// Team member fetch (used by assignee popup)
// ---------------------------------------------------------------------------

struct Member {
    pub id: String,
    pub name: String,
}

fn fetch_team_members(token: &str, team_id: &str) -> Result<Vec<Member>> {
    use serde::Deserialize;
    use serde_json::json;

    const TEAM_MEMBERS_QUERY: &str = r#"
query TeamMembers($teamId: String!) {
  team(id: $teamId) {
    members {
      nodes {
        id
        name
      }
    }
  }
}
"#;

    #[derive(Deserialize)]
    struct MemberNode {
        id: String,
        name: String,
    }
    #[derive(Deserialize)]
    struct MemberConnection {
        nodes: Vec<MemberNode>,
    }
    #[derive(Deserialize)]
    struct TeamData {
        members: MemberConnection,
    }
    #[derive(Deserialize)]
    struct TeamWrapper {
        team: TeamData,
    }

    let variables = json!({ "teamId": team_id });
    let data: TeamWrapper =
        crate::linear::client::graphql_query(token, TEAM_MEMBERS_QUERY, variables)?;
    Ok(data
        .team
        .members
        .nodes
        .into_iter()
        .map(|m| Member {
            id: m.id,
            name: m.name,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Optimistic SQLite helpers (bd-3dz)
// ---------------------------------------------------------------------------

fn optimistic_update_sqlite(
    issue: &crate::issues::list::Issue,
    kind: &PopupKind,
    item: &PopupItem,
) {
    let conn = match crate::db::open_db() {
        Ok(c) => c,
        Err(_) => return,
    };
    let db_issue = build_db_issue_optimistic(issue, kind, item);
    let _ = crate::db::upsert_issues(&conn, &[db_issue]);
}

fn revert_sqlite(orig: &crate::issues::list::Issue, _kind: &PopupKind) {
    let conn = match crate::db::open_db() {
        Ok(c) => c,
        Err(_) => return,
    };
    let db_issue = crate::db::Issue {
        id: orig.id.clone(),
        identifier: orig.identifier.clone(),
        title: orig.title.clone(),
        priority_label: orig.priority_label.clone(),
        state_name: orig.state.name.clone(),
        assignee_name: orig.assignee.as_ref().map(|a| a.name.clone()),
        team_name: orig.team.name.clone(),
        team_key: Some(orig.team.id.clone()),
        created_at: orig.created_at.clone(),
        updated_at: orig.updated_at.clone(),
        synced_at: chrono::Utc::now().to_rfc3339(),
    };
    let _ = crate::db::upsert_issues(&conn, &[db_issue]);
}

fn build_db_issue_optimistic(
    issue: &crate::issues::list::Issue,
    kind: &PopupKind,
    item: &PopupItem,
) -> crate::db::Issue {
    let priority_label = match kind {
        PopupKind::Priority => item.label.clone(),
        _ => issue.priority_label.clone(),
    };
    let state_name = match kind {
        PopupKind::State => item.label.clone(),
        _ => issue.state.name.clone(),
    };
    let assignee_name = match kind {
        PopupKind::Assignee => {
            if item.id.is_none() {
                None
            } else {
                Some(item.label.clone())
            }
        }
        _ => issue.assignee.as_ref().map(|a| a.name.clone()),
    };
    crate::db::Issue {
        id: issue.id.clone(),
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        priority_label,
        state_name,
        assignee_name,
        team_name: issue.team.name.clone(),
        team_key: Some(issue.team.id.clone()),
        created_at: issue.created_at.clone(),
        updated_at: issue.updated_at.clone(),
        synced_at: chrono::Utc::now().to_rfc3339(),
    }
}

fn apply_optimistic_in_memory(app: &mut App, kind: &PopupKind, item: &PopupItem) {
    let issue = match app.selected_issue_mut() {
        Some(i) => i,
        None => return,
    };
    match kind {
        PopupKind::State => {
            issue.state.name = item.label.clone();
            if let Some(id) = &item.id {
                issue.state.id = id.clone();
            }
        }
        PopupKind::Priority => {
            issue.priority_label = item.label.clone();
            if let Some(pstr) = &item.id {
                issue.priority = pstr.parse().unwrap_or(issue.priority);
            }
        }
        PopupKind::Assignee => {
            if item.id.is_none() {
                issue.assignee = None;
            } else {
                issue.assignee = Some(crate::issues::list::User {
                    id: item.id.clone().unwrap_or_default(),
                    name: item.label.clone(),
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

pub fn run(args: IssueArgs) -> Result<()> {
    let (issues, has_next_page, end_cursor) = fetch(&args, None)?;
    let app = App::new(issues, has_next_page, end_cursor, args);
    let mut terminal = ratatui::init();
    let result = run_app(&mut terminal, app);
    ratatui::restore();
    result
}

fn run_app(terminal: &mut ratatui::DefaultTerminal, mut app: App) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::render(frame, &mut app))?;

        if app.quit {
            return Ok(());
        }

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match app.mode {
                    Mode::InputFilter => handle_input_key(&mut app, key.code),
                    Mode::Popup(_) => handle_popup_key(&mut app, key.code),
                    Mode::Detail => handle_detail_key(&mut app, key.code),
                    Mode::List => handle_normal_key(&mut app, key.code, key.modifiers),
                }
            }
        }
    }
}

fn handle_input_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => {
            app.mode = Mode::List;
            app.input_mode = false;
            app.input_buf.clear();
        }
        KeyCode::Enter => {
            app.mode = Mode::List;
            app.input_mode = false;
            let query = app.input_buf.trim().to_string();
            app.args.title = if query.is_empty() { None } else { Some(query) };
            app.input_buf.clear();
            app.cursor_stack.clear();
            app.current_cursor = None;
            app.do_fetch(true);
        }
        KeyCode::Backspace => {
            app.input_buf.pop();
        }
        KeyCode::Char(c) => {
            app.input_buf.push(c);
        }
        _ => {}
    }
}

// -- Popup key handler (bd-3dz) ----------------------------------------------

fn handle_popup_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('j') | KeyCode::Down => app.popup_move(1),
        KeyCode::Char('k') | KeyCode::Up => app.popup_move(-1),
        KeyCode::Enter => app.popup_confirm(),
        KeyCode::Esc => app.popup_cancel(),
        _ => {}
    }
}

// -- Detail pane keybindings (bd-2g8) ----------------------------------------

fn handle_detail_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc | KeyCode::Char('q') => app.close_detail(),
        KeyCode::Char('j') | KeyCode::Down => app.detail_scroll_down(),
        KeyCode::Char('k') | KeyCode::Up => app.detail_scroll_up(),
        KeyCode::Char('o') => {
            if let Some(detail) = &app.detail {
                let url = format!("https://linear.app/issue/{}", detail.identifier);
                let _ = open::that(url);
            }
        }
        _ => {}
    }
}

// -- Normal list keybindings -------------------------------------------------

fn handle_normal_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.quit = true,
        // Open detail pane (bd-2g8)
        KeyCode::Enter => app.open_detail(),
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
        KeyCode::Char('g') => app.move_top(),
        KeyCode::Char('G') => app.move_bottom(),
        KeyCode::Char('d') if ctrl => app.half_page_down(),
        KeyCode::Char('u') if ctrl => app.half_page_up(),
        KeyCode::Char('n') if ctrl => app.next_page(),
        KeyCode::Char('p') if ctrl => app.prev_page(),
        KeyCode::PageDown => app.page_down(),
        KeyCode::PageUp => app.page_up(),
        KeyCode::Char('o') => {
            if let Some(issue) = app.selected_issue() {
                let url = format!("https://linear.app/issue/{}", issue.identifier);
                let _ = open::that(url);
            }
        }
        KeyCode::Char('r') => app.refresh(),
        // 'S' (capital) cycles sort field to avoid collision with 's' (state popup)
        KeyCode::Char('S') => app.cycle_sort(),
        KeyCode::Char('d') => app.toggle_desc(),
        KeyCode::Char('/') => {
            app.input_buf = app.args.title.clone().unwrap_or_default();
            app.input_mode = true;
            app.mode = Mode::InputFilter;
        }
        // Write op keybindings (bd-3dz)
        KeyCode::Char('s') => app.open_state_popup(),
        KeyCode::Char('p') => app.open_priority_popup(),
        KeyCode::Char('a') => app.open_assignee_popup(),
        _ => {}
    }
}
