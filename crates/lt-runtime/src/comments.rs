//! Persistence of an issue's comment thread: fetch from the API edge, then
//! replace the local `issue_comments` rows for that issue.

use anyhow::Result;
use lt_storage::db;
use lt_types::comments::CommentNode;
use lt_upstream::client::GraphqlTransport;
use lt_upstream::comments::fetch_all;

/// Render a wire timestamp back to RFC3339 text for storage, preserving
/// millisecond precision and the `Z` suffix so text ordering matches
/// chronological ordering against existing rows.
fn format_datetime(dt: &lt_types::scalars::DateTime) -> String {
    dt.0.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn api_to_db(c: &CommentNode, issue_id: &str) -> db::Comment {
    db::Comment {
        id: c.id.inner().to_string(),
        issue_id: issue_id.to_string(),
        body: c.body.clone(),
        author_name: c.user.as_ref().map(|u| u.name.clone()),
        created_at: format_datetime(&c.created_at),
        updated_at: format_datetime(&c.updated_at),
        synced_at: String::new(), // filled by upsert_comments
    }
}

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
    let api_comments = fetch_all(transport, issue_id)?;
    let rows: Vec<db::Comment> = api_comments
        .iter()
        .map(|c| api_to_db(c, issue_id))
        .collect();

    // Replace the existing comments for this issue with the fresh set.
    db::delete_comments_for_issue(conn, issue_id)?;
    db::upsert_comments(conn, &rows)?;
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
                    "user": { "name": "Alice" }
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
            &[db::Comment {
                id: "old".to_string(),
                issue_id: "i1".to_string(),
                body: "stale".to_string(),
                author_name: None,
                created_at: "2025-01-01T00:00:00Z".to_string(),
                updated_at: "2025-01-01T00:00:00Z".to_string(),
                synced_at: String::new(),
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
            rows.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
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

    #[test]
    fn api_to_db_maps_fields_and_author() {
        let api: CommentNode = serde_json::from_value(json!({
            "id": "c1",
            "body": "looks good",
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-02T00:00:00Z",
            "user": { "name": "Alice" }
        }))
        .unwrap();
        let row = api_to_db(&api, "issue-9");
        assert_eq!(row.id, "c1");
        assert_eq!(row.issue_id, "issue-9");
        assert_eq!(row.body, "looks good");
        assert_eq!(row.author_name.as_deref(), Some("Alice"));
        assert_eq!(row.created_at, "2026-01-01T00:00:00.000Z");
        assert_eq!(row.updated_at, "2026-01-02T00:00:00.000Z");
        // synced_at is stamped later by upsert_comments.
        assert!(row.synced_at.is_empty());
    }

    #[test]
    fn api_to_db_handles_missing_author() {
        let api: CommentNode = serde_json::from_value(json!({
            "id": "c2",
            "body": "system note",
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
            "user": null
        }))
        .unwrap();
        assert!(api_to_db(&api, "issue-9").author_name.is_none());
    }
}
