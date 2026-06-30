use std::sync::mpsc;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers};

use super::{App, Mode, PopupItem, TextInput, priority_popup_items};
use crate::linear::client::HttpTransport;
use crate::linear::viewer::fetch_viewer;

// ---------------------------------------------------------------------------
// New-issue modal state
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
            Self::Description => Self::Title,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            Self::Title | Self::Team => Self::Title,
            Self::Priority => Self::Team,
            Self::State => Self::Priority,
            Self::Assignee => Self::State,
            Self::Description => Self::Assignee,
        }
    }
}

// ---------------------------------------------------------------------------
// Events for modal background loading
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
    pub title: TextInput,
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

    /// Receiver for background-loaded modal data.
    pub modal_rx: Option<mpsc::Receiver<ModalEvent>>,
}

// ---------------------------------------------------------------------------
// Modal lifecycle methods
// ---------------------------------------------------------------------------

impl super::App {
    pub(crate) fn open_new_issue_modal(&mut self) {
        let Ok(Some(token)) = crate::config::load_token() else {
            self.footer_msg = Some("Not logged in".to_string());
            return;
        };

        // Pre-fill team from active filter if set.
        let preset_team = self.args.team.clone();

        let mut modal = NewIssueModal {
            focused_field: NewIssueField::Title,
            title: TextInput::new(),
            description: String::new(),
            teams: Vec::new(),
            team_selected: 0,
            priorities: priority_popup_items(),
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
        match crate::linear::mutations::fetch_teams(&HttpTransport::new(token.access_token)) {
            Ok(teams) => {
                modal.teams = teams
                    .into_iter()
                    .map(|t| PopupItem {
                        label: t.name.clone(),
                        id: Some(t.id),
                    })
                    .collect();
                // Pre-select team from filter.
                if let Some(ref preset) = preset_team
                    && let Some(idx) = modal
                        .teams
                        .iter()
                        .position(|t| t.label.to_lowercase().contains(&preset.to_lowercase()))
                {
                    modal.team_selected = idx;
                }
                modal.loading = false;
            }
            Err(e) => {
                modal.error = format!("Failed to fetch teams: {e}");
                modal.loading = false;
            }
        }

        self.mode = Mode::NewIssue;
        self.new_issue_modal = Some(modal);
    }

    /// Kick off background loading of states and assignees for the selected team.
    fn new_issue_load_states_and_assignees_bg(&mut self) {
        let Some(modal) = self.new_issue_modal.as_mut() else {
            return;
        };
        let Some(team_id) = modal
            .teams
            .get(modal.team_selected)
            .and_then(|t| t.id.clone())
        else {
            return;
        };

        modal.loading = true;
        modal.error.clear();

        let (tx, rx) = mpsc::channel::<ModalEvent>();
        modal.modal_rx = Some(rx);

        std::thread::spawn(move || {
            let Ok(Some(token)) = crate::config::load_token() else {
                let _ = tx.send(ModalEvent::LoadError("Not logged in".to_string()));
                return;
            };

            let transport = HttpTransport::new(token.access_token);

            // Fetch viewer for "me" shortcut.
            let viewer = fetch_viewer(&transport).ok();

            // Fetch states.
            match crate::linear::mutations::fetch_workflow_states(&transport, &team_id) {
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
                    let _ = tx.send(ModalEvent::LoadError(format!(
                        "Failed to fetch states: {e}"
                    )));
                    return;
                }
            }

            // Fetch assignees.
            match fetch_team_members(&transport, &team_id) {
                Ok(members) => {
                    let items = build_assignee_items(viewer.as_ref(), members);
                    let _ = tx.send(ModalEvent::AssigneesLoaded(items));
                }
                Err(e) => {
                    let _ = tx.send(ModalEvent::LoadError(format!(
                        "Failed to fetch assignees: {e}"
                    )));
                }
            }
        });
    }

