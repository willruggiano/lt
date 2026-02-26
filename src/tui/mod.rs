mod ui;

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::widgets::TableState;

use crate::issues::list::{fetch, Issue};
use crate::issues::IssueArgs;

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
    pub status: Status,
}

impl App {
    fn new(issues: Vec<Issue>, has_next_page: bool, args: IssueArgs) -> Self {
        let mut table_state = TableState::default();
        if !issues.is_empty() {
            table_state.select(Some(0));
        }
        Self {
            issues,
            table_state,
            args,
            has_next_page,
            status: Status::Idle,
        }
    }

    fn selected_issue(&self) -> Option<&Issue> {
        self.table_state.selected().and_then(|i| self.issues.get(i))
    }

    fn move_down(&mut self) {
        let n = self.issues.len();
        if n == 0 {
            return;
        }
        let i = self.table_state.selected().unwrap_or(0);
        self.table_state.select(Some((i + 1).min(n - 1)));
    }

    fn move_up(&mut self) {
        if self.issues.is_empty() {
            return;
        }
        let i = self.table_state.selected().unwrap_or(0);
        self.table_state.select(Some(i.saturating_sub(1)));
    }

    fn move_top(&mut self) {
        if !self.issues.is_empty() {
            self.table_state.select(Some(0));
        }
    }

    fn move_bottom(&mut self) {
        let n = self.issues.len();
        if n > 0 {
            self.table_state.select(Some(n - 1));
        }
    }

    fn refresh(&mut self) {
        self.status = Status::Loading;
        match fetch(&self.args) {
            Ok((issues, has_next_page)) => {
                self.issues = issues;
                self.has_next_page = has_next_page;
                let n = self.issues.len();
                let sel = self.table_state.selected().unwrap_or(0).min(n.saturating_sub(1));
                self.table_state.select(if n > 0 { Some(sel) } else { None });
                self.status = Status::Idle;
            }
            Err(e) => {
                self.status = Status::Error(e.to_string());
            }
        }
    }
}

pub fn run(args: IssueArgs) -> Result<()> {
    let (issues, has_next_page) = fetch(&args)?;
    let app = App::new(issues, has_next_page, args);
    let mut terminal = ratatui::init();
    let result = run_app(&mut terminal, app);
    ratatui::restore();
    result
}

fn run_app(terminal: &mut ratatui::DefaultTerminal, mut app: App) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::render(frame, &mut app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('j') | KeyCode::Down => app.move_down(),
                    KeyCode::Char('k') | KeyCode::Up => app.move_up(),
                    KeyCode::Char('g') => app.move_top(),
                    KeyCode::Char('G') => app.move_bottom(),
                    KeyCode::Char('o') => {
                        if let Some(issue) = app.selected_issue() {
                            let url = format!(
                                "https://linear.app/issue/{}",
                                issue.identifier
                            );
                            let _ = open::that(url);
                        }
                    }
                    KeyCode::Char('r') => app.refresh(),
                    _ => {}
                }
            }
        }
    }
}
