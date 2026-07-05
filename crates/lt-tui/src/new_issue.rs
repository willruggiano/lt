use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lt_runtime::{SubId, Subscription};
use lt_types::new_issue::{NewIssueData, NewIssueQuery, NewIssueVariables};
use lt_types::types::User;

use super::{
    App, Keymap, PopupItem, ScrollMotion, TextInput, Unbound, View, keymap, priority_popup_items,
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

/// All mutable state for the new-issue modal form: one `NewIssueQuery`
/// subscription is the whole data contract
/// (docs/design/operation-seam-adr.md, "Decision 3"); a team change is a new
/// vars value, so the view drops the old subscription and subscribes anew
/// (replacing the old hand-diffed `watched_team_id` bookkeeping).
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
    sub: Subscription<NewIssueData>,
}

impl NewIssueModal {
    /// This modal's own team-id lookup.
    pub(crate) fn selected_team_id(&self) -> Option<String> {
        self.teams
            .get(self.team_selected)
            .and_then(|t| t.id.clone())
    }

    /// A matching subscription update re-reads the whole form: teams
    /// re-anchored by id, states/assignees rebuilt from whatever the
    /// current vars' team scope produced, `loading` cleared.
    pub(crate) fn apply_update(&mut self, id: SubId) {
        if self.sub.id() != id {
            return;
        }
        let Some(data) = self.sub.take() else {
            return;
        };

        let current_team = self.selected_team_id();
        self.teams = data.teams.into_iter().map(PopupItem::from).collect();
        self.team_selected = reanchor(&self.teams, current_team.as_deref());

        let current_state = self
            .states
            .get(self.state_selected)
            .and_then(|s| s.id.clone());
        self.states = data.states.into_iter().map(PopupItem::from).collect();
        self.state_selected = reanchor(&self.states, current_state.as_deref());

        let current_assignee = self
            .assignees
            .get(self.assignee_selected)
            .and_then(|a| a.id.clone());
        self.assignees = build_assignee_items(data.viewer.as_ref(), data.members);
        self.assignee_selected = reanchor(&self.assignees, current_assignee.as_deref());

        self.loading = false;
    }

    /// This modal's declared keymap, by focused field: the text fields
    /// (Title/Description) forward to their own editor; the picker fields
    /// (Team/Priority/State/Assignee) navigate.
    pub(crate) fn keymap(&self) -> &'static Keymap {
        match self.focused_field {
            NewIssueField::Title | NewIssueField::Description => &TEXT_KEYMAP,
            NewIssueField::Team
            | NewIssueField::Priority
            | NewIssueField::State
            | NewIssueField::Assignee => &PICKER_KEYMAP,
        }
    }
}