    fn new_issue_submit(&mut self) {
        let Some(modal) = self.new_issue_modal.as_ref() else {
            return;
        };

        if modal.title.value.trim().is_empty() {
            if let Some(m) = self.new_issue_modal.as_mut() {
                m.error = "Title is required".to_string();
                m.focused_field = NewIssueField::Title;
            }
            return;
        }

        let Some(team_id) = modal
            .teams
            .get(modal.team_selected)
            .and_then(|t| t.id.clone())
        else {
            if let Some(m) = self.new_issue_modal.as_mut() {
                m.error = "Select a team".to_string();
            }
            return;
        };

        // Offline create: write an optimistic temp row and queue the command.
        // The sync drainer posts it and reconciles the temp id with the server.
        let (input, optimistic) = build_create_request(modal, team_id);
        let result = crate::db::db_path()
            .and_then(crate::db::open_db)
            .and_then(|conn| crate::db::outbox::enqueue_issue_create(&conn, &optimistic, &input));

        match result {
            Ok(()) => {
                let identifier = optimistic.identifier.clone();
                self.mode = Mode::List;
                self.new_issue_modal = None;
                self.footer_msg = Some("Created issue (pending sync)".to_string());
                self.do_fetch_and_select(Some(identifier));
            }
            Err(e) => {
                if let Some(m) = self.new_issue_modal.as_mut() {
                    m.error = format!("Failed to queue issue: {e}");
                }
            }
        }
    }

