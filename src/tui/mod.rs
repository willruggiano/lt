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

/// Application mode -- only one active at a time.
pub enum Mode {
    /// Normal list browsing mode.
    List,
    /// Detail pane showing full issue content.
    Detail,
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
    // Filter overlay
    pub input_mode: bool,
    pub input_buf: String,
    // Set by ui::render each frame so key handlers know page size.
    pub viewport_height: u16,
    // -- Detail pane --
    pub mode: Mode,
    /// Loaded detail for the currently-open issue. None while loading or in List mode.
    pub detail: Option<IssueDetail>,
    /// Vertical scroll offset inside the detail pane (in lines).
    pub detail_scroll: u16,
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
        }
    }

    fn selected_issue(&self) -> Option<&Issue> {
        self.table_state.selected().and_then(|i| self.issues.get(i))
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

    // -- Detail pane ----------------------------------------------------------

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
}

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
                if app.input_mode {
                    handle_input_key(&mut app, key.code);
                } else {
                    match app.mode {
                        Mode::Detail => handle_detail_key(&mut app, key.code),
                        Mode::List => handle_normal_key(&mut app, key.code, key.modifiers),
                    }
                }
            }
        }
    }
}

fn handle_input_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => {
            app.input_mode = false;
            app.input_buf.clear();
        }
        KeyCode::Enter => {
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

// -- Detail pane keybindings -------------------------------------------------

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
        // -- Detail pane --
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
        KeyCode::Char('s') => app.cycle_sort(),
        KeyCode::Char('d') => app.toggle_desc(),
        KeyCode::Char('/') => {
            app.input_buf = app.args.title.clone().unwrap_or_default();
            app.input_mode = true;
        }
        _ => {}
    }
}
