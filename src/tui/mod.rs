mod ui;

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;

use crate::issues::IssueArgs;
use crate::issues::list::{Issue, fetch};

pub enum Status {
    Idle,
    Loading,
    Error(String),
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
                    handle_normal_key(&mut app, key.code, key.modifiers);
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

fn handle_normal_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.quit = true,
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
        KeyCode::Char('/') => {
            app.input_buf = app.args.title.clone().unwrap_or_default();
            app.input_mode = true;
        }
        _ => {}
    }
}
