//! The comment domain: replay of queued `commentCreate` mutations and the
//! per-issue comment fetch that pulls the API's comment thread. Persistence of
//! the fetched thread into the local DB lives in `lt-runtime`.

use anyhow::Result;
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
// Comment fetch (pull an issue's thread from the API)
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

/// A single comment as returned by the API, before local persistence.
#[derive(Deserialize)]
pub struct ApiComment {
    pub id: String,
    pub body: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    user: Option<CommentUser>,
}

impl ApiComment {
    /// The comment author's name, if any.
    pub fn author_name(&self) -> Option<String> {
        self.user.as_ref().map(|u| u.name.clone())
    }
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

/// Fetch every comment for `issue_id` from the Linear API, paginating until the
/// thread is exhausted. Returns an empty vec when the issue is not found.
pub fn fetch_all(transport: &dyn GraphqlTransport, issue_id: &str) -> Result<Vec<ApiComment>> {
    let mut all: Vec<ApiComment> = Vec::new();
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
        all.extend(conn_data.nodes);

        if !conn_data.page_info.has_next_page {
            break;
        }
        cursor = conn_data.page_info.end_cursor;
    }

    Ok(all)
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

    #[test]
    fn fetch_all_paginates() {
        let transport = FakeTransport::new(vec![
            comments_page(&[comment_node("c1", "first")], true, Some("cur")),
            comments_page(&[comment_node("c2", "second")], false, None),
        ]);
        let comments = fetch_all(&transport, "i1").unwrap();
        assert_eq!(
            comments.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["c1", "c2"]
        );
        // Second request carries the first page's cursor.
        assert_eq!(transport.variables(1)["after"], json!("cur"));
    }

    #[test]
    fn fetch_all_missing_issue_is_empty() {
        let transport = FakeTransport::new(vec![json!({ "issue": null })]);
        assert!(fetch_all(&transport, "i1").unwrap().is_empty());
    }

    #[test]
    fn api_comment_author_name() {
        let with_author: ApiComment = serde_json::from_value(comment_node("c1", "b")).unwrap();
        assert_eq!(with_author.author_name().as_deref(), Some("Alice"));

        let no_author: ApiComment = serde_json::from_value(json!({
            "id": "c2", "body": "note",
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
            "user": null
        }))
        .unwrap();
        assert!(no_author.author_name().is_none());
    }
}
