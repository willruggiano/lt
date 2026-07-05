//! `Read`/`Upsert` for the composed issue-detail operation
//! (`lt_types::detail::IssueDetailQuery`): joins today's
//! `query_issue_by_id` + `query_comments` + `query_children` for the read
//! side, and the issue-upsert path plus the comment replace-set for the
//! write side.

use anyhow::Result;
use lt_types::detail::{IssueDetailData, IssueDetailQuery};
use rusqlite::Connection;

use crate::db::comments::{delete_comments_for_issue, query_comments, upsert_comments};
use crate::db::issues::{issue_upsert_touched, query_children, query_issue_by_id, upsert_issues};
use crate::db::ops::{EntityKey, Read, Upsert};

impl Read for IssueDetailQuery {
    /// `None` when the id is locally absent: the current detail view opens
    /// from a listed (already-cached) issue, so absence means a stale cache
    /// after an upstream delete, not a bug to panic over.
    fn read(conn: &Connection, vars: &Self::Variables) -> Result<Self::Output> {
        let Some(issue) = query_issue_by_id(conn, &vars.id)? else {
            return Ok(None);
        };
        let comments = query_comments(conn, &vars.id)?;
        let children = query_children(conn, &vars.id)?;
        Ok(Some(IssueDetailData {
            issue,
            comments,
            children,
            // The local cache always holds the whole thread (comments append
            // to exhaustion on every refresh), so there is never a next page.
            comments_cursor: None,
        }))
    }

    fn reads(vars: &Self::Variables) -> Vec<EntityKey> {
        vec![
            EntityKey::Issue,
            EntityKey::Comment {
                issue_id: vars.id.clone(),
            },
        ]
    }
}

impl Upsert for IssueDetailQuery {
    /// The issue and its children go through the issue upsert path (so
    /// touched mirrors [`crate::db::issues::IssuesQuery`]'s impl); comments
    /// replace the set, same as the per-entity comment upsert did.
    fn upsert(
        conn: &Connection,
        vars: &Self::Variables,
        out: &Self::Output,
    ) -> Result<Vec<EntityKey>> {
        let Some(data) = out else {
            return Ok(Vec::new());
        };

        let mut nodes = Vec::with_capacity(1 + data.children.len());
        nodes.push(data.issue.clone());
        nodes.extend(data.children.iter().cloned());
        upsert_issues(conn, &nodes)?;
        let mut touched = issue_upsert_touched(&nodes);

        delete_comments_for_issue(conn, &vars.id)?;
        upsert_comments(conn, &data.comments)?;
        touched.push(EntityKey::Comment {
            issue_id: vars.id.clone(),
        });
        Ok(touched)
    }
}

#[cfg(test)]
mod tests {
    use lt_types::comments::Comment;
    use lt_types::detail::IssueDetailVariables;
    use lt_types::types;

    use super::*;
    use crate::db::outbox::sample_base_issue;

    fn test_db() -> Connection {
        let conn = crate::db::Database::memory().unwrap().connect().unwrap();
        // `sample_base_issue`'s state must already be locally known (sync
        // owns workflow states; issue upserts never write them).
        crate::db::teams::upsert_team_state(
            &conn,
            "ENG",
            &types::WorkflowState {
                id: "s-todo".into(),
                name: "Todo".to_string(),
                position: 1.0,
            },
        )
        .unwrap();
        conn
    }

    fn vars(id: &str) -> IssueDetailVariables {
        IssueDetailVariables { id: id.to_string() }
    }

    #[test]
    fn read_is_none_for_a_locally_absent_issue() {
        let conn = test_db();
        assert!(
            IssueDetailQuery::read(&conn, &vars("missing"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn read_joins_issue_comments_and_children() {
        let conn = test_db();
        let parent = sample_base_issue("1");
        let mut child = sample_base_issue("2");
        child.parent = Some(types::Parent {
            id: "1".into(),
            identifier: "ENG-1".to_string(),
        });
        upsert_issues(&conn, &[parent, child]).unwrap();
        upsert_comments(
            &conn,
            &[Comment {
                id: "c1".into(),
                body: "hi".to_string(),
                created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                user: None,
                issue_id: Some("1".to_string()),
            }],
        )
        .unwrap();

        let data = IssueDetailQuery::read(&conn, &vars("1")).unwrap().unwrap();
        assert_eq!(data.issue.identifier, "ENG-1");
        assert_eq!(data.comments.len(), 1);
        assert_eq!(data.children.len(), 1);
        assert_eq!(data.children[0].identifier, "ENG-2");
    }

    #[test]
    fn reads_declares_issue_and_the_scoped_comment_key() {
        assert_eq!(
            IssueDetailQuery::reads(&vars("1")),
            vec![
                EntityKey::Issue,
                EntityKey::Comment {
                    issue_id: "1".to_string()
                }
            ]
        );
    }

    #[test]
    fn upsert_of_none_is_a_noop() {
        let conn = test_db();
        assert!(
            IssueDetailQuery::upsert(&conn, &vars("1"), &None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn upsert_writes_issue_children_and_comments_and_reports_touched() {
        let conn = test_db();
        let data = IssueDetailData {
            issue: sample_base_issue("1"),
            comments: vec![Comment {
                id: "c1".into(),
                body: "hi".to_string(),
                created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                user: None,
                issue_id: Some("1".to_string()),
            }],
            children: vec![sample_base_issue("2")],
            comments_cursor: None,
        };
        let touched = IssueDetailQuery::upsert(&conn, &vars("1"), &Some(data)).unwrap();
        assert!(touched.contains(&EntityKey::Issue));
        assert!(touched.contains(&EntityKey::Comment {
            issue_id: "1".to_string()
        }));
        assert!(query_issue_by_id(&conn, "1").unwrap().is_some());
        assert!(query_issue_by_id(&conn, "2").unwrap().is_some());
        assert_eq!(query_comments(&conn, "1").unwrap().len(), 1);
    }
}
