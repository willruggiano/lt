//! The issue domain: the live (`--live`) list fetch behind the CLI's cache.
//! The cached read model lives in `lt-storage`; this query is the issue path
//! that hits the network. Create/update/replay mutations execute
//! `lt-types::issues` mutation types directly via `execute`; this module owns
//! only the shared issue node fixture their tests reuse.

use anyhow::Result;
use lt_types::issues::{IssueConnection, IssuesQuery, IssuesVariables};

use crate::auth::refresh::load_or_refresh_token;
use crate::client::{GraphqlTransport, HttpTransport, execute};

/// Fetch one page of issues, loading (and refreshing) the token first.
pub fn fetch(vars: IssuesVariables) -> Result<IssueConnection> {
    let token = load_or_refresh_token()?;
    fetch_with(&HttpTransport::new(token.access_token), vars)
}

/// Fetch one page of issues through `transport`. Split from `fetch` so the
/// request building and page-info extraction are testable with a fake transport.
pub fn fetch_with(
    transport: &dyn GraphqlTransport,
    vars: IssuesVariables,
) -> Result<IssueConnection> {
    execute::<IssuesQuery>(transport, vars)
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
        let vars = IssuesVariables {
            filter: None,
            sort: None,
            first: Some(50),
            after: Some("0".to_string()),
        };
        let page = fetch_with(&transport, vars).unwrap();
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
