use anyhow::{Context, Result};
use chrono::Utc;
use lt_types::comments::{Comment, CommentConnection, CommentsQuery};
use lt_types::pagination::PageInfo;
use lt_types::types::User;
use rusqlite::{Connection, params};

use crate::db::ops::{EntityKey, Read, Upsert};
use crate::db::parse_datetime_column;
use crate::db::sql::{self, EntityTable};

/// Insert or replace a slice of comments: upsert each comment's author into
/// the `users` table (relational storage, no more flattened `author_name`),
/// then the comment row, stamping `synced_at` to now (UTC). Errors if a
/// comment has no `issue_id` -- a comment reaching storage without one is a
/// bug, since `issue_comments` is keyed on it.
pub fn upsert_comments(conn: &Connection, comments: &[Comment]) -> Result<()> {
    let synced_at = Utc::now().to_rfc3339();
    let mut stmt = sql::prepare(conn, sql::UPSERT_COMMENT)
        .context("failed to prepare upsert_comments statement")?;

    for c in comments {
        let issue_id = c
            .issue_id
            .as_deref()
            .with_context(|| format!("comment {} has no issue id", c.id.inner()))?;
        if let Some(user) = &c.user {
            crate::db::issues::upsert_named_entity(
                conn,
                EntityTable::Users,
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
    let mut stmt = sql::prepare(conn, sql::QUERY_COMMENTS)
        .context("failed to prepare query_comments statement")?;

    let rows = stmt
        .query_map(params![issue_id], |row| {
            let created_at: String = row.get("created_at")?;
            let updated_at: String = row.get("updated_at")?;
            let user_id: Option<String> = row.get("user_id")?;
            let user_name: Option<String> = row.get("user_name")?;
            Ok(Comment {
                id: row.get::<_, String>("id")?.into(),
                body: row.get("body")?,
                created_at: parse_datetime_column(&created_at)?,
                updated_at: parse_datetime_column(&updated_at)?,
                user: user_id.map(|id| User {
                    id: id.into(),
                    name: user_name.unwrap_or_default(),
                }),
                issue_id: Some(issue_id.to_string()),
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
    sql::execute(
        conn,
        sql::DELETE_COMMENTS_FOR_ISSUE,
        params![issue_id],
        "delete comments for issue",
    )
}

impl Read for CommentsQuery {
    /// The local read has no next page: every synced comment for the issue is
    /// already in `issue_comments` (fetch-all semantics, ADR "Decision 3"),
    /// so the whole thread is one unpaginated connection.
    fn read(conn: &Connection, vars: &Self::Variables) -> Result<Self::Output> {
        Ok(CommentConnection {
            nodes: query_comments(conn, &vars.id)?,
            page_info: PageInfo {
                has_next_page: false,
                end_cursor: None,
            },
        })
    }

    fn reads(vars: &Self::Variables) -> Vec<EntityKey> {
        vec![EntityKey::Comment {
            issue_id: vars.id.clone(),
        }]
    }
}

impl Upsert for CommentsQuery {
    /// Replace-set semantics preserved: the issue's synced comments are
    /// cleared (un-acked `local:` rows survive, see
    /// [`delete_comments_for_issue`]) before the freshly fetched set is
    /// written.
    fn upsert(
        conn: &Connection,
        vars: &Self::Variables,
        out: &Self::Output,
    ) -> Result<Vec<EntityKey>> {
        delete_comments_for_issue(conn, &vars.id)?;
        upsert_comments(conn, &out.nodes)?;
        Ok(vec![EntityKey::Comment {
            issue_id: vars.id.clone(),
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(id: &str, issue_id: &str, created_at: &str) -> Comment {
        Comment {
            id: id.into(),
            body: format!("body {id}"),
            created_at: created_at.parse().unwrap(),
            updated_at: created_at.parse().unwrap(),
            user: Some(User {
                id: "u-alice".into(),
                name: "Alice".to_string(),
            }),
            issue_id: Some(issue_id.to_string()),
        }
    }

    fn test_db() -> Connection {
        let db = crate::db::Database::memory().unwrap();
        db.connect().unwrap()
    }

    #[test]
    fn query_returns_comments_ordered_by_created_at() {
        let conn = test_db();
        upsert_comments(
            &conn,
            &[
                comment("c2", "i1", "2026-01-02T00:00:00Z"),
                comment("c1", "i1", "2026-01-01T00:00:00Z"),
            ],
        )
        .unwrap();
        upsert_comments(&conn, &[comment("c3", "i2", "2026-01-03T00:00:00Z")]).unwrap();

        let got = query_comments(&conn, "i1").unwrap();
        assert_eq!(
            got.iter().map(|c| c.id.inner()).collect::<Vec<_>>(),
            ["c1", "c2"]
        );
        assert_eq!(got[0].body, "body c1");
        assert_eq!(got[0].author(), "Alice");
        assert_eq!(got[0].issue_id.as_deref(), Some("i1"));
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
    fn upsert_with_no_author_leaves_user_none() {
        let conn = test_db();
        let mut c = comment("c1", "i1", "2026-01-01T00:00:00Z");
        c.user = None;
        upsert_comments(&conn, &[c]).unwrap();

        let got = query_comments(&conn, "i1").unwrap();
        assert_eq!(got[0].author(), "unknown");
    }

    #[test]
    fn upsert_with_no_issue_id_errors() {
        let conn = test_db();
        let mut c = comment("c1", "i1", "2026-01-01T00:00:00Z");
        c.issue_id = None;
        assert!(upsert_comments(&conn, &[c]).is_err());
    }

    #[test]
    fn delete_removes_only_target_issue() {
        let conn = test_db();
        upsert_comments(&conn, &[comment("c1", "i1", "2026-01-01T00:00:00Z")]).unwrap();
        upsert_comments(&conn, &[comment("c2", "i2", "2026-01-02T00:00:00Z")]).unwrap();

        delete_comments_for_issue(&conn, "i1").unwrap();

        assert!(query_comments(&conn, "i1").unwrap().is_empty());
        assert_eq!(query_comments(&conn, "i2").unwrap().len(), 1);
    }

    fn vars(issue_id: &str) -> lt_types::comments::CommentsVariables {
        lt_types::comments::CommentsVariables {
            id: issue_id.to_string(),
            after: None,
        }
    }

    #[test]
    fn comments_query_read_wraps_the_thread_with_no_next_page() {
        let conn = test_db();
        upsert_comments(&conn, &[comment("c1", "i1", "2026-01-01T00:00:00Z")]).unwrap();

        let page = CommentsQuery::read(&conn, &vars("i1")).unwrap();
        assert_eq!(page.nodes.len(), 1);
        assert!(!page.page_info.has_next_page);
        assert!(page.page_info.end_cursor.is_none());
    }

    #[test]
    fn comments_query_reads_only_its_own_issue() {
        assert_eq!(
            CommentsQuery::reads(&vars("i1")),
            vec![EntityKey::Comment {
                issue_id: "i1".to_string()
            }]
        );
    }

    #[test]
    fn comments_query_upsert_replaces_the_set_and_reports_the_issue() {
        let conn = test_db();
        upsert_comments(&conn, &[comment("old", "i1", "2025-01-01T00:00:00Z")]).unwrap();
        let page = CommentConnection {
            nodes: vec![comment("c1", "i1", "2026-01-01T00:00:00Z")],
            page_info: PageInfo {
                has_next_page: false,
                end_cursor: None,
            },
        };
        let touched = CommentsQuery::upsert(&conn, &vars("i1"), &page).unwrap();
        assert_eq!(
            touched,
            vec![EntityKey::Comment {
                issue_id: "i1".to_string()
            }]
        );
        let got = query_comments(&conn, "i1").unwrap();
        assert_eq!(got.iter().map(|c| c.id.inner()).collect::<Vec<_>>(), ["c1"]);
    }
}
