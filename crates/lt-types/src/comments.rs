//! The per-issue comment thread query and the `commentCreate` mutation,
//! modelled as cynic `QueryFragment`s. These are the shared "currency" types;
//! the fetch/replay lives in `lt-upstream`.

use cynic::{MutationBuilder, QueryBuilder};

use crate::inputs::CommentCreateInput;
use crate::pagination::PageInfo;
use crate::scalars::DateTime;
use crate::schema;

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
    pub nodes: Vec<CommentNode>,
    pub page_info: PageInfo,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "User")]
pub struct CommentUserRef {
    pub name: String,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Comment")]
pub struct CommentNode {
    pub id: cynic::Id,
    pub body: String,
    pub created_at: DateTime,
    pub updated_at: DateTime,
    pub user: Option<CommentUserRef>,
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
    pub comment_create: CommentCreatePayload,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "CommentPayload")]
pub struct CommentCreatePayload {
    pub success: bool,
    pub comment: CreatedCommentNode,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Comment")]
pub struct CreatedCommentNode {
    pub id: cynic::Id,
    pub body: String,
    pub created_at: DateTime,
    pub updated_at: DateTime,
    pub user: Option<CommentUserRef>,
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
    use super::{create_mutation, query};

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
}
