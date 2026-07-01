//! The issues list query and the `issueUpdate`/`issueCreate` mutations,
//! modelled as cynic `QueryFragment`s. These are the shared "currency" types;
//! the fetch/replay lives in `lt-upstream`. The list query selects
//! [`crate::types::Issue`] directly -- there is no separate wire projection.
//!
//! The `filter`/`sort` variables are assembled at runtime as plain JSON by
//! `lt-upstream` (`build_filter`/`build_sort`), not built from typed
//! `InputObject`s. [`IssueFilterValue`] and [`IssueSortValue`] exist only so
//! the built query string declares the right GraphQL variable types
//! (`$filter: IssueFilter`, `$sort: [IssueSortInput!]`); the wire payload
//! itself is still sent as `serde_json::Value` by the caller.

use cynic::variables::VariableType;
use cynic::{MutationBuilder, QueryBuilder};

use crate::inputs::{IssueCreateInput, IssueUpdateInput};
use crate::pagination::PageInfo;
use crate::schema;
use crate::types::Issue;

/// A dynamically-assembled `IssueFilter`, carried as pre-built JSON.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IssueFilterValue(pub serde_json::Value);

impl schema::variable::Variable for IssueFilterValue {
    const TYPE: VariableType = VariableType::Named("IssueFilter");
}
cynic::impl_coercions!(IssueFilterValue, schema::IssueFilter);

/// A dynamically-assembled `[IssueSortInput!]`, carried as pre-built JSON.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IssueSortValue(pub serde_json::Value);

impl schema::variable::Variable for IssueSortValue {
    const TYPE: VariableType = VariableType::List(&VariableType::Named("IssueSortInput"));
}
cynic::impl_coercions!(IssueSortValue, schema::IssueSortInput);

#[derive(cynic::QueryVariables)]
pub struct IssuesVariables {
    pub filter: Option<IssueFilterValue>,
    pub sort: Option<IssueSortValue>,
    pub first: Option<i32>,
    pub after: Option<String>,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query", variables = "IssuesVariables")]
pub struct IssuesQuery {
    #[arguments(filter: $filter, sort: $sort, first: $first, after: $after)]
    pub issues: IssueConnection,
}

/// The built issues list query string.
#[must_use]
pub fn query() -> String {
    IssuesQuery::build(IssuesVariables {
        filter: None,
        sort: None,
        first: None,
        after: None,
    })
    .query
}

#[derive(cynic::QueryFragment)]
pub struct IssueConnection {
    pub nodes: Vec<Issue>,
    pub page_info: PageInfo,
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

#[derive(cynic::QueryVariables)]
pub struct IssueUpdateVariables {
    pub id: String,
    pub input: IssueUpdateInput,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Mutation", variables = "IssueUpdateVariables")]
pub struct IssueUpdateMutation {
    #[arguments(id: $id, input: $input)]
    pub issue_update: IssueUpdatePayload,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "IssuePayload")]
pub struct IssueUpdatePayload {
    pub success: bool,
}

/// The built `issueUpdate` mutation string.
#[must_use]
pub fn update_mutation() -> String {
    IssueUpdateMutation::build(IssueUpdateVariables {
        id: String::new(),
        input: IssueUpdateInput::default(),
    })
    .query
}

#[derive(cynic::QueryVariables)]
pub struct IssueCreateVariables {
    pub input: IssueCreateInput,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Mutation", variables = "IssueCreateVariables")]
pub struct IssueCreateMutation {
    #[arguments(input: $input)]
    pub issue_create: IssueCreatePayload,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "IssuePayload")]
pub struct IssueCreatePayload {
    pub success: bool,
    pub issue: Option<CreatedIssue>,
}

/// The trimmed selection `issueCreate` returns: enough to confirm success and
/// reconcile the optimistic temp row with the server's id/identifier.
#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Issue")]
pub struct CreatedIssue {
    pub id: cynic::Id,
    pub identifier: String,
    pub title: String,
}

/// The built `issueCreate` mutation string.
#[must_use]
pub fn create_mutation() -> String {
    IssueCreateMutation::build(IssueCreateVariables {
        input: IssueCreateInput {
            title: String::new(),
            team_id: String::new(),
            description: None,
            state_id: None,
            priority: None,
            assignee_id: None,
        },
    })
    .query
}

#[cfg(test)]
mod tests {
    use super::{create_mutation, query, update_mutation};

    #[test]
    fn query_declares_expected_variables() {
        let built = query();
        assert!(built.contains("$filter: IssueFilter"));
        assert!(built.contains("$sort: [IssueSortInput!]"));
        assert!(built.contains("$first: Int"));
        assert!(built.contains("$after: String"));
    }

    #[test]
    fn update_mutation_declares_expected_variables_and_name() {
        let built = update_mutation();
        assert!(built.contains("issueUpdate"));
        assert!(built.contains("$id: String!"));
        assert!(built.contains("$input: IssueUpdateInput!"));
    }

    #[test]
    fn create_mutation_declares_expected_variables_and_name() {
        let built = create_mutation();
        assert!(built.contains("issueCreate"));
        assert!(built.contains("$input: IssueCreateInput!"));
    }
}
