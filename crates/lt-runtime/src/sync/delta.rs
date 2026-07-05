use anyhow::Result;
use lt_storage::db;
use lt_storage::db::EntityKey;
use lt_types::issues::{IssueFilter, IssueSort, IssuesVariables};
use lt_types::query::SortField;
use lt_upstream::client::GraphqlTransport;

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
            desc: true,
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
///
/// Returns the union of entity keys the sync touched
/// (docs/design/operation-seam-adr.md, "Decision 5").
pub fn run(
    conn: &rusqlite::Connection,
    transport: &dyn GraphqlTransport,
) -> Result<Vec<EntityKey>> {
    let last_synced_at = db::get_meta(conn, "last_synced_at")?;

    // No previous sync -- fall back to full sync.
    let Some(since) = last_synced_at else {
        return super::full::run(conn, transport);
    };

    // Drain queued local mutations first so the base reflects acked edits before
    // the delta fetch overwrites it.
    super::drain::drain(conn, transport)?;
    // Persist the viewer so cached reads can resolve `me` offline.
    let mut touched = super::persist_viewer(conn, transport)?;

    touched.extend(super::sync_pages(conn, transport, |after| {
        variables(&since, after)
    })?);
    Ok(touched)
}

#[cfg(test)]
mod tests {
    use lt_types::issues::sample_issue_node;
    use lt_upstream::client::FakeTransport;
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
        let conn = lt_storage::db::Database::memory()
            .unwrap()
            .connect()
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
