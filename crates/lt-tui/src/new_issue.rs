use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lt_runtime::db::Connection;
use lt_runtime::sync::service::Scope;
use lt_types::types::User;

use super::{
    App, KeyFlow, PopupItem, StateCtx, StateEvent, TextInput, View, priority_popup_items,
    state_items,
};

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

    /// True while a targeted team refresh is in flight.
    pub loading: bool,
    /// Non-empty on submit validation failure. Per-fetch errors are not
    /// surfaced here (offline, every targeted refresh would fail, making the
    /// field constant noise); those go to `tracing`.
    pub error: String,
    /// The team scope the service is currently watching on this modal's
    /// behalf, distinct from `team_selected` (the live picker cursor, which
    /// can move via j/k before the user tabs away and commits it). Diffed by
    /// `new_issue_team_changed` to unwatch the old team and watch the new
    /// one in the same handler (Decision 3).
    pub watched_team_id: Option<String>,
}

impl NewIssueModal {
    /// This modal's own team-id lookup, deduplicating the call sites that
    /// need it: submit validation, the `Team{team_id}` consume guard, and
    /// `View::scopes()`.
    pub(crate) fn selected_team_id(&self) -> Option<String> {
        self.teams
            .get(self.team_selected)
            .and_then(|t| t.id.clone())
    }

    /// `Teams`: re-read the team list and re-anchor the selection by id
    /// (fallback index 0). `Team{team_id}`, guarded by `selected_team_id()`
    /// matching: re-read states/members, preserving the user's picks by item
    /// id, and clear `loading`. A team-id mismatch (a stale refresh for a
    /// team the user has since tabbed away from) falls through.
    pub(crate) fn consume(&mut self, ctx: &StateCtx, _focused: bool, ev: &StateEvent) {
        match ev {
            StateEvent::Teams => {
                let Ok(conn) = ctx.db.connect() else {
                    return;
                };
                let Ok(teams) = lt_runtime::db::query_teams(&conn) else {
                    return;
                };
                let current_id = self.selected_team_id();
                self.teams = teams.into_iter().map(team_item).collect();
                self.team_selected = reanchor(&self.teams, current_id.as_deref());
            }
            StateEvent::Team { team_id }
                if self.selected_team_id().as_deref() == Some(team_id.as_str()) =>
            {
                let Ok(conn) = ctx.db.connect() else {
                    return;
                };
                let current_state = self
                    .states
                    .get(self.state_selected)
                    .and_then(|s| s.id.clone());
                self.states = state_items(&conn, team_id);
                self.state_selected = reanchor(&self.states, current_state.as_deref());

                let current_assignee = self
                    .assignees
                    .get(self.assignee_selected)
                    .and_then(|a| a.id.clone());
                self.assignees = assignee_items(&conn, team_id);
                self.assignee_selected = reanchor(&self.assignees, current_assignee.as_deref());

                self.loading = false;
            }
            _ => {}
        }
    }
}

/// The modal's assignee picker items for `team_id`, from the local cache
/// only: `build_assignee_items` fed by the persisted `db::synced_viewer` and
/// `query_team_members`. `state_items` (the states half) is shared with the
/// state popup -- imported from `popup`, not redefined here.
fn assignee_items(conn: &Connection, team_id: &str) -> Vec<PopupItem> {
    let viewer = lt_runtime::db::synced_viewer(conn).ok().flatten();
    let members = lt_runtime::db::query_team_members(conn, team_id).unwrap_or_default();
    build_assignee_items(viewer.as_ref(), members)
}

/// `state_items`/`assignee_items` for `team_id`, opening a fresh connection.
/// Empty on a connection failure (offline-safe: the caller just sees an
/// empty picker until the background refresh lands).
fn team_scoped_items(
    db: &lt_runtime::db::Database,
    team_id: &str,
) -> (Vec<PopupItem>, Vec<PopupItem>) {
    match db.connect() {
        Ok(conn) => (state_items(&conn, team_id), assignee_items(&conn, team_id)),
        Err(_) => (Vec::new(), Vec::new()),
    }
}

