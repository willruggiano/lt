//! The issue domain: the live (`--live`) list fetch behind the CLI's cache.
//! The cached read model lives in `lt-storage`; this query is the issue path
//! that hits the network. Create/update/replay mutations execute
//! `lt-types::issues` mutation types directly via `execute`; this module owns
//! only the shared issue node fixture their tests reuse.

use anyhow::Result;
use lt_types::issues::{
    IssueConnection, IssueFilterValue, IssueSortValue, IssuesQuery, IssuesVariables,
};
use lt_types::query::{IssueQuery, build_sort, parse_date};
use lt_types::scalars::Priority;
use serde_json::{Value, json};

use crate::auth::refresh::load_or_refresh_token;
use crate::client::{GraphqlTransport, HttpTransport, execute};

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
        let p: Priority = priority_str.parse()?;
        filters.push(json!({
            "priority": { "eq": f64::from(p.0) }
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
pub fn fetch(args: &IssueQuery, after: Option<&str>) -> Result<IssueConnection> {
    let token = load_or_refresh_token()?;
    fetch_with(&HttpTransport::new(token.access_token), args, after)
}

/// Fetch one page of issues through `transport`. Split from `fetch` so the
/// request building and page-info extraction are testable with a fake transport.
pub fn fetch_with(
    transport: &dyn GraphqlTransport,
    args: &IssueQuery,
    after: Option<&str>,
) -> Result<IssueConnection> {
    let limit = args.limit.min(250);
    let filter = build_filter(args)?;
    let sort = build_sort(&args.sort, args.desc);

    let variables = IssuesVariables {
        filter: filter.map(IssueFilterValue),
        sort: Some(IssueSortValue(sort)),
        first: Some(i32::try_from(limit).unwrap_or(250)),
        after: after.map(ToOwned::to_owned),
    };

    execute::<IssuesQuery>(transport, variables)
}

// ---------------------------------------------------------------------------
// Shared test fixture (issue node shape reused by the create/update/replay
// mutation tests)
// ---------------------------------------------------------------------------

/// A minimal GraphQL issue node matching [`Issue`]'s deserialization, shared by
/// the fetch tests here and the `lt-runtime` delta-sync tests (via `test-util`).
/// Defined once in `lt-types` (the fixture's data owner) and re-exported here
/// so existing call sites keep this path.
#[cfg(any(test, feature = "test-util"))]
pub use lt_types::issues::sample_issue_node;

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
        let page = fetch_with(&transport, &args, Some("0")).unwrap();
        assert_eq!(page.nodes.len(), 1);
        assert_eq!(page.nodes[0].identifier, "ENG-1");
        assert!(page.page_info.has_next_page);
        assert_eq!(page.page_info.end_cursor.as_deref(), Some("50"));

        let vars = transport.variables(0);
        assert_eq!(vars["first"], serde_json::json!(50));
        assert_eq!(vars["after"], serde_json::json!("0"));
    }

    #[test]
    fn create_returns_server_identity() {
        let transport = FakeTransport::new(vec![serde_json::json!({
            "issueCreate": { "success": true, "issue": sample_issue_node("1") }
        })]);
        let created = execute::<lt_types::issues::IssueCreateMutation>(
            &transport,
            lt_types::issues::IssueCreateVariables {
                input: lt_types::inputs::IssueCreateInput {
                    title: "New".to_string(),
                    team_id: "t1".to_string(),
                    description: None,
                    state_id: Some("s1".to_string()),
                    priority: None,
                    assignee_id: None,
                },
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
    fn create_rejects_success_false() {
        let transport = FakeTransport::new(vec![serde_json::json!({
            "issueCreate": { "success": false, "issue": null }
        })]);
        let Err(err) = execute::<lt_types::issues::IssueCreateMutation>(
            &transport,
            lt_types::issues::IssueCreateVariables {
                input: lt_types::inputs::IssueCreateInput {
                    title: "New".to_string(),
                    team_id: "t1".to_string(),
                    description: None,
                    state_id: None,
                    priority: None,
                    assignee_id: None,
                },
            },
        ) else {
            panic!("expected a success=false error");
        };
        assert!(err.to_string().contains("issueCreate"));
    }

    #[test]
    fn issue_update_replay_sends_variables_and_returns_server_issue() {
        let transport = FakeTransport::new(vec![serde_json::json!({
            "issueUpdate": { "success": true, "issue": sample_issue_node("1") }
        })]);
        let issue = execute::<lt_types::issues::IssueUpdateMutation>(
            &transport,
            lt_types::issues::IssueUpdateVariables {
                id: "i1".to_string(),
                input: lt_types::inputs::IssueUpdateInput {
                    state_id: Some("s9".to_string()),
                    ..Default::default()
                },
            },
        )
        .unwrap();
        assert_eq!(issue.unwrap().identifier, "ENG-1");
        let vars = transport.variables(0);
        assert_eq!(vars["id"], serde_json::json!("i1"));
        assert_eq!(vars["input"]["stateId"], serde_json::json!("s9"));
    }

    #[test]
    fn issue_update_replay_tolerates_absent_issue() {
        let transport = FakeTransport::new(vec![serde_json::json!({
            "issueUpdate": { "success": true, "issue": null }
        })]);
        let issue = execute::<lt_types::issues::IssueUpdateMutation>(
            &transport,
            lt_types::issues::IssueUpdateVariables {
                id: "i1".to_string(),
                input: lt_types::inputs::IssueUpdateInput {
                    state_id: Some("s9".to_string()),
                    ..Default::default()
                },
            },
        )
        .unwrap();
        assert!(issue.is_none());
    }
}
