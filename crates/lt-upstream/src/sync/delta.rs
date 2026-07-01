use anyhow::Result;
use lt_storage::db;
use lt_types::types::{Issue, IssuesData};
use serde_json::json;

use crate::client::{GraphqlTransport, HttpTransport, query_as};
use crate::issues::ISSUES_QUERY;

/// Fetch one page of issues updated after `since` (an RFC3339 timestamp).
fn fetch_page(
    transport: &dyn GraphqlTransport,
    since: &str,
    after: Option<&str>,
) -> Result<(Vec<Issue>, bool, Option<String>)> {
    // Request all states including completed/archived so delta picks up
    // changes to previously-completed issues.
    let filter = json!({
        "updatedAt": { "gt": since }
    });

    let sort = json!([{ "updatedAt": { "order": "Descending" } }]);

    let variables = json!({
        "filter": filter,
        "sort": sort,
        "first": 250,
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

/// Run incremental (delta) sync.
///
/// - If no `last_synced_at` is recorded, delegates to `sync full`.
/// - Otherwise fetches issues where updatedAt > `last_synced_at`, upserts them,
///   and updates `last_synced_at`.
pub fn run() -> Result<()> {
    let conn = db::open_db(db::db_path()?)?;

    let last_synced_at = db::get_meta(&conn, "last_synced_at")?;

    // No previous sync -- fall back to full sync.
    let Some(since) = last_synced_at else {
        return super::full::run();
    };

    let token = crate::auth::refresh::load_or_refresh_token()?;
    let transport = HttpTransport::new(token.access_token);

    // Drain queued local mutations first so the base reflects acked edits before
    // the delta fetch overwrites it.
    super::drain::drain(&conn, &transport)?;
    // Persist the viewer so cached reads can resolve `me` offline.
    super::persist_viewer(&conn, &transport)?;

    super::sync_pages(&conn, |after| fetch_page(&transport, &since, after))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::FakeTransport;

    #[test]
    fn fetch_page_filters_by_since_and_extracts_page_info() {
        let transport = FakeTransport::new(vec![json!({
            "issues": {
                "nodes": [crate::issues::sample_issue_node("1")],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }
        })]);
        let (issues, has_next, _end) =
            fetch_page(&transport, "2026-01-01T00:00:00Z", None).unwrap();
        assert_eq!(issues.len(), 1);
        assert!(!has_next);

        let vars = transport.variables(0);
        assert_eq!(
            vars["filter"]["updatedAt"]["gt"],
            json!("2026-01-01T00:00:00Z")
        );
        assert_eq!(vars["first"], json!(250));
    }
}
