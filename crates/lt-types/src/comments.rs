//! The per-issue comment thread query and the `commentCreate` mutation,
//! modelled as cynic `QueryFragment`s. These are the shared "currency" types;
//! the fetch/replay lives in `lt-upstream`, and `lt-storage` reconstructs the
//! same [`Comment`] from its relational joins -- there is one `Comment` type,
//! not a wire projection plus a mirrored domain type.

use cynic::{MutationBuilder, QueryBuilder};

use crate::inputs::CommentCreateInput;
use crate::pagination::PageInfo;
use crate::scalars::DateTime;
use crate::schema;
use crate::types::User;

#[derive(cynic::QueryVariables)]
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

/// The built issue-comments query string.
#[must_use]
pub fn query() -> String {
    CommentsQuery::build(CommentsVariables {
        id: String::new(),
        after: None,
    })
    .query
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Issue", variables = "CommentsVariables")]
pub struct IssueWithComments {
    #[arguments(first: 100, after: $after)]
    pub comments: CommentConnection,
}

#[derive(cynic::QueryFragment)]
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

#[derive(cynic::QueryVariables)]
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

/// The built `commentCreate` mutation string.
#[must_use]
pub fn create_mutation() -> String {
    CommentCreateMutation::build(CommentCreateVariables {
        input: CommentCreateInput {
            issue_id: String::new(),
            body: String::new(),
        },
    })
    .query
}

#[cfg(test)]
mod tests {
    use super::{Comment, create_mutation, query};

    #[test]
    fn query_declares_expected_variables() {
        let built = query();
        assert!(built.contains("$id: String!"));
        assert!(built.contains("$after: String"));
    }

    #[test]
    fn create_mutation_declares_expected_variables_and_name() {
        let built = create_mutation();
        assert!(built.contains("commentCreate"));
        assert!(built.contains("$input: CommentCreateInput!"));
    }

    #[test]
    fn author_falls_back_to_unknown() {
        let comment = Comment {
            id: cynic::Id::new("c1"),
            body: "hi".to_string(),
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            user: None,
        };
        assert_eq!(comment.author(), "unknown");
    }
}
