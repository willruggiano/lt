//! The per-issue comment thread query and the `commentCreate` mutation. This
//! module's [`Comment`]/[`CommentConnection`] decode straight off the wire;
//! `lt-storage` also reconstructs [`Comment`] directly from its relational
//! joins.

use cynic::{MutationBuilder, QueryBuilder};

use crate::graphql::{GraphqlOperation, extract_on_success};
use crate::inputs::CommentCreateInput;
use crate::pagination::PageInfo;
use crate::scalars::DateTime;
use crate::schema;
use crate::types::User;

#[derive(cynic::QueryVariables, Clone)]
pub struct CommentsVariables {
    pub id: String,
    pub after: Option<String>,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query", variables = "CommentsVariables")]
pub struct CommentsQuery {
    #[arguments(id: $id)]
    pub issue: IssueWithComments,
}

impl GraphqlOperation for CommentsQuery {
    type Variables = CommentsVariables;
    type Output = CommentConnection;
    const NAME: &'static str = "comments";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }
}

impl TryFrom<CommentsQuery> for CommentConnection {
    type Error = anyhow::Error;

    fn try_from(op: CommentsQuery) -> anyhow::Result<Self> {
        Ok(op.issue.comments)
    }
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Issue", variables = "CommentsVariables")]
pub struct IssueWithComments {
    #[arguments(first: 100, after: $after)]
    pub comments: CommentConnection,
}

#[derive(Default, cynic::QueryFragment)]
pub struct CommentConnection {
    pub nodes: Vec<Comment>,
    pub page_info: PageInfo,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "Comment")]
pub struct Comment {
    pub id: cynic::Id,
    pub body: String,
    pub created_at: DateTime,
    pub updated_at: DateTime,
    pub user: Option<User>,
    /// The comment's issue, nullable since a comment can attach to something
    /// other than an issue (e.g. a project update).
    pub issue_id: Option<String>,
}

impl Comment {
    /// The comment's author display name, or "unknown" for a comment with no
    /// associated user.
    #[must_use]
    pub fn author(&self) -> &str {
        self.user.as_ref().map_or("unknown", |u| u.name.as_str())
    }
}

// ---------------------------------------------------------------------------
// Mutation
// ---------------------------------------------------------------------------

#[derive(cynic::QueryVariables, Clone, serde::Deserialize)]
pub struct CommentCreateVariables {
    pub input: CommentCreateInput,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Mutation", variables = "CommentCreateVariables")]
pub struct CommentCreateMutation {
    #[arguments(input: $input)]
    pub comment_create: CommentPayload,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "CommentPayload")]
pub struct CommentPayload {
    pub success: bool,
    pub comment: Comment,
}

impl GraphqlOperation for CommentCreateMutation {
    type Variables = CommentCreateVariables;
    type Output = Comment;
    const NAME: &'static str = "commentCreate";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }
}

impl TryFrom<CommentCreateMutation> for Comment {
    type Error = anyhow::Error;

    fn try_from(op: CommentCreateMutation) -> anyhow::Result<Self> {
        extract_on_success(
            CommentCreateMutation::NAME,
            op.comment_create.success,
            op.comment_create.comment,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_declares_expected_variables() {
        let built = CommentsQuery::operation(CommentsVariables {
            id: String::new(),
            after: None,
        })
        .query;
        assert!(built.contains("$id: String!"));
        assert!(built.contains("$after: String"));
        assert!(built.contains("issueId"));
    }

    #[test]
    fn create_mutation_declares_expected_variables_and_name() {
        let built = CommentCreateMutation::operation(CommentCreateVariables {
            input: CommentCreateInput {
                issue_id: String::new(),
                body: String::new(),
            },
        })
        .query;
        assert!(built.contains("commentCreate"));
        assert!(built.contains("$input: CommentCreateInput!"));
    }

    #[test]
    fn comments_query_recomposes_into_the_connection() {
        let data = serde_json::json!({ "issue": { "comments": {
            "nodes": [{
                "id": "c1", "body": "hi",
                "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
                "user": { "id": "u1", "name": "Ada" },
                "issueId": "i1"
            }],
            "pageInfo": { "hasNextPage": true, "endCursor": "cur" }
        }}});
        let page: CommentConnection = serde_json::from_value::<CommentsQuery>(data)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(page.nodes.len(), 1);
        assert!(page.page_info.has_next_page);
        assert_eq!(page.page_info.end_cursor.as_deref(), Some("cur"));
    }

    #[test]
    fn comment_create_recompose_rejects_success_false() {
        let data = serde_json::json!({
            "commentCreate": { "success": false, "comment": {
                "id": "c1", "body": "hi",
                "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
                "user": null, "issueId": "i1"
            }}
        });
        let err = Comment::try_from(serde_json::from_value::<CommentCreateMutation>(data).unwrap())
            .unwrap_err();
        assert!(err.to_string().contains("commentCreate"));
    }

    #[test]
    fn author_falls_back_to_unknown() {
        let comment = Comment {
            id: "c1".into(),
            body: "hi".to_string(),
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            user: None,
            issue_id: Some("i1".to_string()),
        };
        assert_eq!(comment.author(), "unknown");
    }
}
