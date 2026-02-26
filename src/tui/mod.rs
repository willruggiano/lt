mod ui;

use std::sync::mpsc;
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
// Background sync events (bd-25j)
// ---------------------------------------------------------------------------

/// Events sent from the background sync thread to the TUI event loop.
pub enum SyncEvent {
    /// Sync completed successfully; includes the refreshed issue list.
    Done(Vec<Issue>),
    /// Sync encountered an error.
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
    /// New-issue modal form (bd-l6r).
    NewIssue,
    /// Searchable help popup (bd-5lz).
    Help,
}

// ---------------------------------------------------------------------------
// Help popup state (bd-5lz)
// ---------------------------------------------------------------------------

/// A single keybinding entry shown in the help popup.
pub struct HelpEntry {
    pub key: &'static str,
    pub description: &'static str,
}

/// All keybindings shown in the help popup.
pub const ALL_KEYBINDINGS: &[HelpEntry] = &[
    HelpEntry { key: "q / Esc",       description: "quit" },
    HelpEntry { key: "j / Down",      description: "move down" },
    HelpEntry { key: "k / Up",        description: "move up" },
    HelpEntry { key: "g",             description: "go to top" },
    HelpEntry { key: "G",             description: "go to bottom" },
    HelpEntry { key: "Ctrl-d",        description: "half page down" },
    HelpEntry { key: "Ctrl-u",        description: "half page up" },
    HelpEntry { key: "PageDown",      description: "page down" },
    HelpEntry { key: "PageUp",        description: "page up" },
    HelpEntry { key: "Enter",         description: "open detail pane" },
    HelpEntry { key: "/",             description: "filter by title" },
    HelpEntry { key: "?",             description: "open this help popup" },
    HelpEntry { key: "n",             description: "new issue" },
    HelpEntry { key: "s",             description: "set state" },
    HelpEntry { key: "p",             description: "set priority" },
    HelpEntry { key: "a",             description: "set assignee" },
    HelpEntry { key: "o",             description: "open in browser" },
    HelpEntry { key: "r",             description: "refresh" },
    HelpEntry { key: "S",             description: "cycle sort field" },
    HelpEntry { key: "d",             description: "toggle sort direction" },
    HelpEntry { key: "Ctrl-n",        description: "next page" },
    HelpEntry { key: "Ctrl-p",        description: "previous page" },
];

/// Mutable state for the help popup.
pub struct HelpPopup {
    /// Current search query typed by the user.
    pub search: String,
    /// Indices into ALL_KEYBINDINGS that match the current search.
    pub filtered: Vec<usize>,
    /// Currently highlighted row in the filtered list.
    pub selected: usize,
}

impl HelpPopup {
    pub fn new() -> Self {
        let filtered = (0..ALL_KEYBINDINGS.len()).collect();
        Self {
            search: String::new(),
            filtered,
            selected: 0,
        }
    }

    pub fn update_filter(&mut self) {
        let q = self.search.to_lowercase();
        self.filtered = ALL_KEYBINDINGS
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                q.is_empty()
                    || e.key.to_lowercase().contains(&q)
                    || e.description.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }
}

// ---------------------------------------------------------------------------
// New-issue modal state (bd-l6r)
// ---------------------------------------------------------------------------

/// Which field of the new-issue form is currently focused.
#[derive(Clone, PartialEq)]
pub enum NewIssueField {
    Title,
    Team,
    Priority,
    State,
    Assignee,
    Description,
}

impl NewIssueField {
    pub fn next(&self) -> Self {
        match self {
            Self::Title => Self::Team,
            Self::Team => Self::Priority,
            Self::Priority => Self::State,
            Self::State => Self::Assignee,
            Self::Assignee => Self::Description,
            Self::Description => Self::Description,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            Self::Title => Self::Title,
            Self::Team => Self::Title,
            Self::Priority => Self::Team,
            Self::State => Self::Priority,
            Self::Assignee => Self::State,
            Self::Description => Self::Assignee,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Team => "Team",
            Self::Priority => "Priority",
            Self::State => "State",
            Self::Assignee => "Assignee",
            Self::Description => "Description",
        }
    }
}

