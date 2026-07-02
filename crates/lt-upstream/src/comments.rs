//! The comment domain: replay of queued `commentCreate` mutations and the
//! per-issue comment fetch that pulls the API's comment thread. Persistence of
//! the fetched thread into the local DB lives in `lt-runtime`.

use anyhow::Result;
use lt_types::comments::{Comment, CommentsQuery, CommentsVariables};

use crate::client::{GraphqlTransport, execute};

// ---------------------------------------------------------------------------
// Comment fetch (pull an issue's thread from the API)
// ---------------------------------------------------------------------------

/// Fetch every comment for `issue_id` from the Linear API, paginating until the
/// thread is exhausted.
pub fn fetch_all(transport: &dyn GraphqlTransport, issue_id: &str) -> Result<Vec<Comment>> {
    let mut all: Vec<Comment> = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        let variables = CommentsVariables {
            id: issue_id.to_string(),
            after: cursor,
        };

        let page = execute::<CommentsQuery>(transport, variables)?;

        all.extend(page.nodes);

        if !page.info.has_next_page {
            break;
        }
        cursor = page.info.end_cursor;
        if cursor.is_none() {
            break;
        }
    }

    Ok(all)
}

#[cfg(test)]
mod tests {
    use lt_types::comments::CommentCreateMutation;
    use lt_types::inputs::CommentCreateInput;
    use serde_json::json;

    use super::*;
    use crate::client::FakeTransport;

    #[test]
    fn comment_create_replay_returns_server_comment() {
        let transport = FakeTransport::new(vec![json!({
            "commentCreate": { "success": true, "comment": {
                "id": "c1", "body": "hi",
                "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
                "user": { "id": "u1", "name": "Ada" },
                "issueId": "i1"
            }}
        })]);
        let created = execute::<CommentCreateMutation>(
            &transport,
            lt_types::comments::CommentCreateVariables {
                input: CommentCreateInput {
                    issue_id: "i1".to_string(),
                    body: "hi".to_string(),
                },
            },
        )
        .unwrap();
        assert_eq!(created.id.inner(), "c1");
        assert_eq!(created.user.unwrap().name, "Ada");
        assert_eq!(transport.variables(0)["input"]["issueId"], json!("i1"));
    }

    #[test]
    fn comment_create_replay_rejects_success_false() {
        let transport = FakeTransport::new(vec![json!({
            "commentCreate": { "success": false, "comment": {
                "id": "c1", "body": "hi",
                "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
                "user": null, "issueId": "i1"
            }}
        })]);
        let err = execute::<CommentCreateMutation>(
            &transport,
            lt_types::comments::CommentCreateVariables {
                input: CommentCreateInput {
                    issue_id: "i1".to_string(),
                    body: "hi".to_string(),
                },
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("commentCreate"));
    }

    fn comment_node(id: &str, body: &str) -> serde_json::Value {
        json!({
            "id": id, "body": body,
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
            "user": { "id": "u1", "name": "Alice" },
            "issueId": "i1"
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
            comments.iter().map(|c| c.id.inner()).collect::<Vec<_>>(),
            ["c1", "c2"]
        );
        // Second request carries the first page's cursor.
        assert_eq!(transport.variables(1)["after"], json!("cur"));
    }

    #[test]
    fn fetch_all_missing_issue_returns_error() {
        // `Query.issue` is non-null in the schema; a missing issue surfaces as a
        // GraphQL error rather than a null `data.issue` (unlike the old
        // hand-rolled decode, which treated `issue: null` as "no comments").
        let transport = FakeTransport::new(vec![json!({ "issue": null })]);
        assert!(fetch_all(&transport, "i1").is_err());
    }
}
