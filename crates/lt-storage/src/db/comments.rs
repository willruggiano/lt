use anyhow::{Context, Result};
use chrono::Utc;
use lt_types::comments::Comment;
use lt_types::types::User;
use rusqlite::{Connection, params};

use crate::db::parse_datetime_column;

/// Insert or replace a slice of comments for `issue_id`: upsert each comment's
/// author into the `users` table (relational storage, no more flattened
/// `author_name`), then the comment row, stamping `synced_at` to now (UTC).
pub fn upsert_comments(conn: &Connection, issue_id: &str, comments: &[Comment]) -> Result<()> {
    let synced_at = Utc::now().to_rfc3339();
    let mut stmt = conn
        .prepare(
            "INSERT OR REPLACE INTO issue_comments
             (id, issue_id, body, user_id, created_at, updated_at, synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .context("failed to prepare upsert_comments statement")?;

    for c in comments {
        if let Some(user) = &c.user {
            crate::db::issues::upsert_named_entity(
                conn,
                "users",
                user.id.inner(),
                Some(&user.name),
            )?;
        }
        stmt.execute(params![
            c.id.inner(),
            issue_id,
            c.body,
            c.user.as_ref().map(|u| u.id.inner()),
            c.created_at.to_rfc3339_millis(),
            c.updated_at.to_rfc3339_millis(),
            synced_at,
        ])
        .context("failed to upsert comment")?;
    }
    Ok(())
}

/// Return all comments for a given `issue_id`, ordered by `created_at`
/// ascending, reconstructing each comment's author via a `LEFT JOIN` against
/// `users`.
pub fn query_comments(conn: &Connection, issue_id: &str) -> Result<Vec<Comment>> {
    let mut stmt = conn
        .prepare(
            "SELECT ic.id, ic.body, ic.created_at, ic.updated_at, ic.user_id, u.name
             FROM issue_comments ic
             LEFT JOIN users u ON u.id = ic.user_id
             WHERE ic.issue_id = ?1
             ORDER BY ic.created_at ASC",
        )
        .context("failed to prepare query_comments statement")?;

    let rows = stmt
        .query_map(params![issue_id], |row| {
            let created_at: String = row.get(2)?;
            let updated_at: String = row.get(3)?;
            let user_id: Option<String> = row.get(4)?;
            let user_name: Option<String> = row.get(5)?;
            Ok(Comment {
                id: lt_types::Id::new(row.get::<_, String>(0)?),
                body: row.get(1)?,
                created_at: parse_datetime_column(&created_at)?,
                updated_at: parse_datetime_column(&updated_at)?,
                user: user_id.map(|id| User {
                    id: lt_types::Id::new(id),
                    name: user_name.unwrap_or_default(),
                }),
            })
        })
        .context("failed to execute query_comments")?;

    let mut comments = Vec::new();
    for row in rows {
        comments.push(row.context("failed to read comment row")?);
    }
    Ok(comments)
}

/// Delete the synced comments for an `issue_id` before re-inserting a fresh set.
/// Optimistic `local:` rows (un-acked comment creates) are preserved so a sync
/// does not wipe a comment the drainer has not posted yet.
pub fn delete_comments_for_issue(conn: &Connection, issue_id: &str) -> Result<()> {
    crate::db::execute(
        conn,
        "DELETE FROM issue_comments WHERE issue_id = ?1 AND id NOT LIKE 'local:%'",
        params![issue_id],
        "delete comments for issue",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(id: &str, created_at: &str) -> Comment {
        Comment {
            id: lt_types::Id::new(id),
            body: format!("body {id}"),
            created_at: created_at.parse().unwrap(),
            updated_at: created_at.parse().unwrap(),
            user: Some(User {
                id: lt_types::Id::new("u-alice"),
                name: "Alice".to_string(),
            }),
        }
    }

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        conn
    }

    #[test]
    fn query_returns_comments_ordered_by_created_at() {
        let conn = test_db();
        upsert_comments(
            &conn,
            "i1",
            &[
                comment("c2", "2026-01-02T00:00:00Z"),
                comment("c1", "2026-01-01T00:00:00Z"),
            ],
        )
        .unwrap();
        upsert_comments(&conn, "i2", &[comment("c3", "2026-01-03T00:00:00Z")]).unwrap();

        let got = query_comments(&conn, "i1").unwrap();
        assert_eq!(
            got.iter().map(|c| c.id.inner()).collect::<Vec<_>>(),
            ["c1", "c2"]
        );
        assert_eq!(got[0].body, "body c1");
        assert_eq!(got[0].author(), "Alice");
    }

    #[test]
    fn query_unknown_issue_is_empty() {
        let conn = test_db();
        assert!(query_comments(&conn, "missing").unwrap().is_empty());
    }

    #[test]
    fn upsert_replaces_existing_by_id() {
        let conn = test_db();
        upsert_comments(&conn, "i1", &[comment("c1", "2026-01-01T00:00:00Z")]).unwrap();

        let mut updated = comment("c1", "2026-01-01T00:00:00Z");
        updated.body = "edited".to_string();
        upsert_comments(&conn, "i1", &[updated]).unwrap();

        let got = query_comments(&conn, "i1").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body, "edited");
    }

    #[test]
    fn upsert_with_no_author_leaves_user_none() {
        let conn = test_db();
        let mut c = comment("c1", "2026-01-01T00:00:00Z");
        c.user = None;
        upsert_comments(&conn, "i1", &[c]).unwrap();

        let got = query_comments(&conn, "i1").unwrap();
        assert_eq!(got[0].author(), "unknown");
    }

    #[test]
    fn delete_removes_only_target_issue() {
        let conn = test_db();
        upsert_comments(&conn, "i1", &[comment("c1", "2026-01-01T00:00:00Z")]).unwrap();
        upsert_comments(&conn, "i2", &[comment("c2", "2026-01-02T00:00:00Z")]).unwrap();

        delete_comments_for_issue(&conn, "i1").unwrap();

        assert!(query_comments(&conn, "i1").unwrap().is_empty());
        assert_eq!(query_comments(&conn, "i2").unwrap().len(), 1);
    }
}
