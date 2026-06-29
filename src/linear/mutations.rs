#![allow(dead_code)]

use anyhow::Result;
use serde::Deserialize;
use serde_json::json;

use super::client::{GraphqlTransport, query_as};

const ISSUE_UPDATE_MUTATION: &str = r"
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
";

const ISSUE_CREATE_MUTATION: &str = r"
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
";

const COMMENT_CREATE_MUTATION: &str = r"
mutation CommentCreate($input: CommentCreateInput!) {
  commentCreate(input: $input) {
    success
  }
}
";

const TEAMS_QUERY: &str = r"
query Teams {
  teams {
    nodes {
      id
      name
    }
  }
}
";

const WORKFLOW_STATES_QUERY: &str = r"
query WorkflowStates($teamId: String!) {
  team(id: $teamId) {
    states {
      nodes {
        id
        name
        type
      }
    }
  }
}
";

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
struct TeamWithStates {
    states: WorkflowStateConnection,
}

#[derive(Deserialize)]
struct WorkflowStatesData {
    team: TeamWithStates,
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

/// Run the `issueUpdate` mutation for `id` with the given `input` payload.
fn run_issue_update(
    transport: &dyn GraphqlTransport,
    id: &str,
    input: &serde_json::Value,
) -> Result<Issue> {
    let variables = json!({
        "id": id,
        "input": input,
    });
    let data: IssueUpdateData = query_as(transport, ISSUE_UPDATE_MUTATION, variables)?;
    Ok(data.issue_update.issue)
}

pub fn update_issue_state(
    transport: &dyn GraphqlTransport,
    id: &str,
    state_id: &str,
) -> Result<Issue> {
    run_issue_update(transport, id, &json!({ "stateId": state_id }))
}

pub fn update_issue_priority(
    transport: &dyn GraphqlTransport,
    id: &str,
    priority: u8,
) -> Result<Issue> {
    run_issue_update(transport, id, &json!({ "priority": priority }))
}

pub fn update_issue_assignee(
    transport: &dyn GraphqlTransport,
    id: &str,
    assignee_id: Option<String>,
) -> Result<Issue> {
    let assignee_id = assignee_id.map_or(serde_json::Value::Null, serde_json::Value::String);
    run_issue_update(transport, id, &json!({ "assigneeId": assignee_id }))
}

pub fn fetch_workflow_states(
    transport: &dyn GraphqlTransport,
    team_id: &str,
) -> Result<Vec<WorkflowState>> {
    let variables = json!({ "teamId": team_id });
    let data: WorkflowStatesData = query_as(transport, WORKFLOW_STATES_QUERY, variables)?;
    Ok(data.team.states.nodes)
}

pub fn fetch_teams(transport: &dyn GraphqlTransport) -> Result<Vec<Team>> {
    let data: TeamsData = query_as(transport, TEAMS_QUERY, json!({}))?;
    Ok(data.teams.nodes)
}

pub fn create_comment(transport: &dyn GraphqlTransport, issue_id: &str, body: &str) -> Result<()> {
    #[derive(Deserialize)]
    struct CommentCreatePayload {
        success: bool,
    }
    #[derive(Deserialize)]
    struct CommentCreateData {
        #[serde(rename = "commentCreate")]
        comment_create: CommentCreatePayload,
    }

    let variables = json!({
        "input": { "issueId": issue_id, "body": body },
    });
    let data: CommentCreateData = query_as(transport, COMMENT_CREATE_MUTATION, variables)?;
    if !data.comment_create.success {
        anyhow::bail!("commentCreate returned success=false");
    }
    Ok(())
}

pub fn create_issue(
    transport: &dyn GraphqlTransport,
    input: CreateIssueInput,
) -> Result<CreatedIssue> {
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
    let data: IssueCreateData = query_as(transport, ISSUE_CREATE_MUTATION, variables)?;
    Ok(data.issue_create.issue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::client::FakeTransport;

    #[test]
    fn fetch_teams_extracts_nodes() {
        let transport = FakeTransport::new(vec![json!({
            "teams": { "nodes": [{ "id": "t1", "name": "Eng" }, { "id": "t2", "name": "Design" }] }
        })]);
        let teams = fetch_teams(&transport).unwrap();
        assert_eq!(
            teams.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["Eng", "Design"]
        );
    }

    #[test]
    fn create_issue_builds_input_omitting_absent_optionals() {
        let transport = FakeTransport::new(vec![json!({
            "issueCreate": { "issue": {
                "id": "i1", "identifier": "ENG-1", "title": "New",
                "state": { "name": "Todo" }, "priority": 0, "team": { "name": "Eng" }
            }}
        })]);
        let created = create_issue(
            &transport,
            CreateIssueInput {
                title: "New".to_string(),
                team_id: "t1".to_string(),
                description: None,
                state_id: Some("s1".to_string()),
                priority: None,
                assignee_id: None,
            },
        )
        .unwrap();
        assert_eq!(created.identifier, "ENG-1");

        let input = &transport.variables(0)["input"];
        assert_eq!(input["title"], json!("New"));
        assert_eq!(input["teamId"], json!("t1"));
        assert_eq!(input["stateId"], json!("s1"));
        // Absent optionals are not serialized into the input object.
        assert!(input.get("description").is_none());
        assert!(input.get("priority").is_none());
        assert!(input.get("assigneeId").is_none());
    }

    #[test]
    fn create_comment_errors_on_unsuccessful_payload() {
        let transport = FakeTransport::new(vec![json!({ "commentCreate": { "success": false } })]);
        let err = create_comment(&transport, "i1", "hi")
            .unwrap_err()
            .to_string();
        assert!(err.contains("success=false"), "got: {err}");
    }

    #[test]
    fn update_issue_state_sends_state_id_input() {
        let transport = FakeTransport::new(vec![json!({
            "issueUpdate": { "issue": {
                "id": "i1", "identifier": "ENG-1", "title": "t",
                "state": { "name": "Done" }, "priority": 1, "assignee": null
            }}
        })]);
        let issue = update_issue_state(&transport, "i1", "s9").unwrap();
        assert_eq!(issue.state.name, "Done");
        let vars = transport.variables(0);
        assert_eq!(vars["id"], json!("i1"));
        assert_eq!(vars["input"]["stateId"], json!("s9"));
    }
}
