use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::json;

use super::client::{GraphqlTransport, query_as};
use super::inputs::IssueCreateInput;

const ISSUE_UPDATE_MUTATION: &str = r"
mutation IssueUpdate($id: String!, $input: IssueUpdateInput!) {
  issueUpdate(id: $id, input: $input) {
    success
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
    }
  }
}
";

const COMMENT_CREATE_MUTATION: &str = r"
mutation CommentCreate($input: CommentCreateInput!) {
  commentCreate(input: $input) {
    success
    comment {
      id
      body
      createdAt
      updatedAt
      user { name }
    }
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

/// The fields the `issueCreate` response returns: enough to confirm success and
/// reconcile the optimistic temp row with the server's id/identifier.
#[derive(Deserialize, Debug, Clone)]
pub struct CreatedIssue {
    pub id: String,
    pub identifier: String,
    pub title: String,
}

#[derive(Deserialize)]
struct SuccessPayload {
    success: bool,
}

#[derive(Deserialize)]
struct IssueUpdateData {
    #[serde(rename = "issueUpdate")]
    issue_update: SuccessPayload,
}

#[derive(Deserialize)]
struct IssueCreatePayload {
    success: bool,
    issue: CreatedIssue,
}

#[derive(Deserialize)]
struct IssueCreateData {
    #[serde(rename = "issueCreate")]
    issue_create: IssueCreatePayload,
}

/// The created comment returned by `commentCreate`, used to replace the
/// optimistic temp row on ack.
#[derive(Deserialize, Debug, Clone)]
pub struct CreatedComment {
    pub id: String,
    pub body: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    pub user: Option<CommentAuthor>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CommentAuthor {
    pub name: String,
}

#[derive(Deserialize)]
struct CommentCreatePayload {
    success: bool,
    comment: CreatedComment,
}

#[derive(Deserialize)]
struct CommentCreateData {
    #[serde(rename = "commentCreate")]
    comment_create: CommentCreatePayload,
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

// ---------------------------------------------------------------------------
// Mutation replay (driven by the outbox drainer)
// ---------------------------------------------------------------------------

/// Replay an `issueUpdate` from its stored variables. The drainer reconciles the
/// base itself, so only success matters here.
pub fn post_issue_update(
    transport: &dyn GraphqlTransport,
    variables: serde_json::Value,
) -> Result<()> {
    let data: IssueUpdateData = query_as(transport, ISSUE_UPDATE_MUTATION, variables)?;
    if !data.issue_update.success {
        bail!("issueUpdate returned success=false");
    }
    Ok(())
}

/// Replay an `issueCreate` from its stored variables, returning the server's
/// id/identifier for temp-row reconciliation.
pub fn post_issue_create(
    transport: &dyn GraphqlTransport,
    variables: serde_json::Value,
) -> Result<CreatedIssue> {
    let data: IssueCreateData = query_as(transport, ISSUE_CREATE_MUTATION, variables)?;
    if !data.issue_create.success {
        bail!("issueCreate returned success=false");
    }
    Ok(data.issue_create.issue)
}

/// Replay a `commentCreate` from its stored variables, returning the server's
/// comment so the optimistic temp row can be replaced.
pub fn post_comment_create(
    transport: &dyn GraphqlTransport,
    variables: serde_json::Value,
) -> Result<CreatedComment> {
    let data: CommentCreateData = query_as(transport, COMMENT_CREATE_MUTATION, variables)?;
    if !data.comment_create.success {
        bail!("commentCreate returned success=false");
    }
    Ok(data.comment_create.comment)
}

/// Create an issue synchronously (the CLI `lt issues new` path, which is an
/// inherently online command rather than a queued TUI edit).
pub fn create_issue(
    transport: &dyn GraphqlTransport,
    input: &IssueCreateInput,
) -> Result<CreatedIssue> {
    post_issue_create(transport, json!({ "input": input }))
}

// ---------------------------------------------------------------------------
// Modal data loads (online reads, used by the popup and new-issue modal)
// ---------------------------------------------------------------------------

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
    fn create_issue_returns_server_identity() {
        let transport = FakeTransport::new(vec![json!({
            "issueCreate": { "success": true, "issue": {
                "id": "i1", "identifier": "ENG-1", "title": "New"
            }}
        })]);
        let created = create_issue(
            &transport,
            &IssueCreateInput {
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
        assert!(input.get("description").is_none());
        assert!(input.get("priority").is_none());
        assert!(input.get("assigneeId").is_none());
    }

    #[test]
    fn post_comment_create_returns_server_comment() {
        let transport = FakeTransport::new(vec![json!({
            "commentCreate": { "success": true, "comment": {
                "id": "c1", "body": "hi",
                "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
                "user": { "name": "Ada" }
            }}
        })]);
        let created = post_comment_create(
            &transport,
            json!({ "input": { "issueId": "i1", "body": "hi" } }),
        )
        .unwrap();
        assert_eq!(created.id, "c1");
        assert_eq!(created.user.unwrap().name, "Ada");
        assert_eq!(transport.variables(0)["input"]["issueId"], json!("i1"));
    }

    #[test]
    fn post_issue_update_sends_variables_and_checks_success() {
        let transport = FakeTransport::new(vec![json!({ "issueUpdate": { "success": true } })]);
        post_issue_update(
            &transport,
            json!({ "id": "i1", "input": { "stateId": "s9" } }),
        )
        .unwrap();
        let vars = transport.variables(0);
        assert_eq!(vars["id"], json!("i1"));
        assert_eq!(vars["input"]["stateId"], json!("s9"));
    }
}