fn team_item(team: lt_types::types::Team) -> PopupItem {
    PopupItem {
        label: team.name,
        id: Some(team.id.into_inner()),
    }
}

/// Find `id`'s position in `items`, falling back to index 0 when it is gone
/// (or `id` is `None`, e.g. nothing was selected yet).
fn reanchor(items: &[PopupItem], id: Option<&str>) -> usize {
    items
        .iter()
        .position(|item| item.id.as_deref() == id)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Modal lifecycle methods
// ---------------------------------------------------------------------------

impl super::App {
    pub(crate) fn open_new_issue_modal(&mut self) {
        // Pre-fill team from active filter if set.
        let preset_team = self.args.team.clone();

        let teams: Vec<PopupItem> = self
            .db
            .connect()
            .and_then(|conn| lt_runtime::db::query_teams(&conn))
            .unwrap_or_default()
            .into_iter()
            .map(team_item)
            .collect();

        let team_selected = preset_team
            .as_ref()
            .and_then(|preset| {
                teams
                    .iter()
                    .position(|t| t.label.to_lowercase().contains(&preset.to_lowercase()))
            })
            .unwrap_or(0);
        let team_id = teams.get(team_selected).and_then(|t| t.id.clone());
        let (states, assignees) = match &team_id {
            Some(id) => team_scoped_items(&self.db, id),
            None => (Vec::new(), Vec::new()),
        };

        self.push_view(View::NewIssue(NewIssueModal {
            focused_field: NewIssueField::Title,
            title: TextInput::new(),
            description: String::new(),
            teams,
            team_selected,
            priorities: priority_popup_items(),
            priority_selected: 0,
            states,
            state_selected: 0,
            assignees,
            assignee_selected: 0,
            loading: true,
            error: String::new(),
            watched_team_id: team_id,
        }));
    }

    /// Leaving the Team field (Tab/Enter): unwatch the previously-watched
    /// team and watch the newly-selected one (Decision 3), then an instant
    /// cache read for it.
    fn new_issue_team_changed(&mut self, i: usize) {
        let (old_team_id, new_team_id) = match self.views.get(i) {
            Some(View::NewIssue(modal)) => {
                (modal.watched_team_id.clone(), modal.selected_team_id())
            }
            _ => return,
        };
        if old_team_id != new_team_id {
            if let Some(old) = old_team_id {
                self.service.unwatch(Scope::Team { team_id: old });
            }
            if let Some(new) = new_team_id.clone() {
                self.service.watch(Scope::Team { team_id: new });
            }
            if let Some(View::NewIssue(modal)) = self.views.get_mut(i) {
                modal.watched_team_id.clone_from(&new_team_id);
            }
        }

        let Some(team_id) = new_team_id else {
            return;
        };

        let (states, assignees) = team_scoped_items(&self.db, &team_id);
        if let Some(View::NewIssue(modal)) = self.views.get_mut(i) {
            modal.states = states;
            modal.state_selected = 0;
            modal.assignees = assignees;
            modal.assignee_selected = 0;
            modal.loading = true;
        }
    }

    fn new_issue_submit(&mut self, i: usize) {
        let Some(View::NewIssue(modal)) = self.views.get(i) else {
            return;
        };

        if modal.title.value.trim().is_empty() {
            if let Some(View::NewIssue(m)) = self.views.get_mut(i) {
                m.error = "Title is required".to_string();
                m.focused_field = NewIssueField::Title;
            }
            return;
        }

        let Some(team_id) = modal.selected_team_id() else {
            if let Some(View::NewIssue(m)) = self.views.get_mut(i) {
                m.error = "Select a team".to_string();
            }
            return;
        };

        let input = build_issue_create_input(modal, &team_id);
        match self.service.create_issue(&input) {
            Ok(identifier) => {
                self.pop_view();
                self.footer_msg = Some("Created issue (pending sync)".to_string());
                if let Some(list) = self.base_list_mut() {
                    list.pending_select = Some(identifier);
                }
            }
            Err(e) => {
                if let Some(View::NewIssue(m)) = self.views.get_mut(i) {
                    m.error = format!("Failed to queue issue: {e}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Issue creation
// ---------------------------------------------------------------------------

/// Build the typed `issueCreate` input (ids only) from the modal's current
/// picker selections. `team_id` is the resolved (validated) team id. The
/// optimistic issue fragment used to live here too; it moved into
/// `LinearSyncService::create_issue`, which resolves display names from its
/// own lookup tables -- the fragment is a database row, not presentation.
fn build_issue_create_input(
    modal: &NewIssueModal,
    team_id: &str,
) -> lt_types::inputs::IssueCreateInput {
    let title = modal.title.value.trim().to_string();
    let description = if modal.description.trim().is_empty() {
        None
    } else {
        Some(modal.description.trim().to_string())
    };
    let state_id = modal
        .states
        .get(modal.state_selected)
        .and_then(|s| s.id.clone());
    let priority = modal
        .priorities
        .get(modal.priority_selected)
        .and_then(|p| p.id.as_ref())
        .and_then(|s| s.parse::<u8>().ok());
    let assignee_id = modal
        .assignees
        .get(modal.assignee_selected)
        .and_then(|a| a.id.clone());

    lt_types::inputs::IssueCreateInput {
        title,
        team_id: team_id.to_string(),
        description,
        state_id,
        priority: priority.map(i32::from),
        assignee_id,
    }
}

/// Build the assignee popup items: "Me (name)" at top if the viewer is known,
/// then "Unassigned", then the remaining team members (excluding the viewer).
/// `viewer` is the persisted `db::synced_viewer` -- offline-safe, absent
/// before the first successful sync.
pub(crate) fn build_assignee_items(viewer: Option<&User>, members: Vec<User>) -> Vec<PopupItem> {
    let mut items: Vec<PopupItem> = Vec::new();
    if let Some(v) = viewer {
        items.push(PopupItem {
            label: format!("Me ({})", v.name),
            id: Some(v.id.inner().to_string()),
        });
    }
    items.push(PopupItem {
        label: "Unassigned".to_string(),
        id: None,
    });
    for m in members {
        // Skip the viewer entry since it is already at the top.
        if viewer.is_some_and(|v| v.id.inner() == m.id.inner()) {
            continue;
        }
        items.push(PopupItem {
            label: m.name,
            id: Some(m.id.into_inner()),
        });
    }
    items
}

// ---------------------------------------------------------------------------
// Key handlers
// ---------------------------------------------------------------------------

pub(crate) fn handle_key(app: &mut App, i: usize, key: KeyEvent) -> KeyFlow {
    let code = key.code;
    let modifiers = key.modifiers;
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    let alt = modifiers.contains(KeyModifiers::ALT);

    // Ctrl-Enter submits the form (Alt-Enter on terminals that cannot
    // distinguish Ctrl-Enter from Enter).
    if (ctrl || alt) && code == KeyCode::Enter {
        app.new_issue_submit(i);
        return KeyFlow::Consumed;
    }

    // Esc cancels.
    if code == KeyCode::Esc {
        app.pop_view();
        return KeyFlow::Consumed;
    }

    let Some(View::NewIssue(modal)) = app.views.get_mut(i) else {
        return KeyFlow::Consumed;
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
                    // When leaving Team field, refresh states and assignees.
                    if field == NewIssueField::Team {
                        let next = modal.focused_field.next();
                        modal.focused_field = next;
                        // Release the mutable borrow before calling the method.
                        let _ = modal;
                        app.new_issue_team_changed(i);
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
                        app.new_issue_team_changed(i);
                    } else {
                        modal.focused_field = modal.focused_field.next();
                    }
                }
                _ => {}
            }
        }
    }
    KeyFlow::Consumed
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
