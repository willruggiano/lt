//! The issue domain: the live (`--live`) list fetch behind the CLI's cache and
//! the create/replay mutations. The cached read model lives in `lt-storage`;
//! these queries are the issue paths that hit the network.

use anyhow::{Result, anyhow, bail};
use lt_storage::query::{IssueQuery, build_sort, parse_date};
use lt_types::inputs::IssueCreateInput;
use lt_types::types::{Issue, IssuesData};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::refresh::load_or_refresh_token;
use crate::client::{GraphqlTransport, HttpTransport, query_as};
use crate::graphql::{CreatePayload, post_create};

pub const ISSUES_QUERY: &str = r"
query Issues($filter: IssueFilter, $sort: [IssueSortInput!], $first: Int, $after: String) {
  issues(filter: $filter, sort: $sort, first: $first, after: $after) {
    nodes {
      id
      identifier
      title
      description
      priorityLabel
      priority
      state { id name }
      assignee { id name }
      team { id name }
      labels { nodes { id name } }
      project { id name }
      cycle { id name }
      creator { id name }
      parent { id identifier }
      createdAt
      updatedAt
    }
    pageInfo { hasNextPage endCursor }
  }
}
";

fn parse_priority(s: &str) -> Result<f64> {
    match s.to_lowercase().as_str() {
        "none" | "0" => Ok(0.0),
        "urgent" | "1" => Ok(1.0),
        "high" | "2" => Ok(2.0),
        "normal" | "medium" | "3" => Ok(3.0),
        "low" | "4" => Ok(4.0),
        _ => Err(anyhow!(
            "--priority: expected none/urgent/high/normal/medium/low or 0-4, got {s:?}"
        )),
    }
}

/// Build a GraphQL `IssueFilter` from the query spec (the `--live` path).
pub fn build_filter(args: &IssueQuery) -> Result<Option<Value>> {
    let mut filters: Vec<Value> = Vec::new();

    if let Some(team) = &args.team {
        filters.push(json!({
            "team": {
                "or": [
                    { "key": { "eqIgnoreCase": team } },
                    { "name": { "containsIgnoreCase": team } }
                ]
            }
        }));
    }

    if let Some(assignee) = &args.assignee {
        if assignee.eq_ignore_ascii_case("me") {
            filters.push(json!({
                "assignee": { "isMe": { "eq": true } }
            }));
        } else {
            filters.push(json!({
                "assignee": {
                    "or": [
                        { "name": { "containsIgnoreCase": assignee } },
                        { "email": { "containsIgnoreCase": assignee } }
                    ]
                }
            }));
        }
    } else if args.no_assignee {
        filters.push(json!({
            "assignee": { "null": true }
        }));
    }

    if let Some(state) = &args.state {
        filters.push(json!({
            "state": { "name": { "containsIgnoreCase": state } }
        }));
    }

    if let Some(priority_str) = &args.priority {
        let priority_val = parse_priority(priority_str)?;
        filters.push(json!({
            "priority": { "eq": priority_val }
        }));
    }

    if let Some(date) = &args.created_after {
        let ts = parse_date(date, "created-after")?;
        filters.push(json!({ "createdAt": { "gte": ts } }));
    }

    if let Some(date) = &args.created_before {
        let ts = parse_date(date, "created-before")?;
        filters.push(json!({ "createdAt": { "lt": ts } }));
    }

    if let Some(date) = &args.updated_after {
        let ts = parse_date(date, "updated-after")?;
        filters.push(json!({ "updatedAt": { "gte": ts } }));
    }

    if let Some(date) = &args.updated_before {
        let ts = parse_date(date, "updated-before")?;
        filters.push(json!({ "updatedAt": { "lt": ts } }));
    }

    if let Some(title) = &args.title {
        filters.push(json!({ "title": { "containsIgnoreCase": title } }));
    }

    match filters.len() {
        0 => Ok(None),
        1 => Ok(Some(filters.remove(0))),
        _ => Ok(Some(json!({ "and": filters }))),
    }
}

/// Fetch one page of issues, loading (and refreshing) the token first.
pub fn fetch(args: &IssueQuery, after: Option<&str>) -> Result<(Vec<Issue>, bool, Option<String>)> {
    let token = load_or_refresh_token()?;
    fetch_with(&HttpTransport::new(token.access_token), args, after)
}

