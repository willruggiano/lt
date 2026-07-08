//! The issue domain: create/update/replay mutations execute `query::issues`
//! mutation types directly via `execute`; this module owns only the shared
//! issue node fixture their tests reuse.

// ---------------------------------------------------------------------------
// Shared test fixture (issue node shape reused by the create/update/replay
// mutation tests)
// ---------------------------------------------------------------------------

/// A minimal GraphQL issue node matching [`Issue`]'s deserialization, shared by
/// the fetch tests here and the `lt-runtime` delta-sync tests (via `test-util`).
/// Defined once in `query::issues` (the fixture's data owner) and re-exported
/// here so existing call sites keep this path.
#[cfg(any(test, feature = "test-util"))]
pub use crate::query::issues::sample_issue_node;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{FakeTransport, execute};

    #[test]
    fn create_returns_server_identity() {
        let transport = FakeTransport::new(vec![serde_json::json!({
            "issueCreate": { "success": true, "issue": sample_issue_node("1") }
        })]);
        let created = execute::<crate::query::issues::IssueCreateMutation>(
            &transport,
            crate::query::issues::IssueCreateVariables {
                input: crate::query::inputs::IssueCreateInput {
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
        let Err(err) = execute::<crate::query::issues::IssueCreateMutation>(
            &transport,
            crate::query::issues::IssueCreateVariables {
                input: crate::query::inputs::IssueCreateInput {
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
        let issue = execute::<crate::query::issues::IssueUpdateMutation>(
            &transport,
            crate::query::issues::IssueUpdateVariables {
                id: "i1".to_string(),
                input: crate::query::inputs::IssueUpdateInput {
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
        let issue = execute::<crate::query::issues::IssueUpdateMutation>(
            &transport,
            crate::query::issues::IssueUpdateVariables {
                id: "i1".to_string(),
                input: crate::query::inputs::IssueUpdateInput {
                    state_id: Some("s9".to_string()),
                    ..Default::default()
                },
            },
        )
        .unwrap();
        assert!(issue.is_none());
    }
}
