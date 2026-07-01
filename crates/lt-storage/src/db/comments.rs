use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, params};

/// A row in the `issue_comments` table.
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub id: String,
    pub issue_id: String,
    pub body: String,
    pub author_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub synced_at: String,
}

impl From<Comment> for lt_types::types::Comment {
    fn from(c: Comment) -> Self {
        Self {
            body: c.body,
            created_at: c.created_at,
            user: c
                .author_name
                .map(|name| lt_types::types::CommentUser { name }),
        }
    }
}

/// Insert or replace a slice of comments, setting `synced_at` to now (UTC).
pub fn upsert_comments(conn: &Connection, comments: &[Comment]) -> Result<()> {
    let synced_at = Utc::now().to_rfc3339();
    let mut stmt = conn
        .prepare(
            "INSERT OR REPLACE INTO issue_comments
             (id, issue_id, body, author_name, created_at, updated_at, synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .context("failed to prepare upsert_comments statement")?;

    for c in comments {
        stmt.execute(params![
            c.id,
            c.issue_id,
            c.body,
            c.author_name,
            c.created_at,
            c.updated_at,
            synced_at,
        ])
        .context("failed to upsert comment")?;
    }
    Ok(())
}

/// Return all comments for a given `issue_id`, ordered by `created_at` ascending.
pub fn query_comments(conn: &Connection, issue_id: &str) -> Result<Vec<Comment>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, issue_id, body, author_name, created_at, updated_at, synced_at
             FROM issue_comments
             WHERE issue_id = ?1
             ORDER BY created_at ASC",
        )
        .context("failed to prepare query_comments statement")?;

    let rows = stmt
        .query_map(params![issue_id], |row| {
            Ok(Comment {
                id: row.get(0)?,
                issue_id: row.get(1)?,
                body: row.get(2)?,
                author_name: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
                synced_at: row.get(6)?,
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

    fn comment(id: &str, issue_id: &str, created_at: &str) -> Comment {
        Comment {
            id: id.to_string(),
            issue_id: issue_id.to_string(),
            body: format!("body {id}"),
            author_name: Some("Alice".to_string()),
            created_at: created_at.to_string(),
            updated_at: created_at.to_string(),
            // Overwritten by upsert_comments; value here is irrelevant.
            synced_at: String::new(),
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
            &[
                comment("c2", "i1", "2026-01-02T00:00:00Z"),
                comment("c1", "i1", "2026-01-01T00:00:00Z"),
                comment("c3", "i2", "2026-01-03T00:00:00Z"),
            ],
        )
        .unwrap();

        let got = query_comments(&conn, "i1").unwrap();
        assert_eq!(
            got.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["c1", "c2"]
        );
        assert_eq!(got[0].body, "body c1");
        assert_eq!(got[0].author_name.as_deref(), Some("Alice"));
        // synced_at is stamped on insert, not carried from the input.
        assert!(!got[0].synced_at.is_empty());
    }

    #[test]
    fn query_unknown_issue_is_empty() {
        let conn = test_db();
        assert!(query_comments(&conn, "missing").unwrap().is_empty());
    }

    #[test]
    fn upsert_replaces_existing_by_id() {
        let conn = test_db();
        upsert_comments(&conn, &[comment("c1", "i1", "2026-01-01T00:00:00Z")]).unwrap();

        let mut updated = comment("c1", "i1", "2026-01-01T00:00:00Z");
        updated.body = "edited".to_string();
        upsert_comments(&conn, &[updated]).unwrap();

        let got = query_comments(&conn, "i1").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].body, "edited");
    }

    #[test]
    fn delete_removes_only_target_issue() {
        let conn = test_db();
        upsert_comments(
            &conn,
            &[
                comment("c1", "i1", "2026-01-01T00:00:00Z"),
                comment("c2", "i2", "2026-01-02T00:00:00Z"),
            ],
        )
        .unwrap();

        delete_comments_for_issue(&conn, "i1").unwrap();

        assert!(query_comments(&conn, "i1").unwrap().is_empty());
        assert_eq!(query_comments(&conn, "i2").unwrap().len(), 1);
    }
}
