//! The issues list query and the `issueUpdate`/`issueCreate` mutations,
//! modelled as cynic `QueryFragment`s. These are the shared "currency" types;
//! the fetch/replay lives in `lt-upstream`. The list query selects
//! [`crate::types::Issue`] directly -- there is no separate wire projection.
//!
//! [`IssueFilter`]/[`IssueSort`] are the typed, allowlisted filter/sort the
//! build validates against the schema (`build/search_filter_fields.toml`).
//! Their `Serialize` impls produce the wire `IssueFilter`/`[IssueSortInput!]`
//! JSON directly; `lt-storage` lowers the same values to SQL.

use cynic::variables::VariableType;
use cynic::{MutationBuilder, QueryBuilder};
use serde_json::{Value, json};

use crate::graphql::{GraphqlOperation, ensure_success, extract_on_success};
use crate::inputs::{IssueCreateInput, IssueUpdateInput};
use crate::pagination::PageInfo;
use crate::query::SortField;
use crate::scalars::Priority;
use crate::schema;
use crate::types::Issue;

/// The allowlisted assignee filter: no assignee, an exact (typically
/// viewer-resolved) name, or a substring match. `Exact` and `Contains` lower
/// to the same wire shape (`containsIgnoreCase` on name/email); only the
/// local SQL lowering (`lt-storage`) treats them differently.
#[derive(Debug, Clone, PartialEq)]
pub enum AssigneeFilter {
    IsNull,
    Exact(String),
    Contains(String),
}

/// The allowlisted subset of Linear's `IssueFilter` this app supports,
/// carried as a typed value rather than a JSON blob: the same value lowers to
/// the wire `IssueFilter` input (this module's `Serialize` impl) and to the
/// local SQL `WHERE` clause (`lt_storage::db::filters`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IssueFilter {
    /// Team name substring, or exact key.
    pub team: Option<String>,
    pub assignee: Option<AssigneeFilter>,
    /// Workflow state name substring.
    pub state: Option<String>,
    pub priority: Option<Priority>,
    /// RFC3339 timestamp, inclusive lower bound.
    pub created_after: Option<String>,
    /// RFC3339 timestamp, exclusive upper bound.
    pub created_before: Option<String>,
    pub updated_after: Option<String>,
    pub updated_before: Option<String>,
    /// Title substring.
    pub title: Option<String>,
    /// Label name substring.
    pub label: Option<String>,
    /// Project name substring.
    pub project: Option<String>,
    /// Cycle name substring.
    pub cycle: Option<String>,
    /// Creator name substring.
    pub creator: Option<String>,
    /// Free-text term: drives local FTS; on the wire, maps to a title substring.
    pub term: Option<String>,
}

impl IssueFilter {
    /// The Linear `IssueFilter` JSON this filter lowers to. AND-joins every
    /// set field.
    fn to_wire(&self) -> Value {
        let mut filters: Vec<Value> = Vec::new();

        if let Some(team) = &self.team {
            filters.push(json!({
                "team": {
                    "or": [
                        { "key": { "eqIgnoreCase": team } },
                        { "name": { "containsIgnoreCase": team } }
                    ]
                }
            }));
        }

        match &self.assignee {
            Some(AssigneeFilter::IsNull) => {
                filters.push(json!({ "assignee": { "null": true } }));
            }
            Some(AssigneeFilter::Exact(name) | AssigneeFilter::Contains(name)) => {
                filters.push(json!({
                    "assignee": {
                        "or": [
                            { "name": { "containsIgnoreCase": name } },
                            { "email": { "containsIgnoreCase": name } }
                        ]
                    }
                }));
            }
            None => {}
        }

        if let Some(state) = &self.state {
            filters.push(json!({ "state": { "name": { "containsIgnoreCase": state } } }));
        }

        if let Some(priority) = self.priority {
            filters.push(json!({ "priority": { "eq": f64::from(priority.0) } }));
        }

        if let Some(date) = &self.created_after {
            filters.push(json!({ "createdAt": { "gte": date } }));
        }
        if let Some(date) = &self.created_before {
            filters.push(json!({ "createdAt": { "lt": date } }));
        }
        if let Some(date) = &self.updated_after {
            filters.push(json!({ "updatedAt": { "gte": date } }));
        }
        if let Some(date) = &self.updated_before {
            filters.push(json!({ "updatedAt": { "lt": date } }));
        }

        if let Some(title) = &self.title {
            filters.push(json!({ "title": { "containsIgnoreCase": title } }));
        }
        if let Some(label) = &self.label {
            filters.push(json!({ "labels": { "name": { "containsIgnoreCase": label } } }));
        }
        if let Some(project) = &self.project {
            filters.push(json!({ "project": { "name": { "containsIgnoreCase": project } } }));
        }
        if let Some(cycle) = &self.cycle {
            filters.push(json!({ "cycle": { "name": { "containsIgnoreCase": cycle } } }));
        }
        if let Some(creator) = &self.creator {
            filters.push(json!({ "creator": { "name": { "containsIgnoreCase": creator } } }));
        }
        if let Some(term) = &self.term {
            filters.push(json!({ "title": { "containsIgnoreCase": term } }));
        }

        match filters.len() {
            0 => Value::Null,
            1 => filters.remove(0),
            _ => json!({ "and": filters }),
        }
    }
}

impl serde::Serialize for IssueFilter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_wire().serialize(serializer)
    }
}

impl schema::variable::Variable for IssueFilter {
    const TYPE: VariableType = VariableType::Named("IssueFilter");
}
cynic::impl_coercions!(IssueFilter, schema::IssueFilter);

/// A typed `[IssueSortInput!]` of exactly one field: Linear accepts a list,
/// but this app only ever sorts by one field/direction.
#[derive(Debug, Clone)]
pub struct IssueSort {
    pub field: SortField,
    pub desc: bool,
}

impl serde::Serialize for IssueSort {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        crate::query::build_sort(&self.field, self.desc).serialize(serializer)
    }
}

impl schema::variable::Variable for IssueSort {
    const TYPE: VariableType = VariableType::List(&VariableType::Named("IssueSortInput"));
}
cynic::impl_coercions!(IssueSort, schema::IssueSortInput);

#[derive(cynic::QueryVariables, Clone)]
pub struct IssuesVariables {
    pub filter: Option<IssueFilter>,
    pub sort: Option<IssueSort>,
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
