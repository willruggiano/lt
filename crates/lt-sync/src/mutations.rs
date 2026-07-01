use anyhow::{Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use lt_storage::sync_port::{Member, Team, WorkflowState};
use lt_types::inputs::IssueCreateInput;

use super::client::{GraphqlTransport, query_as};

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
    items: states {
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

/// The `team(id) { items: <conn> { nodes } }` envelope, shared by the workflow-
/// state and team-member queries via the `items:` field alias.
#[derive(Deserialize)]
struct TeamItems<T> {
    team: TeamItemsTeam<T>,
}

#[derive(Deserialize)]
struct TeamItemsTeam<T> {
    items: ItemsConnection<T>,
}

#[derive(Deserialize)]
struct ItemsConnection<T> {
    nodes: Vec<T>,
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

/// A `*Create` response envelope: a success flag plus the created entity.
trait CreatePayload: DeserializeOwned {
    type Created;
    fn into_created(self) -> (bool, Self::Created);
}

impl CreatePayload for IssueCreateData {
    type Created = CreatedIssue;
    fn into_created(self) -> (bool, CreatedIssue) {
        (self.issue_create.success, self.issue_create.issue)
    }
}

impl CreatePayload for CommentCreateData {
    type Created = CreatedComment;
    fn into_created(self) -> (bool, CreatedComment) {
        (self.comment_create.success, self.comment_create.comment)
    }
}

/// Replay a `*Create` mutation from its stored variables, returning the server's
/// created entity for temp-row reconciliation.
fn post_create<R: CreatePayload>(
    transport: &dyn GraphqlTransport,
    mutation: &str,
    op: &str,
    variables: serde_json::Value,
) -> Result<R::Created> {
    let (success, created) = query_as::<R>(transport, mutation, variables)?.into_created();
    if !success {
        bail!("{op} returned success=false");
    }
    Ok(created)
}

/// Replay an `issueCreate`, returning the server's id/identifier.
pub fn post_issue_create(
    transport: &dyn GraphqlTransport,
    variables: serde_json::Value,
) -> Result<CreatedIssue> {
    post_create::<IssueCreateData>(transport, ISSUE_CREATE_MUTATION, "issueCreate", variables)
}

/// Replay a `commentCreate`, returning the server's comment so the optimistic
/// temp row can be replaced.
pub fn post_comment_create(
    transport: &dyn GraphqlTransport,
    variables: serde_json::Value,
) -> Result<CreatedComment> {
    post_create::<CommentCreateData>(
        transport,
        COMMENT_CREATE_MUTATION,
        "commentCreate",
        variables,
    )
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

/// Run a `team(id) { items: <conn> { nodes } }` query and return the nodes.
/// Both team-scoped list queries alias their connection to `items`, so one
/// generic decode serves both.
fn fetch_team_items<T: DeserializeOwned>(
    transport: &dyn GraphqlTransport,
    query: &str,
    team_id: &str,
) -> Result<Vec<T>> {
    let data: TeamItems<T> = query_as(transport, query, json!({ "teamId": team_id }))?;
    Ok(data.team.items.nodes)
}

pub fn fetch_workflow_states(
    transport: &dyn GraphqlTransport,
    team_id: &str,
) -> Result<Vec<WorkflowState>> {
    fetch_team_items(transport, WORKFLOW_STATES_QUERY, team_id)
}

pub fn fetch_teams(transport: &dyn GraphqlTransport) -> Result<Vec<Team>> {
    let data: TeamsData = query_as(transport, TEAMS_QUERY, json!({}))?;
    Ok(data.teams.nodes)
}

const TEAM_MEMBERS_QUERY: &str = r"
query TeamMembers($teamId: String!) {
  team(id: $teamId) {
    items: members {
      nodes {
        id
        name
      }
    }
  }
}
";

pub fn fetch_team_members(transport: &dyn GraphqlTransport, team_id: &str) -> Result<Vec<Member>> {
    fetch_team_items(transport, TEAM_MEMBERS_QUERY, team_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::FakeTransport;

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