/// Fetch one page of issues through `transport`. Split from `fetch` so the
/// request building and page-info extraction are testable with a fake transport.
pub fn fetch_with(
    transport: &dyn GraphqlTransport,
    args: &IssueQuery,
    after: Option<&str>,
) -> Result<(Vec<Issue>, bool, Option<String>)> {
    let limit = args.limit.min(250);
    let filter = build_filter(args)?;
    let sort = build_sort(&args.sort, args.desc);

    let variables = json!({
        "filter": filter,
        "sort": sort,
        "first": limit,
        "after": after,
    });

    let data: IssuesData = query_as(transport, ISSUES_QUERY, variables)?;

    let conn = data.issues;
    Ok((
        conn.nodes,
        conn.page_info.has_next_page,
        conn.page_info.end_cursor,
    ))
}

// ---------------------------------------------------------------------------
// Mutations (create synchronously; replay queued outbox commands)
// ---------------------------------------------------------------------------

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

impl CreatePayload for IssueCreateData {
    type Created = CreatedIssue;
    fn into_created(self) -> (bool, CreatedIssue) {
        (self.issue_create.success, self.issue_create.issue)
    }
}

/// Replay an `issueUpdate` from its stored variables. The drainer reconciles the
/// base itself, so only success matters here.
pub fn replay_update(transport: &dyn GraphqlTransport, variables: serde_json::Value) -> Result<()> {
    let data: IssueUpdateData = query_as(transport, ISSUE_UPDATE_MUTATION, variables)?;
    if !data.issue_update.success {
        bail!("issueUpdate returned success=false");
    }
    Ok(())
}

/// Replay an `issueCreate`, returning the server's id/identifier.
pub fn replay_create(
    transport: &dyn GraphqlTransport,
    variables: serde_json::Value,
) -> Result<CreatedIssue> {
    post_create::<IssueCreateData>(transport, ISSUE_CREATE_MUTATION, "issueCreate", variables)
}

/// Create an issue synchronously (the CLI `lt issues new` path, which is an
/// inherently online command rather than a queued TUI edit).
pub fn create(transport: &dyn GraphqlTransport, input: &IssueCreateInput) -> Result<CreatedIssue> {
    replay_create(transport, json!({ "input": input }))
}

/// A minimal GraphQL issue node matching [`Issue`]'s deserialization, shared by
/// the fetch tests here and in `sync::delta`.
#[cfg(test)]
pub(crate) fn sample_issue_node(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "identifier": format!("ENG-{id}"), "title": "t",
        "priorityLabel": "High", "priority": 2,
        "state": { "id": "s", "name": "Todo" },
        "assignee": null,
        "team": { "id": "ENG", "name": "Engineering" },
        "description": null,
        "labels": { "nodes": [] },
        "project": null, "cycle": null, "creator": null, "parent": null,
        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::FakeTransport;

    #[test]
    fn fetch_with_maps_nodes_and_sends_pagination_vars() {
        let transport = FakeTransport::new(vec![serde_json::json!({
            "issues": {
                "nodes": [sample_issue_node("1")],
                "pageInfo": { "hasNextPage": true, "endCursor": "50" }
            }
        })]);
        let args = IssueQuery::default();
        let (issues, has_next, end) = fetch_with(&transport, &args, Some("0")).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].identifier, "ENG-1");
        assert!(has_next);
        assert_eq!(end.as_deref(), Some("50"));

        let vars = transport.variables(0);
        assert_eq!(vars["first"], serde_json::json!(50));
        assert_eq!(vars["after"], serde_json::json!("0"));
    }

    #[test]
    fn create_returns_server_identity() {
        let transport = FakeTransport::new(vec![serde_json::json!({
            "issueCreate": { "success": true, "issue": {
                "id": "i1", "identifier": "ENG-1", "title": "New"
            }}
        })]);
        let created = create(
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
        assert_eq!(input["title"], serde_json::json!("New"));
        assert_eq!(input["teamId"], serde_json::json!("t1"));
        assert_eq!(input["stateId"], serde_json::json!("s1"));
        assert!(input.get("description").is_none());
        assert!(input.get("priority").is_none());
        assert!(input.get("assigneeId").is_none());
    }

    #[test]
    fn replay_update_sends_variables_and_checks_success() {
        let transport = FakeTransport::new(vec![
            serde_json::json!({ "issueUpdate": { "success": true } }),
        ]);
        replay_update(
            &transport,
            serde_json::json!({ "id": "i1", "input": { "stateId": "s9" } }),
        )
        .unwrap();
        let vars = transport.variables(0);
        assert_eq!(vars["id"], serde_json::json!("i1"));
        assert_eq!(vars["input"]["stateId"], serde_json::json!("s9"));
    }
}
