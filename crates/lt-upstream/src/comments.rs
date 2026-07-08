//! The comment domain: replay of queued `commentCreate` mutations. Fetching an
//! issue's comment thread executes [`crate::query::comments::CommentsQuery`]
//! directly (`lt-runtime`'s `IssueDetailQuery` refresh paginates it to
//! exhaustion); persistence lives in `lt-storage`.

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::client::{FakeTransport, execute};
    use crate::query::comments::CommentCreateMutation;
    use crate::query::inputs::CommentCreateInput;

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
            crate::query::comments::CommentCreateVariables {
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
            crate::query::comments::CommentCreateVariables {
                input: CommentCreateInput {
                    issue_id: "i1".to_string(),
                    body: "hi".to_string(),
                },
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("commentCreate"));
    }
}
