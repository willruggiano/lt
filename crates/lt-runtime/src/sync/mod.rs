pub mod delta;
pub mod drain;
pub mod full;
pub mod probe;
pub mod service;

use anyhow::Result;
use chrono::Utc;
use lt_storage::db;
use lt_storage::db::{EntityKey, Mutation};
use lt_types::issues::{IssuesQuery, IssuesVariables};
use lt_types::states::{AllWorkflowStatesQuery, AllWorkflowStatesVariables};
use lt_types::teams::TeamsQuery;
use lt_types::viewer::ViewerQuery;
use lt_upstream::client::{GraphqlTransport, execute};

/// Persist the authenticated viewer's identity into `sync_meta` so cached reads
/// can resolve `me` without a network round-trip. Goes through the same
/// `Mutation` seam every other operation does, so its touched `Viewer` key
/// folds into the cycle's own propagation instead of being a side effect
/// nothing downstream hears about.
fn persist_viewer(
    conn: &rusqlite::Connection,
    transport: &dyn GraphqlTransport,
) -> Result<Vec<EntityKey>> {
    crate::ops::refresh::<ViewerQuery>(conn, transport, ())
}

/// Paginate the org-wide `AllWorkflowStatesQuery` to exhaustion, upserting
/// each page as it arrives via its `Mutation` impl.
fn sync_workflow_states(
    conn: &rusqlite::Connection,
    transport: &dyn GraphqlTransport,
) -> Result<Vec<EntityKey>> {
    let mut cursor: Option<String> = None;
    let mut touched: Vec<EntityKey> = Vec::new();
    loop {
        let vars = AllWorkflowStatesVariables {
            first: 250,
            after: cursor.take(),
        };
        let page = execute::<AllWorkflowStatesQuery>(transport, vars.clone())?;
        touched.extend(AllWorkflowStatesQuery::apply(conn, &vars, &page)?);

        if !page.page_info.has_next_page {
            break;
        }
        cursor = page.page_info.end_cursor;
    }

    let mut seen = std::collections::HashSet::new();
    touched.retain(|k| seen.insert(k.clone()));
    Ok(touched)
}

/// Fetch every team, then every workflow state across every team, before any
/// issue page: an issue's `state_id` must already reference a locally known
/// row by the time its row lands (sync owns workflow states; issue upserts
/// never write them).
fn sync_reference_data(
    conn: &rusqlite::Connection,
    transport: &dyn GraphqlTransport,
) -> Result<Vec<EntityKey>> {
    let mut touched = crate::ops::refresh::<TeamsQuery>(conn, transport, ())?;
    touched.extend(sync_workflow_states(conn, transport)?);
    Ok(touched)
}

/// Paginate an `IssuesQuery` refresh to exhaustion, upserting each page as it
/// arrives via [`IssuesQuery`]'s `Mutation` impl, then record the current UTC
/// time as `last_synced_at`. Returns the deduplicated union of every page's
/// touched entity keys (docs/design/operation-seam-adr.md, "Decision 5"), so
/// the caller can propagate them to live subscriptions.
///
/// `make_vars` builds one page's variables from the previous page's end
/// cursor (`None` for the first page); `full`/`delta` supply the filter.
fn sync_pages<F>(
    conn: &rusqlite::Connection,
    transport: &dyn GraphqlTransport,
    mut make_vars: F,
) -> Result<Vec<EntityKey>>
where
    F: FnMut(Option<&str>) -> IssuesVariables,
{
    let mut cursor: Option<String> = None;
    let mut touched: Vec<EntityKey> = Vec::new();
    loop {
        let vars = make_vars(cursor.as_deref());
        let page = execute::<IssuesQuery>(transport, vars.clone())?;
        touched.extend(IssuesQuery::apply(conn, &vars, &page)?);

        if !page.page_info.has_next_page {
            break;
        }
        cursor = page.page_info.end_cursor;
    }

    let now = Utc::now().to_rfc3339();
    db::set_meta(conn, "last_synced_at", &now)?;

    let mut seen = std::collections::HashSet::new();
    touched.retain(|k| seen.insert(k.clone()));
    Ok(touched)
}

#[cfg(test)]
mod tests {
    use lt_types::issues::sample_issue_node;
    use lt_upstream::client::FakeTransport;
    use serde_json::json;

    use super::*;

    fn page(
        nodes: &[serde_json::Value],
        has_next: bool,
        end_cursor: Option<&str>,
    ) -> serde_json::Value {
        json!({
            "issues": {
                "nodes": nodes,
                "pageInfo": { "hasNextPage": has_next, "endCursor": end_cursor }
            }
        })
    }

    fn plain_vars(after: Option<&str>) -> IssuesVariables {
        IssuesVariables {
            filter: None,
            sort: None,
            first: Some(250),
            after: after.map(ToOwned::to_owned),
        }
    }