    /// Poll modal background channel and update modal state.
    pub(crate) fn poll_modal_events(&mut self) {
        // Collect events before mutating -- avoids borrow issues.
        let events: Vec<ModalEvent> = {
            let Some(modal) = self.new_issue_modal.as_ref() else {
                return;
            };
            let Some(rx) = modal.modal_rx.as_ref() else {
                return;
            };
            let mut evts = Vec::new();
            while let Ok(ev) = rx.try_recv() {
                evts.push(ev);
            }
            evts
        };

        for ev in events {
            let Some(modal) = self.new_issue_modal.as_mut() else {
                break;
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
// Issue creation + team member fetch
// ---------------------------------------------------------------------------

pub(crate) struct Member {
    pub id: String,
    pub name: String,
}

/// Build the typed `issueCreate` input and the optimistic issue fragment from
/// the modal's current selections. The optimistic fragment carries a `local:`
/// temp id and a `NEW` placeholder identifier; the drainer rewrites both with
/// the server's values on ack. `team_id` is the resolved (validated) team id.
fn build_create_request(
    modal: &NewIssueModal,
    team_id: String,
) -> (
    crate::linear::inputs::IssueCreateInput,
    crate::linear::types::Issue,
) {
    use crate::linear::types;

    let title = modal.title.value.trim().to_string();
    let description = if modal.description.trim().is_empty() {
        None
    } else {
        Some(modal.description.trim().to_string())
    };
    let state = modal.states.get(modal.state_selected);
    let state_id = state.and_then(|s| s.id.clone());
    let state_name = state.map_or_else(|| "Backlog".to_string(), |s| s.label.clone());
    let priority = modal
        .priorities
        .get(modal.priority_selected)
        .and_then(|p| p.id.as_ref())
        .and_then(|s| s.parse::<u8>().ok());
    let assignee = modal.assignees.get(modal.assignee_selected);
    let assignee_id = assignee.and_then(|a| a.id.clone());
    let team_name = modal
        .teams
        .get(modal.team_selected)
        .map(|t| t.label.clone())
        .unwrap_or_default();

    let input = crate::linear::inputs::IssueCreateInput {
        title: title.clone(),
        team_id: team_id.clone(),
        description: description.clone(),
        state_id: state_id.clone(),
        priority: priority.map(i32::from),
        assignee_id: assignee_id.clone(),
    };

    let priority = priority.unwrap_or(0);
    let now = chrono::Utc::now().to_rfc3339();
    let optimistic = types::Issue {
        id: crate::db::outbox::temp_id(),
        identifier: "NEW".to_string(),
        title,
        priority,
        priority_label: types::priority_u8_to_label(priority).to_string(),
        // Fall back to a name-keyed id when the modal lacked one so the
        // relational join still resolves a label.
        state: types::State {
            id: state_id.unwrap_or_else(|| state_name.clone()),
            name: state_name,
        },
        assignee: assignee_id.map(|id| types::User {
            id,
            name: assignee.map(|a| a.label.clone()).unwrap_or_default(),
        }),
        team: types::Team {
            id: team_id,
            name: team_name,
        },
        description,
        labels: types::LabelConnection { nodes: Vec::new() },
        project: None,
        cycle: None,
        creator: None,
        parent: None,
        created_at: now.clone(),
        updated_at: now,
    };

    (input, optimistic)
}

/// Build the assignee popup items: "Me (name)" at top if the viewer is known,
/// then "Unassigned", then the remaining team members (excluding the viewer).
pub(crate) fn build_assignee_items(
    viewer: Option<&crate::linear::viewer::Viewer>,
    members: Vec<Member>,
) -> Vec<PopupItem> {
    let mut items: Vec<PopupItem> = Vec::new();
    if let Some(v) = viewer {
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
        if viewer.is_some_and(|v| v.id == m.id) {
            continue;
        }
        items.push(PopupItem {
            label: m.name,
            id: Some(m.id),
        });
    }
    items
}

pub(crate) fn fetch_team_members(
    transport: &dyn crate::linear::client::GraphqlTransport,
    team_id: &str,
) -> Result<Vec<Member>> {
    use serde::Deserialize;
    use serde_json::json;

    const TEAM_MEMBERS_QUERY: &str = r"
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
";

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
        crate::linear::client::query_as(transport, TEAM_MEMBERS_QUERY, variables)?;
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
// Key handlers
// ---------------------------------------------------------------------------

pub(crate) fn handle_new_issue_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    let alt = modifiers.contains(KeyModifiers::ALT);

    // Ctrl-Enter submits the form (Alt-Enter on terminals that cannot
    // distinguish Ctrl-Enter from Enter).
    if (ctrl || alt) && code == KeyCode::Enter {
        app.new_issue_submit();
        return;
    }

    // Esc cancels.
    if code == KeyCode::Esc {
        app.mode = Mode::List;
        app.new_issue_modal = None;
        return;
    }

    let Some(modal) = app.new_issue_modal.as_mut() else {
        return;
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
            _ => {
                modal.title.handle_key(code, modifiers);
            }
        },
        NewIssueField::Description => handle_description_key(modal, code, ctrl),
        // ---- Picker fields ----
        field => {
            let field = field.clone();
            match code {
                KeyCode::Tab if !shift => {
                    // When leaving Team field, pre-load states and assignees in background.
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
                // "m" shortcut: select "Me (...)" entry in Assignee picker.
                KeyCode::Char('m') if field == NewIssueField::Assignee => {
                    // The "Me (name)" entry is always at index 0 when present.
                    if let Some(first) = modal.assignees.first()
                        && first.label.starts_with("Me (")
                    {
                        modal.assignee_selected = 0;
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

/// Handle a key press while the new-issue Description field is focused.
fn handle_description_key(modal: &mut NewIssueModal, code: KeyCode, ctrl: bool) {
    match code {
        KeyCode::Tab => {
            // Description is last field; Tab wraps to Title.
            modal.focused_field = modal.focused_field.next();
        }
        KeyCode::BackTab => {
            modal.focused_field = modal.focused_field.prev();
        }
        KeyCode::Enter => {
            modal.description.push('\n');
        }
        // Vim word/line deletion for the description field (cursor always at end).
        KeyCode::Backspace => {
            modal.description.pop();
        }
        KeyCode::Char('h') if ctrl => {
            modal.description.pop();
        }
        KeyCode::Char('w') if ctrl => {
            let trimmed = modal
                .description
                .trim_end_matches(|c: char| !c.is_whitespace());
            let new_end = trimmed.trim_end().len();
            modal.description.truncate(new_end);
        }
        KeyCode::Char('u') if ctrl => {
            modal.description.clear();
        }
        KeyCode::Char('k') if ctrl => {
            // cursor is at end, so ctrl+k is a no-op here
        }
        KeyCode::Char(c) if !ctrl => {
            modal.description.push(c);
        }
        _ => {}
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
