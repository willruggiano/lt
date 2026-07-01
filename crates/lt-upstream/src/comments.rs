//! The comment domain: replay of queued `commentCreate` mutations and the
//! per-issue comment sync that pulls the API's comment thread into the DB.

use anyhow::Result;
use lt_storage::db;
use lt_types::types::PageInfo;
use serde::Deserialize;
use serde_json::json;

use crate::client::{GraphqlTransport, query_as};
use crate::graphql::{CreatePayload, post_create};

// ---------------------------------------------------------------------------
// Mutation replay (driven by the outbox drainer)
// ---------------------------------------------------------------------------

const COMMENT_CREATE_MUTATION: &str = r"
mutation CommentCreate($input: CommentCreateInput!) {
  commentCreate(input: $input) {
    success
    comment {
      id
      body
      createdAt
      updatedAt
      user { name }
    }
  }
}
";

/// The created comment returned by `commentCreate`, used to replace the
/// optimistic temp row on ack.
#[derive(Deserialize, Debug, Clone)]
pub struct CreatedComment {
    pub id: String,
    pub body: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    pub user: Option<CommentAuthor>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CommentAuthor {
    pub name: String,
}

#[derive(Deserialize)]
struct CommentCreatePayload {
    success: bool,
    comment: CreatedComment,
}

#[derive(Deserialize)]
struct CommentCreateData {
    #[serde(rename = "commentCreate")]
    comment_create: CommentCreatePayload,
}

impl CreatePayload for CommentCreateData {
    type Created = CreatedComment;
    fn into_created(self) -> (bool, CreatedComment) {
        (self.comment_create.success, self.comment_create.comment)
    }
}

/// Replay a `commentCreate`, returning the server's comment so the optimistic
/// temp row can be replaced.
pub fn replay_create(
    transport: &dyn GraphqlTransport,
    variables: serde_json::Value,
) -> Result<CreatedComment> {
    post_create::<CommentCreateData>(
        transport,
        COMMENT_CREATE_MUTATION,
        "commentCreate",
        variables,
    )
}

// ---------------------------------------------------------------------------
// Comment sync (pull an issue's thread into the local DB)
// ---------------------------------------------------------------------------

const COMMENTS_QUERY: &str = r"
query IssueComments($id: String!, $after: String) {
  issue(id: $id) {
    comments(first: 100, after: $after) {
      nodes {
        id
        body
        createdAt
        updatedAt
        user { name }
      }
      pageInfo { hasNextPage endCursor }
    }
  }
}
";

#[derive(Deserialize)]
struct CommentUser {
    name: String,
}

#[derive(Deserialize)]
struct ApiComment {
    id: String,
    body: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    user: Option<CommentUser>,
}

#[derive(Deserialize)]
struct CommentConnection {
    nodes: Vec<ApiComment>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Deserialize)]
struct IssueWithComments {
    comments: CommentConnection,
}

#[derive(Deserialize)]
struct IssueCommentsData {
    issue: Option<IssueWithComments>,
}

fn api_to_db(c: &ApiComment, issue_id: &str) -> db::Comment {
    db::Comment {
        id: c.id.clone(),
        issue_id: issue_id.to_string(),
        body: c.body.clone(),
        author_name: c.user.as_ref().map(|u| u.name.clone()),
        created_at: c.created_at.clone(),
        updated_at: c.updated_at.clone(),
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
    let mut all_comments: Vec<db::Comment> = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        let variables = json!({
            "id": issue_id,
            "after": cursor,
        });

        let data: IssueCommentsData = query_as(transport, COMMENTS_QUERY, variables)?;
        // issue not found; nothing to sync
        let Some(issue) = data.issue else {
            break;
        };

        let conn_data = issue.comments;
        for c in &conn_data.nodes {
            all_comments.push(api_to_db(c, issue_id));
        }

        if !conn_data.page_info.has_next_page {
            break;
        }
        cursor = conn_data.page_info.end_cursor;
    }

    // Replace the existing comments for this issue with the fresh set.
    db::delete_comments_for_issue(conn, issue_id)?;
    db::upsert_comments(conn, &all_comments)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::FakeTransport;

    #[test]
    fn replay_create_returns_server_comment() {
        let transport = FakeTransport::new(vec![json!({
            "commentCreate": { "success": true, "comment": {
                "id": "c1", "body": "hi",
                "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
                "user": { "name": "Ada" }
            }}
        })]);
        let created = replay_create(
            &transport,
            json!({ "input": { "issueId": "i1", "body": "hi" } }),
        )
        .unwrap();
        assert_eq!(created.id, "c1");
        assert_eq!(created.user.unwrap().name, "Ada");
        assert_eq!(transport.variables(0)["input"]["issueId"], json!("i1"));
    }

    fn comment_node(id: &str, body: &str) -> serde_json::Value {
        json!({
            "id": id, "body": body,
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
            "user": { "name": "Alice" }
        })
    }

    fn comments_page(
        nodes: &[serde_json::Value],
        has_next: bool,
        end: Option<&str>,
    ) -> serde_json::Value {
        json!({ "issue": { "comments": {
            "nodes": nodes,
            "pageInfo": { "hasNextPage": has_next, "endCursor": end }
        }}})
    }

    fn test_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        db::run_migrations(&conn).unwrap();
        conn
    }

    #[test]
    fn sync_paginates_and_replaces_existing() {
        let conn = test_conn();
        // A stale comment that the sync should replace.
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

        let transport = FakeTransport::new(vec![
            comments_page(&[comment_node("c1", "first")], true, Some("cur")),
            comments_page(&[comment_node("c2", "second")], false, None),
        ]);
        sync(&conn, &transport, "i1").unwrap();

        let rows = db::query_comments(&conn, "i1").unwrap();
        assert_eq!(
            rows.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["c1", "c2"]
        );
        // Second request carries the first page's cursor.
        assert_eq!(transport.variables(1)["after"], json!("cur"));
    }

    #[test]
    fn sync_missing_issue_clears_existing() {
        let conn = test_conn();
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

        let transport = FakeTransport::new(vec![json!({ "issue": null })]);
        sync(&conn, &transport, "i1").unwrap();
        assert!(db::query_comments(&conn, "i1").unwrap().is_empty());
    }

    #[test]
    fn api_to_db_maps_fields_and_author() {
        let api: ApiComment = serde_json::from_value(json!({
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
        assert_eq!(row.created_at, "2026-01-01T00:00:00Z");
        assert_eq!(row.updated_at, "2026-01-02T00:00:00Z");
        // synced_at is stamped later by upsert_comments.
        assert!(row.synced_at.is_empty());
    }

    #[test]
    fn api_to_db_handles_missing_author() {
        let api: ApiComment = serde_json::from_value(json!({
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