    /// The workflow state `sample_issue_node`'s issues reference -- sync owns
    /// workflow states, so a page's read-model reconstruction needs it
    /// already present, mirroring the sync cycle's own ordering
    /// (`sync_reference_data` before `sync_pages`).
    fn seed_sample_issue_node_state(conn: &rusqlite::Connection) {
        db::upsert_team_state(
            conn,
            "ENG",
            &lt_types::types::WorkflowState {
                id: "s".into(),
                name: "Todo".to_string(),
                position: 1.0,
            },
        )
        .unwrap();
    }

    #[test]
    fn sync_pages_upserts_each_page_and_paginates_to_exhaustion() {
        let conn = db::Database::memory().unwrap().connect().unwrap();
        seed_sample_issue_node_state(&conn);
        let transport = FakeTransport::new(vec![
            page(&[sample_issue_node("1")], true, Some("cur")),
            page(&[sample_issue_node("2")], false, None),
        ]);

        sync_pages(&conn, &transport, plain_vars).unwrap();

        assert!(db::query_issue_by_id(&conn, "1").unwrap().is_some());
        assert!(db::query_issue_by_id(&conn, "2").unwrap().is_some());
        // The second request carried the first page's cursor.
        assert_eq!(transport.variables(1)["after"], json!("cur"));
    }

    #[test]
    fn sync_pages_returns_the_deduplicated_union_of_touched_keys() {
        let conn = db::Database::memory().unwrap().connect().unwrap();
        let transport = FakeTransport::new(vec![
            page(&[sample_issue_node("1")], true, Some("cur")),
            page(&[sample_issue_node("2")], false, None),
        ]);

        let touched = sync_pages(&conn, &transport, plain_vars).unwrap();

        assert_eq!(
            touched.iter().filter(|k| **k == EntityKey::Issue).count(),
            1
        );
    }

    #[test]
    fn sync_pages_records_last_synced_at() {
        let conn = db::Database::memory().unwrap().connect().unwrap();
        let transport = FakeTransport::new(vec![page(&[], false, None)]);

        sync_pages(&conn, &transport, plain_vars).unwrap();

        assert!(db::get_meta(&conn, "last_synced_at").unwrap().is_some());
    }

    fn states_page(
        nodes: &[serde_json::Value],
        has_next: bool,
        end_cursor: Option<&str>,
    ) -> serde_json::Value {
        json!({
            "workflowStates": {
                "nodes": nodes,
                "pageInfo": { "hasNextPage": has_next, "endCursor": end_cursor }
            }
        })
    }

    fn state_node(id: &str, name: &str, team_id: &str) -> serde_json::Value {
        json!({ "id": id, "name": name, "position": 1.0, "team": { "id": team_id } })
    }

    #[test]
    fn sync_workflow_states_paginates_to_exhaustion_and_scopes_by_team() {
        let conn = db::Database::memory().unwrap().connect().unwrap();
        let transport = FakeTransport::new(vec![
            states_page(&[state_node("s1", "Todo", "t1")], true, Some("cur")),
            states_page(&[state_node("s2", "Done", "t2")], false, None),
        ]);

        let touched = sync_workflow_states(&conn, &transport).unwrap();

        assert_eq!(
            touched,
            vec![
                EntityKey::WorkflowStates {
                    team_id: "t1".to_string()
                },
                EntityKey::WorkflowStates {
                    team_id: "t2".to_string()
                },
            ]
        );
        assert_eq!(db::query_team_states(&conn, "t1").unwrap()[0].name, "Todo");
        assert_eq!(db::query_team_states(&conn, "t2").unwrap()[0].name, "Done");
        // The second request carried the first page's cursor.
        assert_eq!(transport.variables(1)["after"], json!("cur"));
    }

    #[test]
    fn sync_reference_data_fetches_teams_then_workflow_states() {
        let conn = db::Database::memory().unwrap().connect().unwrap();
        let transport = FakeTransport::new(vec![
            json!({ "teams": { "nodes": [{ "id": "t1", "name": "Eng" }] } }),
            states_page(&[state_node("s1", "Todo", "t1")], false, None),
        ]);

        let touched = sync_reference_data(&conn, &transport).unwrap();

        assert!(touched.contains(&EntityKey::Teams));
        assert!(touched.contains(&EntityKey::WorkflowStates {
            team_id: "t1".to_string()
        }));
        assert_eq!(db::query_teams(&conn).unwrap()[0].name, "Eng");
        assert_eq!(db::query_team_states(&conn, "t1").unwrap()[0].name, "Todo");
        // Teams is the first call, workflow states the second.
        assert!(transport.calls.borrow()[0].0.contains("teams"));
        assert!(transport.calls.borrow()[1].0.contains("workflowStates"));
    }
}
