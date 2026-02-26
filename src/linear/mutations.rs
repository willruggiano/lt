use anyhow::Result;
use serde::Deserialize;
use serde_json::json;

use super::client::graphql_query;

const ISSUE_UPDATE_MUTATION: &str = r#"
mutation IssueUpdate($id: String!, $input: IssueUpdateInput!) {
  issueUpdate(id: $id, input: $input) {
    success
    issue {
      id
      identifier
      title
      state { name }
      priority
      assignee { name }
    }
  }
}
"#;

const ISSUE_CREATE_MUTATION: &str = r#"
mutation IssueCreate($input: IssueCreateInput!) {
  issueCreate(input: $input) {
    success
    issue {
      id
      identifier
      title
      state { name }
      priority
      team { name }
    }
  }
}
"#;

const TEAMS_QUERY: &str = r#"
query Teams {
  teams {
    nodes {
      id
      name
    }
  }
}
"#;

const WORKFLOW_STATES_QUERY: &str = r#"
query WorkflowStates($teamId: String!) {
  workflowStates(filter: { team: { id: { eq: $teamId } } }) {
    nodes {
      id
      name
      type
    }
  }
}
"#;

#[derive(Deserialize, Debug, Clone)]
pub struct IssueState {
    pub name: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct IssueUser {
    pub name: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct IssueTeam {
    pub name: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Issue {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub state: IssueState,
    pub priority: u8,
    pub assignee: Option<IssueUser>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CreatedIssue {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub state: IssueState,
    pub priority: u8,
    pub team: IssueTeam,
}

#[derive(Deserialize)]
struct IssueUpdatePayload {
    issue: Issue,
}

#[derive(Deserialize)]
struct IssueUpdateData {
    #[serde(rename = "issueUpdate")]
    issue_update: IssueUpdatePayload,
}

#[derive(Deserialize)]
struct IssueCreatePayload {
    issue: CreatedIssue,
}

#[derive(Deserialize)]
struct IssueCreateData {
    #[serde(rename = "issueCreate")]
    issue_create: IssueCreatePayload,
}

#[derive(Deserialize, Debug, Clone)]
pub struct WorkflowState {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
}

#[derive(Deserialize)]
struct WorkflowStateConnection {
    nodes: Vec<WorkflowState>,
}

#[derive(Deserialize)]
struct WorkflowStatesData {
    #[serde(rename = "workflowStates")]
    workflow_states: WorkflowStateConnection,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Team {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize)]
struct TeamConnection {
    nodes: Vec<Team>,
}

#[derive(Deserialize)]
struct TeamsData {
    teams: TeamConnection,
}

pub struct CreateIssueInput {
    pub title: String,
    pub team_id: String,
    pub description: Option<String>,
    pub state_id: Option<String>,
    pub priority: Option<u8>,
    pub assignee_id: Option<String>,
}

pub fn update_issue_state(token: &str, id: &str, state_id: &str) -> Result<Issue> {
    let variables = json!({
        "id": id,
        "input": { "stateId": state_id },
    });
    let data: IssueUpdateData = graphql_query(token, ISSUE_UPDATE_MUTATION, variables)?;
    Ok(data.issue_update.issue)
}

pub fn update_issue_priority(token: &str, id: &str, priority: u8) -> Result<Issue> {
    let variables = json!({
        "id": id,
        "input": { "priority": priority },
    });
    let data: IssueUpdateData = graphql_query(token, ISSUE_UPDATE_MUTATION, variables)?;
    Ok(data.issue_update.issue)
}

pub fn update_issue_assignee(token: &str, id: &str, assignee_id: Option<String>) -> Result<Issue> {
    let input = match assignee_id {
        Some(aid) => json!({ "assigneeId": aid }),
        None => json!({ "assigneeId": serde_json::Value::Null }),
    };
    let variables = json!({
        "id": id,
        "input": input,
    });
    let data: IssueUpdateData = graphql_query(token, ISSUE_UPDATE_MUTATION, variables)?;
    Ok(data.issue_update.issue)
}

pub fn fetch_workflow_states(token: &str, team_id: &str) -> Result<Vec<WorkflowState>> {
    let variables = json!({ "teamId": team_id });
    let data: WorkflowStatesData = graphql_query(token, WORKFLOW_STATES_QUERY, variables)?;
    Ok(data.workflow_states.nodes)
}

pub fn fetch_teams(token: &str) -> Result<Vec<Team>> {
    let data: TeamsData = graphql_query(token, TEAMS_QUERY, json!({}))?;
    Ok(data.teams.nodes)
}

pub fn create_issue(token: &str, input: CreateIssueInput) -> Result<CreatedIssue> {
    let mut obj = serde_json::Map::new();
    obj.insert("title".to_string(), json!(input.title));
    obj.insert("teamId".to_string(), json!(input.team_id));
    if let Some(desc) = input.description {
        obj.insert("description".to_string(), json!(desc));
    }
    if let Some(state_id) = input.state_id {
        obj.insert("stateId".to_string(), json!(state_id));
    }
    if let Some(priority) = input.priority {
        obj.insert("priority".to_string(), json!(priority));
    }
    if let Some(assignee_id) = input.assignee_id {
        obj.insert("assigneeId".to_string(), json!(assignee_id));
    }
    let variables = json!({ "input": obj });
    let data: IssueCreateData = graphql_query(token, ISSUE_CREATE_MUTATION, variables)?;
    Ok(data.issue_create.issue)
}
