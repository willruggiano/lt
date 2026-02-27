use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, params};

/// A row in the `issue_comments` table.
#[derive(Debug, Clone)]
pub struct Comment {
    pub id: String,
    pub issue_id: String,
    pub body: String,
    pub author_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[allow(dead_code)]
    pub synced_at: String,
}

/// Insert or replace a slice of comments, setting synced_at to now (UTC).
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

/// Return all comments for a given issue_id, ordered by created_at ascending.
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

/// Delete all comments for a given issue_id (used before re-inserting a fresh set).
pub fn delete_comments_for_issue(conn: &Connection, issue_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM issue_comments WHERE issue_id = ?1",
        params![issue_id],
    )
    .context("failed to delete comments for issue")?;
    Ok(())
}
