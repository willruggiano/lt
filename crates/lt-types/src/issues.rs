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

use crate::graphql::{GraphqlOperation, ensure_success, extract_on_success};
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

impl GraphqlOperation for IssuesQuery {
    type Variables = IssuesVariables;
    type Output = IssueConnection;
    const NAME: &'static str = "issues";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }

    fn extract(self) -> anyhow::Result<Self::Output> {
        Ok(self.issues)
    }
}

#[derive(cynic::QueryFragment)]
pub struct IssueConnection {
    pub nodes: Vec<Issue>,
    pub page_info: PageInfo,
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

#[derive(cynic::QueryVariables, serde::Deserialize)]
pub struct IssueUpdateVariables {
    pub id: String,
    pub input: IssueUpdateInput,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Mutation", variables = "IssueUpdateVariables")]
pub struct IssueUpdateMutation {
    #[arguments(id: $id, input: $input)]
    pub issue_update: IssuePayload,
}

impl GraphqlOperation for IssueUpdateMutation {
    type Variables = IssueUpdateVariables;
    type Output = Option<Issue>;
    const NAME: &'static str = "issueUpdate";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }

    fn extract(self) -> anyhow::Result<Self::Output> {
        extract_on_success(
            Self::NAME,
            self.issue_update.success,
            self.issue_update.issue,
        )
    }
}

#[derive(cynic::QueryVariables, serde::Deserialize)]
pub struct IssueCreateVariables {
    pub input: IssueCreateInput,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Mutation", variables = "IssueCreateVariables")]
pub struct IssueCreateMutation {
    #[arguments(input: $input)]
    pub issue_create: IssuePayload,
}

/// The `IssueUpdate`/`IssueCreate` response envelope, shared by both mutations:
/// a success flag plus the full server-truth issue (nullable in the schema
/// even on success). Both mutations select the whole [`Issue`] fragment so the
/// drainer can reconcile the base from server truth rather than hand-stitching
/// fields.
#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "IssuePayload")]
pub struct IssuePayload {
    pub success: bool,
    pub issue: Option<Issue>,
}

impl GraphqlOperation for IssueCreateMutation {
    type Variables = IssueCreateVariables;
    type Output = Issue;
    const NAME: &'static str = "issueCreate";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }

    fn extract(self) -> anyhow::Result<Self::Output> {
        ensure_success(Self::NAME, self.issue_create.success)?;
        self.issue_create
            .issue
            .ok_or_else(|| anyhow::anyhow!("issueCreate returned no entity"))
    }
}

/// A minimal GraphQL issue node matching [`Issue`]'s deserialization, shared
/// by this module's tests and by `lt-upstream`/`lt-runtime`'s tests (via the
/// `test-util` feature) so the fixture has one definition.
#[cfg(any(test, feature = "test-util"))]
pub fn sample_issue_node(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "identifier": format!("ENG-{id}"), "title": "t",
        "priorityLabel": "High", "priority": 2,
        "state": { "id": "s", "name": "Todo" },
        "assignee": null,
        "team": { "id": "ENG", "name": "Engineering" },
        "description": null,
        "labels": { "nodes": [] },
        "project": null, "cycle": null, "creator": null, "parent": null,
        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_declares_expected_variables() {
        let built = IssuesQuery::operation(IssuesVariables {
            filter: None,
            sort: None,
            first: None,
            after: None,
        })
        .query;
        assert!(built.contains("$filter: IssueFilter"));
        assert!(built.contains("$sort: [IssueSortInput!]"));
        assert!(built.contains("$first: Int"));
        assert!(built.contains("$after: String"));
    }

    #[test]
    fn update_mutation_declares_expected_variables_and_name() {
        let built = IssueUpdateMutation::operation(IssueUpdateVariables {
            id: String::new(),
            input: IssueUpdateInput::default(),
        })
        .query;
        assert!(built.contains("issueUpdate"));
        assert!(built.contains("$id: String!"));
        assert!(built.contains("$input: IssueUpdateInput!"));
    }

    #[test]
    fn create_mutation_declares_expected_variables_and_name() {
        let built = IssueCreateMutation::operation(IssueCreateVariables {
            input: IssueCreateInput {
                title: String::new(),
                team_id: String::new(),
                description: None,
                state_id: None,
                priority: None,
                assignee_id: None,
            },
        })
        .query;
        assert!(built.contains("issueCreate"));
        assert!(built.contains("$input: IssueCreateInput!"));
    }

    #[test]
    fn issues_query_extract_maps_page() {
        let data = serde_json::json!({
            "issues": {
                "nodes": [sample_issue_node("1")],
                "pageInfo": { "hasNextPage": true, "endCursor": "50" }
            }
        });
        let page = serde_json::from_value::<IssuesQuery>(data)
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(page.nodes.len(), 1);
        assert!(page.page_info.has_next_page);
        assert_eq!(page.page_info.end_cursor.as_deref(), Some("50"));
    }

    #[test]
    fn issue_update_extract_tolerates_absent_issue() {
        let data = serde_json::json!({
            "issueUpdate": { "success": true, "issue": null }
        });
        let issue = serde_json::from_value::<IssueUpdateMutation>(data)
            .unwrap()
            .extract()
            .unwrap();
        assert!(issue.is_none());
    }

    #[test]
    fn issue_update_extract_rejects_success_false() {
        let data = serde_json::json!({
            "issueUpdate": { "success": false, "issue": null }
        });
        let Err(err) = serde_json::from_value::<IssueUpdateMutation>(data)
            .unwrap()
            .extract()
        else {
            panic!("expected a success=false error");
        };
        assert!(err.to_string().contains("issueUpdate"));
    }

    #[test]
    fn issue_create_extract_returns_entity() {
        let data = serde_json::json!({
            "issueCreate": { "success": true, "issue": sample_issue_node("1") }
        });
        let issue = serde_json::from_value::<IssueCreateMutation>(data)
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(issue.identifier, "ENG-1");
    }

    #[test]
    fn issue_create_extract_rejects_absent_issue_on_success() {
        let data = serde_json::json!({
            "issueCreate": { "success": true, "issue": null }
        });
        let Err(err) = serde_json::from_value::<IssueCreateMutation>(data)
            .unwrap()
            .extract()
        else {
            panic!("expected a missing-entity error");
        };
        assert!(err.to_string().contains("issueCreate"));
    }
}
