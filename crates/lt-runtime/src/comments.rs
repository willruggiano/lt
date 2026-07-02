//! Persistence of an issue's comment thread: fetch from the API edge, then
//! replace the local `issue_comments` rows for that issue.

use anyhow::Result;
use lt_storage::db;
use lt_upstream::client::GraphqlTransport;
use lt_upstream::comments::fetch_all;

/// Fetch all comments for `issue_id` from the Linear API and upsert them into
/// the local `issue_comments` table.
///
/// All existing comments for the issue are replaced with the freshly fetched
/// set to keep the DB consistent with Linear.
pub fn sync(
    conn: &rusqlite::Connection,
    transport: &dyn GraphqlTransport,
    issue_id: &str,
) -> Result<()> {
    let comments = fetch_all(transport, issue_id)?;

    // Replace the existing comments for this issue with the fresh set.
    db::delete_comments_for_issue(conn, issue_id)?;
    db::upsert_comments(conn, issue_id, &comments)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use lt_upstream::client::FakeTransport;
    use serde_json::json;

    use super::*;

    // A single-page comment thread for `issue_id`. Pagination itself is covered
    // by `lt_upstream::comments::fetch_all`; these tests exercise persistence.
    fn thread(ids: &[&str]) -> serde_json::Value {
        let nodes: Vec<_> = ids
            .iter()
            .map(|id| {
                json!({
                    "id": id, "body": "b",
                    "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
                    "user": { "id": "u1", "name": "Alice" }
                })
            })
            .collect();
        json!({ "issue": { "comments": {
            "nodes": nodes,
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        }}})
    }

    fn conn_with_stale() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        db::run_migrations(&conn).unwrap();
        db::upsert_comments(
            &conn,
            "i1",
            &[lt_types::comments::Comment {
                id: lt_types::Id::new("old"),
                body: "stale".to_string(),
                created_at: "2025-01-01T00:00:00Z".parse().unwrap(),
                updated_at: "2025-01-01T00:00:00Z".parse().unwrap(),
                user: None,
            }],
        )
        .unwrap();
        conn
    }

    #[test]
    fn sync_replaces_existing_with_fetched_set() {
        let conn = conn_with_stale();
        let transport = FakeTransport::new(vec![thread(&["c1", "c2"])]);
        sync(&conn, &transport, "i1").unwrap();

        let rows = db::query_comments(&conn, "i1").unwrap();
        assert_eq!(
            rows.iter().map(|c| c.id.inner()).collect::<Vec<_>>(),
            ["c1", "c2"]
        );
    }

    #[test]
    fn sync_missing_issue_returns_error() {
        // `Query.issue` is non-null in the schema; a missing issue surfaces as a
        // GraphQL error, so `sync` propagates it rather than silently clearing.
        let conn = conn_with_stale();
        let transport = FakeTransport::new(vec![json!({ "issue": null })]);
        assert!(sync(&conn, &transport, "i1").is_err());
    }
}