/// A bare modal fixture for tests that only need `NewIssueModal`'s shape,
/// not live updates: a throwaway team-less `NewIssueQuery` subscription over
/// `runtime`, every picker empty. Callers mutate the `pub` fields to set up
/// their scenario.
#[cfg(all(test, feature = "sim"))]
pub(crate) fn test_new_issue_modal(runtime: &lt_runtime::Runtime) -> NewIssueModal {
    let (sub, _) = runtime.subscribe::<NewIssueQuery>(NewIssueVariables::new(None));
    NewIssueModal {
        focused_field: NewIssueField::Team,
        title: TextInput::from(String::new()),
        description: String::new(),
        teams: Vec::new(),
        team_selected: 0,
        priorities: Vec::new(),
        priority_selected: 0,
        states: Vec::new(),
        state_selected: 0,
        assignees: Vec::new(),
        assignee_selected: 0,
        loading: true,
        error: String::new(),
        sub,
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
        // Pre-fill team from the base list's active filter if set.
        let preset_team = match self.base() {
            View::List(list) => list.query.team_filter(),
            _ => None,
        };

        // Team-less first: the initial vars don't know the team id until the
        // subscription's own team list resolves the preset.
        let (teamless_sub, teamless_data) = self
            .runtime
            .subscribe::<NewIssueQuery>(NewIssueVariables::new(None));
        let teams: Vec<PopupItem> = teamless_data
            .teams
            .into_iter()
            .map(PopupItem::from)
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

        // A resolved team is a new vars value: drop the team-less
        // subscription and subscribe anew with it (the same pattern a later
        // team change reuses).
        let (sub, states, assignees) = match team_id {
            Some(id) => {
                let (sub, data) = self
                    .runtime
                    .subscribe::<NewIssueQuery>(NewIssueVariables::new(Some(id)));
                let states = data.states.into_iter().map(PopupItem::from).collect();
                let assignees = build_assignee_items(data.viewer.as_ref(), data.members);
                (sub, states, assignees)
            }
            None => (teamless_sub, Vec::new(), Vec::new()),
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
            sub,
        }));
    }

    /// Leaving the Team field: drop the old subscription and subscribe anew
    /// for the newly-selected team (RAII replaces the old hand-diffed
    /// watch/unwatch).
    fn new_issue_team_changed(&mut self, i: usize) {
        let new_team_id = match self.views.get(i) {
            Some(View::NewIssue(modal)) => modal.selected_team_id(),
            _ => return,
        };
        let (sub, data) = self
            .runtime
            .subscribe::<NewIssueQuery>(NewIssueVariables::new(new_team_id));
        let states: Vec<PopupItem> = data.states.into_iter().map(PopupItem::from).collect();
        let assignees = build_assignee_items(data.viewer.as_ref(), data.members);

        if let Some(View::NewIssue(modal)) = self.views.get_mut(i) {
            modal.sub = sub;
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
        match self.runtime.create_issue(&input) {
            Ok(identifier) => {
                self.pop_view();
                self.footer_msg = Some("Created issue (pending sync)".to_string());
                if let View::List(list) = self.base_mut() {
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
/// picker selections. `team_id` is the resolved (validated) team id.
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

/// The assignee popup items: "Me (name)" first if known, then "Unassigned",
/// then the remaining team members (excluding the viewer).
pub(crate) fn build_assignee_items(
    viewer: Option<&lt_types::viewer::User>,
    members: Vec<User>,
) -> Vec<PopupItem> {
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
        items.push(m.into());
    }
    items
}

// ---------------------------------------------------------------------------
// Key handlers
// ---------------------------------------------------------------------------

/// Shared by the picker and text keymaps: the submit chord plus
/// Tab/Shift+Tab field navigation.
pub(crate) static FORM_NAV: keymap::Table = &[
    (
        keymap::Binding::Single(keymap::Key::ctrl_code(KeyCode::Enter)),
        keymap::Action::Submit,
    ),
    (
        keymap::Binding::Single(keymap::Key::alt(KeyCode::Enter)),
        keymap::Action::Submit,
    ),
    (
        keymap::Binding::Single(keymap::Key::plain(KeyCode::Tab)),
        keymap::Action::NextField,
    ),
    (
        keymap::Binding::Single(keymap::Key::shift_tab()),
        keymap::Action::PrevField,
    ),
];

/// New-issue modal, picker fields: `FORM_NAV` plus GLOBAL's navigation keys,
/// which move the focused picker's selection; `enter` advances like `Tab`.
pub(crate) static PICKER_BINDINGS: keymap::Table = &[
    (
        keymap::Binding::Single(keymap::Key::plain(KeyCode::Enter)),
        keymap::Action::Confirm,
    ),
    (
        keymap::Binding::Single(keymap::Key::char('m')),
        keymap::Action::PickMe,
    ),
];

pub(crate) static PICKER_KEYMAP: Keymap = Keymap {
    layers: &[PICKER_BINDINGS, FORM_NAV, keymap::GLOBAL],
    apply: Some(apply_new_issue),
    unbound: Unbound::Swallow,
};

/// New-issue modal, text fields (Title/Description): everything but
/// `FORM_NAV`'s rows forwards to the focused field's editor (`enter` inserts
/// a newline in Description).
pub(crate) static TEXT_KEYMAP: Keymap = Keymap {
    layers: &[FORM_NAV],
    apply: Some(apply_new_issue),
    unbound: Unbound::Forward(forward_text),
};

/// `Submit`/`NextField`/`PrevField` are shared by both keymaps;
/// `Confirm`/`PickMe` only resolve from the picker keymap.
pub(crate) fn apply_new_issue(app: &mut App, i: usize, action: keymap::Action) {
    match action {
        keymap::Action::Submit => app.new_issue_submit(i),
        keymap::Action::NextField | keymap::Action::Confirm => new_issue_advance(app, i),
        keymap::Action::PrevField => {
            if let Some(View::NewIssue(modal)) = app.views.get_mut(i) {
                modal.focused_field = modal.focused_field.prev();
            }
        }
        // "m" shortcut: select the "Me (...)" entry in the Assignee picker.
        keymap::Action::PickMe => {
            if let Some(View::NewIssue(modal)) = app.views.get_mut(i)
                && modal.focused_field == NewIssueField::Assignee
                && let Some(first) = modal.assignees.first()
                && first.label.starts_with("Me (")
            {
                modal.assignee_selected = 0;
            }
        }
        // Other keymaps' actions never resolve here; kept exhaustive over
        // `Action` regardless.
        _ => {}
    }
}

/// Advance to the next field: leaving Team swaps the team-scoped
/// subscriptions; any other field just advances.
fn new_issue_advance(app: &mut App, i: usize) {
    let Some(View::NewIssue(modal)) = app.views.get_mut(i) else {
        return;
    };
    if modal.focused_field == NewIssueField::Team {
        let next = modal.focused_field.next();
        modal.focused_field = next;
        app.new_issue_team_changed(i);
    } else {
        modal.focused_field = modal.focused_field.next();
    }
}

/// Forward an unbound key to the focused text field's own editor, using the
/// raw crossterm event rather than the normalized `Key`.
pub(crate) fn forward_text(app: &mut App, i: usize, ev: KeyEvent) {
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let Some(View::NewIssue(modal)) = app.views.get_mut(i) else {
        return;
    };
    match modal.focused_field {
        NewIssueField::Title => {
            modal.title.handle_key(ev.code, ev.modifiers);
        }
        NewIssueField::Description => handle_description_key(modal, ev.code, ctrl),
        NewIssueField::Team
        | NewIssueField::Priority
        | NewIssueField::State
        | NewIssueField::Assignee => {}
    }
}

fn handle_description_key(modal: &mut NewIssueModal, code: KeyCode, ctrl: bool) {
    match code {
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

impl NewIssueModal {
    /// Selection movement over the focused picker field's shared motion set;
    /// a text-focused field has no picker state, so this no-ops.
    pub(crate) fn scroll(&mut self, motion: ScrollMotion, viewport_height: u16) {
        let field = self.focused_field.clone();
        let (len, selected) = new_issue_picker_state(self, &field);
        motion.apply_selection(selected, len, viewport_height);
    }
}

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

#[cfg(all(test, feature = "sim"))]
mod tests {
    use super::*;

    fn item(id: &str) -> PopupItem {
        PopupItem {
            label: id.to_string(),
            id: Some(id.to_string()),
        }
    }

    #[test]
    fn reanchor_finds_the_matching_id() {
        let items = vec![item("a"), item("b"), item("c")];
        assert_eq!(reanchor(&items, Some("b")), 1);
    }

    #[test]
    fn reanchor_falls_back_to_zero_when_the_id_is_gone() {
        let items = vec![item("a"), item("b")];
        assert_eq!(reanchor(&items, Some("gone")), 0);
        assert_eq!(reanchor(&items, None), 0);
    }
}