// ---------------------------------------------------------------------------
// Events for modal background loading (bd-vfi)
// ---------------------------------------------------------------------------

/// Events sent from background threads that load modal picker data.
pub enum ModalEvent {
    /// States loaded for the selected team.
    StatesLoaded(Vec<PopupItem>),
    /// Assignees loaded for the selected team, plus an optional viewer ID.
    AssigneesLoaded(Vec<PopupItem>),
    /// Loading error.
    LoadError(String),
}

/// All mutable state for the new-issue modal form.
pub struct NewIssueModal {
    pub focused_field: NewIssueField,

    // Text fields
    pub title: String,
    pub description: String,

    // Picker fields -- each holds a list of items + current selection index.
    pub teams: Vec<PopupItem>,
    pub team_selected: usize,

    pub priorities: Vec<PopupItem>,
    pub priority_selected: usize,

    pub states: Vec<PopupItem>,
    pub state_selected: usize,

    pub assignees: Vec<PopupItem>,
    pub assignee_selected: usize,

    /// True while we are waiting for picker data to load.
    pub loading: bool,
    /// Non-empty when a load or submit error occurred.
    pub error: String,

    /// Receiver for background-loaded modal data (bd-vfi).
    pub modal_rx: Option<mpsc::Receiver<ModalEvent>>,

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

    // -- new-issue modal (bd-l6r) --------------------------------------------
    pub new_issue_modal: Option<NewIssueModal>,

    // -- background sync (bd-25j) --------------------------------------------
    /// Receiver for background sync events.
    pub sync_rx: Option<mpsc::Receiver<SyncEvent>>,
    /// True while a background sync thread is running.
    pub syncing: bool,
    /// Human-readable description of sync status, shown in footer.
    pub sync_status_label: String,

    // -- help popup (bd-5lz) -------------------------------------------------
    pub help_popup: Option<HelpPopup>,
}

