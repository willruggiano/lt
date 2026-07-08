use anyhow::Result;
use lt_storage::db;
use lt_upstream::query::issues::{IssueFilter, IssueSort, IssuesVariables};
use lt_upstream::query::{SortDirection, SortField};
use lt_upstream::transport::Transport;

/// The variables for one page of the delta fetch: issues updated on or after
/// `since` (an RFC3339 timestamp). Request all states including
/// completed/archived so delta picks up changes to previously-completed
/// issues.
fn variables(since: &str, after: Option<&str>) -> IssuesVariables {
    IssuesVariables {
        filter: Some(IssueFilter {
            updated_after: Some(since.to_string()),
            ..IssueFilter::default()
        }),
        sort: Some(IssueSort {
            field: SortField::Updated,
            direction: SortDirection::Descending,
        }),
        first: Some(250),
        after: after.map(ToOwned::to_owned),
    }
}

/// Run incremental (delta) sync over `conn`, using `transport` for every
/// request.
///
/// - If no `last_synced_at` is recorded, delegates to `sync full`.
/// - Otherwise fetches issues where updatedAt > `last_synced_at`, upserts them,
///   and updates `last_synced_at`.
pub fn run(conn: &rusqlite::Connection, transport: &dyn Transport) -> Result<()> {
    let last_synced_at = db::get_meta(conn, "last_synced_at")?;

    // No previous sync -- fall back to full sync.
    let Some(since) = last_synced_at else {
        return super::full::run(conn, transport);
    };

    // Drain queued local mutations first so the base reflects acked edits before
    // the delta fetch overwrites it.
    super::drain::drain(conn, transport)?;
    // Persist the viewer so cached reads can resolve `me` offline.
    super::persist_viewer(conn, transport)?;
    // Teams, then every workflow state across every team, before any issue
    // page: an issue's `state_id` must already be locally known.
    super::sync_reference_data(conn, transport)?;

    super::sync_pages(conn, transport, |after| variables(&since, after))
}

#[cfg(test)]
mod tests {
    use lt_storage::db::{Memory, Storage};
    use lt_upstream::query::issues::sample_issue_node;
    use lt_upstream::transport::FakeTransport;
    use serde_json::json;

    use super::*;

    #[test]
    fn variables_apply_the_since_filter_and_max_page_size() {
        let vars = variables("2026-01-01T00:00:00Z", None);
        assert_eq!(
            vars.filter.as_ref().unwrap().updated_after.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert_eq!(vars.first, Some(250));
        assert!(vars.after.is_none());
    }

    #[test]
    fn variables_carry_the_cursor_forward() {
        let vars = variables("2026-01-01T00:00:00Z", Some("cur"));
        assert_eq!(vars.after.as_deref(), Some("cur"));
    }

    #[test]
    fn sync_pages_sends_the_since_filter_on_the_wire() {
        let conn = Memory::new().unwrap().connect().unwrap();
        // `sample_issue_node`'s state must already be locally known (sync
        // owns workflow states; issue upserts never write them).
        db::upsert_team_state(
            &conn,
            "ENG",
            &lt_upstream::query::types::WorkflowState {
                id: "s".into(),
                name: "Todo".to_string(),
                position: 1.0,
            },
        )
        .unwrap();
        let transport = FakeTransport::new(vec![json!({
            "issues": {
                "nodes": [sample_issue_node("1")],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }
        })]);

        super::super::sync_pages(&conn, &transport, |after| {
            variables("2026-01-01T00:00:00Z", after)
        })
        .unwrap();

        let sent = transport.variables(0);
        assert_eq!(
            sent["filter"]["updatedAt"]["gte"],
            json!("2026-01-01T00:00:00Z")
        );
        assert_eq!(sent["first"], json!(250));
    }
}