impl App {
    fn new(
        issues: Vec<Issue>,
        has_next_page: bool,
        end_cursor: Option<String>,
        args: IssueArgs,
        sync_rx: Option<mpsc::Receiver<SyncEvent>>,
        syncing: bool,
        sync_status_label: String,
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
            new_issue_modal: None,
            sync_rx,
            syncing,
            sync_status_label,
            help_popup: None,
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

    /// Fetch and then seek to the newly created issue by identifier (bd-3ba).
    fn do_fetch_and_select(&mut self, target_identifier: Option<String>) {
        self.do_fetch(true);
        if let Some(id) = target_identifier {
            if let Some(idx) = self.issues.iter().position(|i| i.identifier == id) {
                self.table_state.select(Some(idx));
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

    // -- New-issue modal (bd-l6r) --------------------------------------------

    fn open_new_issue_modal(&mut self) {
        let token = match crate::config::load_token() {
            Ok(Some(t)) => t,
            _ => {
                self.footer_msg = Some("Not logged in".to_string());
                return;
            }
        };

        // Pre-fill team from active filter if set.
        let preset_team = self.args.team.clone();

        let mut modal = NewIssueModal {
            focused_field: NewIssueField::Title,
            title: String::new(),
            description: String::new(),
            teams: Vec::new(),
            team_selected: 0,
            priorities: vec![
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
            ],
            priority_selected: 0,
            states: Vec::new(),
            state_selected: 0,
            assignees: Vec::new(),
            assignee_selected: 0,
            loading: true,
            error: String::new(),
            modal_rx: None,
        };

        // Fetch teams synchronously (fast -- just a list).
        match crate::linear::mutations::fetch_teams(&token.access_token) {
            Ok(teams) => {
                modal.teams = teams
                    .into_iter()
                    .map(|t| PopupItem {
                        label: t.name.clone(),
                        id: Some(t.id),
                    })
                    .collect();
                // Pre-select team from filter.
                if let Some(ref preset) = preset_team {
                    if let Some(idx) = modal
                        .teams
                        .iter()
                        .position(|t| t.label.to_lowercase().contains(&preset.to_lowercase()))
                    {
                        modal.team_selected = idx;
                    }
                }
                modal.loading = false;
            }
            Err(e) => {
                modal.error = format!("Failed to fetch teams: {}", e);
                modal.loading = false;
            }
        }

        self.mode = Mode::NewIssue;
        self.new_issue_modal = Some(modal);
    }

    /// Kick off background loading of states and assignees for the selected team (bd-vfi).
    fn new_issue_load_states_and_assignees_bg(&mut self) {
        let modal = match self.new_issue_modal.as_mut() {
            Some(m) => m,
            None => return,
        };
        let team_id = match modal.teams.get(modal.team_selected).and_then(|t| t.id.clone()) {
            Some(id) => id,
            None => return,
        };

        modal.loading = true;
        modal.error.clear();

        let (tx, rx) = mpsc::channel::<ModalEvent>();
        modal.modal_rx = Some(rx);

        std::thread::spawn(move || {
            let token = match crate::config::load_token() {
                Ok(Some(t)) => t,
                _ => {
                    let _ = tx.send(ModalEvent::LoadError("Not logged in".to_string()));
                    return;
                }
            };

            // Fetch viewer for "me" shortcut (bd-1fz).
            let viewer = fetch_viewer(&token.access_token).ok();

            // Fetch states.
            match crate::linear::mutations::fetch_workflow_states(&token.access_token, &team_id) {
                Ok(states) => {
                    let items: Vec<PopupItem> = states
                        .into_iter()
                        .map(|s| PopupItem {
                            label: s.name,
                            id: Some(s.id),
                        })
                        .collect();
                    let _ = tx.send(ModalEvent::StatesLoaded(items));
                }
                Err(e) => {
                    let _ = tx.send(ModalEvent::LoadError(format!("Failed to fetch states: {}", e)));
                    return;
                }
            }

            // Fetch assignees.
            match fetch_team_members(&token.access_token, &team_id) {
                Ok(members) => {
                    // Build the assignees list: "Me (name)" at top if viewer is known,
                    // then "Unassigned", then team members.
                    let mut items: Vec<PopupItem> = Vec::new();
                    if let Some(ref v) = viewer {
                        items.push(PopupItem {
                            label: format!("Me ({})", v.name),
                            id: Some(v.id.clone()),
                        });
                    }
                    items.push(PopupItem {
                        label: "Unassigned".to_string(),
                        id: None,
                    });
                    for m in members {
                        // Skip the viewer entry since it is already at the top.
                        if viewer.as_ref().map(|v| v.id == m.id).unwrap_or(false) {
                            continue;
                        }
                        items.push(PopupItem {
                            label: m.name,
                            id: Some(m.id),
                        });
                    }
                    let _ = tx.send(ModalEvent::AssigneesLoaded(items));
                }
                Err(e) => {
                    let _ = tx.send(ModalEvent::LoadError(format!("Failed to fetch assignees: {}", e)));
                }
            }
        });
    }

    fn new_issue_submit(&mut self) {
        let token = match crate::config::load_token() {
            Ok(Some(t)) => t,
            _ => {
                if let Some(m) = self.new_issue_modal.as_mut() {
                    m.error = "Not logged in".to_string();
                }
                return;
            }
        };

        let modal = match self.new_issue_modal.as_ref() {
            Some(m) => m,
            None => return,
        };

        if modal.title.trim().is_empty() {
            if let Some(m) = self.new_issue_modal.as_mut() {
                m.error = "Title is required".to_string();
                m.focused_field = NewIssueField::Title;
            }
            return;
        }

        let team_id = match modal
            .teams
            .get(modal.team_selected)
            .and_then(|t| t.id.clone())
        {
            Some(id) => id,
            None => {
                if let Some(m) = self.new_issue_modal.as_mut() {
                    m.error = "Select a team".to_string();
                }
                return;
            }
        };

        let input = crate::linear::mutations::CreateIssueInput {
            title: modal.title.trim().to_string(),
            team_id: team_id.clone(),
            description: if modal.description.trim().is_empty() {
                None
            } else {
                Some(modal.description.trim().to_string())
            },
            state_id: modal
                .states
                .get(modal.state_selected)
                .and_then(|s| s.id.clone()),
            priority: modal
                .priorities
                .get(modal.priority_selected)
                .and_then(|p| p.id.as_ref())
                .and_then(|s| s.parse::<u8>().ok()),
            assignee_id: modal
                .assignees
                .get(modal.assignee_selected)
                .and_then(|a| a.id.clone()),
        };

        let title_for_db = input.title.clone();
        let team_name = modal
            .teams
            .get(modal.team_selected)
            .map(|t| t.label.clone())
            .unwrap_or_default();
        let state_name = modal
            .states
            .get(modal.state_selected)
            .map(|s| s.label.clone())
            .unwrap_or_else(|| "Backlog".to_string());
        let priority_label = modal
            .priorities
            .get(modal.priority_selected)
            .map(|p| p.label.clone())
            .unwrap_or_else(|| "No priority".to_string());
        let assignee_name = modal.assignees.get(modal.assignee_selected).and_then(|a| {
            if a.id.is_some() {
                Some(a.label.clone())
            } else {
                None
            }
        });

        match crate::linear::mutations::create_issue(&token.access_token, input) {
            Ok(created) => {
                // Optimistically insert into SQLite.
                let now = chrono::Utc::now().to_rfc3339();
                let db_issue = crate::db::Issue {
                    id: created.id.clone(),
                    identifier: created.identifier.clone(),
                    title: title_for_db,
                    priority_label,
                    state_name,
                    assignee_name,
                    team_name,
                    team_key: Some(team_id),
                    created_at: now.clone(),
                    updated_at: now,
                    synced_at: chrono::Utc::now().to_rfc3339(),
                };
                if let Ok(conn) = crate::db::open_db() {
                    let _ = crate::db::upsert_issues(&conn, &[db_issue]);
                }
                // Refresh list and highlight new issue (bd-3ba).
                let new_identifier = created.identifier.clone();
                self.mode = Mode::List;
                self.new_issue_modal = None;
                self.footer_msg = Some(format!("Created {}", created.identifier));
                self.do_fetch_and_select(Some(new_identifier));
            }
            Err(e) => {
                if let Some(m) = self.new_issue_modal.as_mut() {
                    m.error = format!("Failed to create issue: {}", e);
                }
            }
        }
    }

    /// Poll modal background channel and update modal state (bd-vfi).
    fn poll_modal_events(&mut self) {
        // Collect events before mutating -- avoids borrow issues.
        let events: Vec<ModalEvent> = {
            let modal = match self.new_issue_modal.as_ref() {
                Some(m) => m,
                None => return,
            };
            let rx = match modal.modal_rx.as_ref() {
                Some(r) => r,
                None => return,
            };
            let mut evts = Vec::new();
            loop {
                match rx.try_recv() {
                    Ok(ev) => evts.push(ev),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => break,
                }
            }
            evts
        };

        for ev in events {
            let modal = match self.new_issue_modal.as_mut() {
                Some(m) => m,
                None => break,
            };
            match ev {
                ModalEvent::StatesLoaded(items) => {
                    modal.states = items;
                    modal.state_selected = 0;
                }
                ModalEvent::AssigneesLoaded(items) => {
                    modal.assignees = items;
                    modal.assignee_selected = 0;
                    modal.loading = false;
                }
                ModalEvent::LoadError(msg) => {
                    modal.error = msg;
                    modal.loading = false;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Viewer query helper (bd-1fz)
// ---------------------------------------------------------------------------

struct ViewerInfo {
    pub id: String,
    pub name: String,
}

fn fetch_viewer(token: &str) -> Result<ViewerInfo> {
    use serde::Deserialize;
    use serde_json::json;

    const VIEWER_QUERY: &str = r#"
query Viewer {
  viewer {
    id
    name
  }
}
"#;

    #[derive(Deserialize)]
    struct ViewerNode {
        id: String,
        name: String,
    }
    #[derive(Deserialize)]
    struct ViewerData {
        viewer: ViewerNode,
    }

    let data: ViewerData = crate::linear::client::graphql_query(token, VIEWER_QUERY, json!({}))?;
    Ok(ViewerInfo {
        id: data.viewer.id,
        name: data.viewer.name,
    })
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
// Sync status helpers (bd-25j)
// ---------------------------------------------------------------------------

/// Build a human-readable "synced X min ago" or "syncing..." label.
fn build_sync_status_label(syncing: bool) -> String {
    if syncing {
        return "syncing...".to_string();
    }
    // Read last_synced_at from DB.
    let last = (|| -> Option<String> {
        let conn = crate::db::open_db().ok()?;
        crate::db::get_meta(&conn, "last_synced_at").ok()?
    })();

    match last {
        None => "not synced".to_string(),
        Some(ts) => {
            // Parse RFC3339 and compute elapsed minutes.
            match chrono::DateTime::parse_from_rfc3339(&ts) {
                Ok(dt) => {
                    let elapsed = chrono::Utc::now()
                        .signed_duration_since(dt.with_timezone(&chrono::Utc));
                    let mins = elapsed.num_minutes();
                    if mins < 1 {
                        "synced just now".to_string()
                    } else if mins == 1 {
                        "synced 1 min ago".to_string()
                    } else {
                        format!("synced {} min ago", mins)
                    }
                }
                Err(_) => "synced".to_string(),
            }
        }
    }
}

/// Spawn the background delta sync thread and return the receiver (bd-25j).
fn spawn_sync_thread(args: IssueArgs) -> mpsc::Receiver<SyncEvent> {
    let (tx, rx) = mpsc::channel::<SyncEvent>();
    std::thread::spawn(move || {
        // Run delta sync (falls back to full if no prior sync).
        match crate::sync::delta::run() {
            Ok(()) => {
                // Re-query SQLite for a fresh issue list to send to TUI.
                let issues = (|| -> Result<Vec<Issue>> {
                    let conn = crate::db::open_db()?;
                    let db_issues = crate::db::query_issues(&conn, &args)?;
                    // Convert db::Issue -> issues::list::Issue.
                    Ok(db_issues
                        .into_iter()
                        .map(db_issue_to_list_issue)
                        .collect())
                })();
                match issues {
                    Ok(list) => {
                        let _ = tx.send(SyncEvent::Done(list));
                    }
                    Err(e) => {
                        let _ = tx.send(SyncEvent::Error(e.to_string()));
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(SyncEvent::Error(e.to_string()));
            }
        }
    });
    rx
}

/// Convert a `crate::db::Issue` row to a `crate::issues::list::Issue` for TUI display.
fn db_issue_to_list_issue(src: crate::db::Issue) -> Issue {
    Issue {
        id: src.id,
        identifier: src.identifier,
        title: src.title,
        priority_label: src.priority_label.clone(),
        priority: priority_label_to_u8(&src.priority_label),
        state: crate::issues::list::State {
            id: String::new(),
            name: src.state_name,
        },
        assignee: src.assignee_name.map(|n| crate::issues::list::User {
            id: String::new(),
            name: n,
        }),
        team: crate::issues::list::Team {
            id: src.team_key.unwrap_or_default(),
            name: src.team_name,
        },
        created_at: src.created_at,
        updated_at: src.updated_at,
    }
}

fn priority_label_to_u8(label: &str) -> u8 {
    match label.to_lowercase().as_str() {
        "urgent" => 1,
        "high" => 2,
        "normal" | "medium" => 3,
        "low" => 4,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

pub fn run(args: IssueArgs) -> Result<()> {
    // Try to load issues from the local SQLite cache first (local-first UX).
    let cached_issues: Vec<Issue> = (|| -> Result<Vec<Issue>> {
        let conn = crate::db::open_db()?;
        let db_issues = crate::db::query_issues(&conn, &args)?;
        Ok(db_issues.into_iter().map(db_issue_to_list_issue).collect())
    })()
    .unwrap_or_default();

    let have_cache = !cached_issues.is_empty();

    // Determine whether to show "Syncing..." overlay (no cache yet).
    let (issues, syncing, initial_status) = if have_cache {
        (cached_issues, true, Status::Idle)
    } else {
        (Vec::new(), true, Status::Loading)
    };

    let sync_status_label = build_sync_status_label(syncing);

    // Spawn background sync thread.
    let sync_rx = spawn_sync_thread(args.clone());

    let app = App::new(
        issues,
        false,
        None,
        args,
        Some(sync_rx),
        syncing,
        sync_status_label,
    );

    let mut terminal = ratatui::init();
    let mut app = app;
    app.status = initial_status;
    let result = run_app(&mut terminal, app);
    ratatui::restore();
    result
}

fn run_app(terminal: &mut ratatui::DefaultTerminal, mut app: App) -> Result<()> {
    loop {
        // Poll background sync channel (bd-25j).
        poll_sync_events(&mut app);

        // Poll modal background loader channel (bd-vfi).
        app.poll_modal_events();

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
                    Mode::NewIssue => handle_new_issue_key(&mut app, key.code, key.modifiers),
                    Mode::Help => handle_help_key(&mut app, key.code),
                    Mode::List => handle_normal_key(&mut app, key.code, key.modifiers),
                }
            }
        }
    }
}

/// Non-blocking poll of the background sync channel (bd-25j).
fn poll_sync_events(app: &mut App) {
    // Take the receiver out temporarily so we can mutate app freely.
    let rx = match app.sync_rx.take() {
        Some(r) => r,
        None => return,
    };

    let mut got_event = false;
    loop {
        match rx.try_recv() {
            Ok(SyncEvent::Done(new_issues)) => {
                // Only replace list if the user is in normal list mode and not paginated.
                if matches!(app.mode, Mode::List) && app.cursor_stack.is_empty() && app.current_cursor.is_none() {
                    let prev_selected = app.table_state.selected();
                    app.issues = new_issues;
                    let n = app.issues.len();
                    let sel = prev_selected.unwrap_or(0).min(n.saturating_sub(1));
                    app.table_state.select(if n > 0 { Some(sel) } else { None });
                    if matches!(app.status, Status::Loading) {
                        app.status = Status::Idle;
                    }
                }
                app.syncing = false;
                app.sync_status_label = build_sync_status_label(false);
                got_event = true;
            }
            Ok(SyncEvent::Error(msg)) => {
                app.syncing = false;
                app.sync_status_label = format!("sync error: {}", msg);
                if matches!(app.status, Status::Loading) {
                    app.status = Status::Idle;
                }
                got_event = true;
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                app.syncing = false;
                if app.sync_status_label == "syncing..." {
                    app.sync_status_label = build_sync_status_label(false);
                }
                got_event = true;
                break;
            }
        }
    }

    // Put the receiver back if the thread may still send more messages.
    if !got_event || app.syncing {
        app.sync_rx = Some(rx);
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

// -- New-issue modal key handler (bd-l6r) ------------------------------------

fn handle_new_issue_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let shift = modifiers.contains(KeyModifiers::SHIFT);

    // Ctrl-Enter submits the form.
    if ctrl && code == KeyCode::Enter {
        app.new_issue_submit();
        return;
    }

    // Esc cancels.
    if code == KeyCode::Esc {
        app.mode = Mode::List;
        app.new_issue_modal = None;
        return;
    }

    let modal = match app.new_issue_modal.as_mut() {
        Some(m) => m,
        None => return,
    };

    match &modal.focused_field.clone() {
        // ---- Text fields ----
        NewIssueField::Title => match code {
            KeyCode::Tab => {
                modal.focused_field = modal.focused_field.next();
            }
            KeyCode::BackTab => {
                modal.focused_field = modal.focused_field.prev();
            }
            KeyCode::Backspace => {
                modal.title.pop();
            }
            KeyCode::Char(c) => {
                if !ctrl {
                    modal.title.push(c);
                }
            }
            _ => {}
        },
        NewIssueField::Description => match code {
            KeyCode::Tab => {
                // Description is last field; Tab wraps to Title.
                modal.focused_field = modal.focused_field.next();
            }
            KeyCode::BackTab => {
                modal.focused_field = modal.focused_field.prev();
            }
            KeyCode::Backspace => {
                modal.description.pop();
            }
            KeyCode::Enter => {
                modal.description.push('\n');
            }
            KeyCode::Char(c) => {
                if !ctrl {
                    modal.description.push(c);
                }
            }
            _ => {}
        },
        // ---- Picker fields ----
        field => {
            let field = field.clone();
            match code {
                KeyCode::Tab if !shift => {
                    // When leaving Team field, pre-load states and assignees in background (bd-vfi).
                    if field == NewIssueField::Team {
                        let next = modal.focused_field.next();
                        modal.focused_field = next;
                        // Release the mutable borrow before calling the method.
                        let _ = modal;
                        app.new_issue_load_states_and_assignees_bg();
                    } else {
                        modal.focused_field = modal.focused_field.next();
                    }
                }
                KeyCode::BackTab => {
                    modal.focused_field = modal.focused_field.prev();
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    let (items_len, selected) = new_issue_picker_state(modal, &field);
                    if items_len > 0 {
                        *selected = (*selected + 1).min(items_len - 1);
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    let (_items_len, selected) = new_issue_picker_state(modal, &field);
                    *selected = selected.saturating_sub(1);
                }
                // "m" shortcut: select "Me (...)" entry in Assignee picker (bd-1fz).
                KeyCode::Char('m') if field == NewIssueField::Assignee => {
                    // The "Me (name)" entry is always at index 0 when present.
                    if let Some(first) = modal.assignees.first() {
                        if first.label.starts_with("Me (") {
                            modal.assignee_selected = 0;
                        }
                    }
                }
                KeyCode::Enter => {
                    // Enter on a picker field advances to the next field.
                    if field == NewIssueField::Team {
                        let next = modal.focused_field.next();
                        modal.focused_field = next;
                        // Release the mutable borrow before calling the method.
                        let _ = modal;
                        app.new_issue_load_states_and_assignees_bg();
                    } else {
                        modal.focused_field = modal.focused_field.next();
                    }
                }
                _ => {}
            }
        }
    }
}

/// Returns a mutable reference to (item count, selected index) for a picker field.
fn new_issue_picker_state<'a>(
    modal: &'a mut NewIssueModal,
    field: &NewIssueField,
) -> (usize, &'a mut usize) {
    match field {
        NewIssueField::Team => (modal.teams.len(), &mut modal.team_selected),
        NewIssueField::Priority => (modal.priorities.len(), &mut modal.priority_selected),
        NewIssueField::State => (modal.states.len(), &mut modal.state_selected),
        NewIssueField::Assignee => (modal.assignees.len(), &mut modal.assignee_selected),
        // Text fields should not reach here.
        _ => (0, &mut modal.team_selected),
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
        // New issue modal (bd-l6r)
        KeyCode::Char('n') => app.open_new_issue_modal(),
        // Help popup (bd-5lz)
        KeyCode::Char('?') => {
            app.help_popup = Some(HelpPopup::new());
            app.mode = Mode::Help;
        }
        _ => {}
    }
}

// -- Help popup key handler (bd-5lz) -----------------------------------------

fn handle_help_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => {
            app.mode = Mode::List;
            app.help_popup = None;
        }
        KeyCode::Down => {
            if let Some(ref mut popup) = app.help_popup {
                let max = popup.filtered.len().saturating_sub(1);
                if popup.selected < max {
                    popup.selected += 1;
                }
            }
        }
        KeyCode::Up => {
            if let Some(ref mut popup) = app.help_popup {
                popup.selected = popup.selected.saturating_sub(1);
            }
        }
        KeyCode::Backspace => {
            if let Some(ref mut popup) = app.help_popup {
                popup.search.pop();
                popup.update_filter();
            }
        }
        KeyCode::Char('q') => {
            app.mode = Mode::List;
            app.help_popup = None;
        }
        KeyCode::Char('j') => {
            if let Some(ref mut popup) = app.help_popup {
                let max = popup.filtered.len().saturating_sub(1);
                if popup.selected < max {
                    popup.selected += 1;
                }
            }
        }
        KeyCode::Char('k') => {
            if let Some(ref mut popup) = app.help_popup {
                popup.selected = popup.selected.saturating_sub(1);
            }
        }
        KeyCode::Char(c) => {
            if let Some(ref mut popup) = app.help_popup {
                popup.search.push(c);
                popup.update_filter();
            }
        }
        _ => {}
    }
}
