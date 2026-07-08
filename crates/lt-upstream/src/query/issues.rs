//! The issues list query and the `issueUpdate`/`issueCreate` mutations,
//! modelled as cynic `QueryFragment`s. These are the shared "currency" types;
//! the fetch/replay lives in `lt-upstream`.
//!
//! [`IssueFilter`]/[`IssueSort`] are the typed, allowlisted filter/sort keys
//! (`lt_schema_codegen::search_fields`). [`IssueFilter`] lowers to
//! [`IssueFilterInput`] -- cynic `InputObject`s
//! mirroring the allowlisted subset of Linear's own `IssueFilter` shape --
//! whose derived `Serialize` produces the wire JSON; `IssueSort`'s `Serialize`
//! produces the wire `[IssueSortInput!]` JSON directly. `lt-storage` lowers
//! the same `IssueFilter` value to SQL.

use cynic::variables::VariableType;
use cynic::{MutationBuilder, QueryBuilder};
use linear_schema::linear as schema;

use super::graphql::{GraphqlOperation, ensure_success, extract_on_success};
use super::inputs::{IssueCreateInput, IssueUpdateInput};
use super::pagination::PageInfo;
use super::scalars::Priority;
use super::types::Issue;
use super::{SortDirection, SortField};

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

// ---------------------------------------------------------------------------
// Wire `IssueFilter`: cynic `InputObject`s mirroring the allowlisted subset of
// Linear's own `IssueFilter`/comparator input shapes. `IssueFilter` (above)
// stays the producers' vocabulary; these are only the wire projection it
// lowers to, so the Rust type name (`IssueFilterInput`) differs from the
// GraphQL one it maps to (`IssueFilter`) -- the same twist [`IssueSort`]/
// `schema::IssueSortInput` already make below.
// ---------------------------------------------------------------------------

/// A raw ISO-8601 timestamp coerced into Linear's `DateTimeOrDuration`
/// comparator scalar (which also accepts a duration; this app only ever
/// sends a timestamp).
#[derive(cynic::Scalar, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "DateTimeOrDuration")]
pub struct DateTimeOrDuration(pub String);

#[derive(cynic::InputObject, Debug, Clone, Default, PartialEq)]
#[cynic(graphql_type = "StringComparator", rename_all = "camelCase")]
pub struct StringComparator {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub contains_ignore_case: Option<String>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub eq_ignore_case: Option<String>,
}

impl StringComparator {
    fn contains_ignore_case(value: impl Into<String>) -> Self {
        Self {
            contains_ignore_case: Some(value.into()),
            ..Self::default()
        }
    }

    fn eq_ignore_case(value: impl Into<String>) -> Self {
        Self {
            eq_ignore_case: Some(value.into()),
            ..Self::default()
        }
    }
}

#[derive(cynic::InputObject, Debug, Clone, Default, PartialEq)]
#[cynic(graphql_type = "NullableNumberComparator", rename_all = "camelCase")]
pub struct NullableNumberComparator {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub eq: Option<f64>,
}

#[derive(cynic::InputObject, Debug, Clone, Default, PartialEq)]
#[cynic(graphql_type = "DateComparator", rename_all = "camelCase")]
pub struct DateComparator {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub gte: Option<DateTimeOrDuration>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub lt: Option<DateTimeOrDuration>,
}

#[derive(cynic::InputObject, Debug, Clone, Default, PartialEq)]
#[cynic(graphql_type = "TeamFilter", rename_all = "camelCase")]
pub struct TeamFilter {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub key: Option<StringComparator>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub name: Option<StringComparator>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub or: Option<Vec<TeamFilter>>,
}

/// Both `assignee` and `creator` take Linear's `NullableUserFilter`.
#[derive(cynic::InputObject, Debug, Clone, Default, PartialEq)]
#[cynic(graphql_type = "NullableUserFilter", rename_all = "camelCase")]
pub struct NullableUserFilter {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub null: Option<bool>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub name: Option<StringComparator>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub email: Option<StringComparator>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub or: Option<Vec<NullableUserFilter>>,
}

impl AssigneeFilter {
    fn to_wire(&self) -> NullableUserFilter {
        match self {
            AssigneeFilter::IsNull => NullableUserFilter {
                null: Some(true),
                ..NullableUserFilter::default()
            },
            AssigneeFilter::Exact(name) | AssigneeFilter::Contains(name) => NullableUserFilter {
                or: Some(vec![
                    NullableUserFilter {
                        name: Some(StringComparator::contains_ignore_case(name)),
                        ..NullableUserFilter::default()
                    },
                    NullableUserFilter {
                        email: Some(StringComparator::contains_ignore_case(name)),
                        ..NullableUserFilter::default()
                    },
                ]),
                ..NullableUserFilter::default()
            },
        }
    }
}

#[derive(cynic::InputObject, Debug, Clone, Default, PartialEq)]
#[cynic(graphql_type = "WorkflowStateFilter", rename_all = "camelCase")]
pub struct WorkflowStateFilter {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub name: Option<StringComparator>,
}

#[derive(cynic::InputObject, Debug, Clone, Default, PartialEq)]
#[cynic(graphql_type = "IssueLabelCollectionFilter", rename_all = "camelCase")]
pub struct IssueLabelCollectionFilter {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub name: Option<StringComparator>,
}

#[derive(cynic::InputObject, Debug, Clone, Default, PartialEq)]
#[cynic(graphql_type = "NullableProjectFilter", rename_all = "camelCase")]
pub struct NullableProjectFilter {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub name: Option<StringComparator>,
}

#[derive(cynic::InputObject, Debug, Clone, Default, PartialEq)]
#[cynic(graphql_type = "NullableCycleFilter", rename_all = "camelCase")]
pub struct NullableCycleFilter {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub name: Option<StringComparator>,
}

/// The wire projection of [`IssueFilter`]: only the fields the app's filter
/// lowers into. `and` composes multiple set fields (Linear's `IssueFilter`
/// takes exactly one comparator per field, so AND-joining two constraints on
/// different fields needs the explicit compound filter).
#[derive(cynic::InputObject, Debug, Clone, Default, PartialEq)]
#[cynic(graphql_type = "IssueFilter", rename_all = "camelCase")]
pub struct IssueFilterInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub and: Option<Vec<IssueFilterInput>>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub team: Option<TeamFilter>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<NullableUserFilter>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub state: Option<WorkflowStateFilter>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub priority: Option<NullableNumberComparator>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateComparator>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateComparator>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub title: Option<StringComparator>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub labels: Option<IssueLabelCollectionFilter>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub project: Option<NullableProjectFilter>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub cycle: Option<NullableCycleFilter>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub creator: Option<NullableUserFilter>,
}

impl IssueFilterInput {
    fn team(filter: TeamFilter) -> Self {
        Self {
            team: Some(filter),
            ..Self::default()
        }
    }

    fn assignee(filter: NullableUserFilter) -> Self {
        Self {
            assignee: Some(filter),
            ..Self::default()
        }
    }

    fn state(filter: WorkflowStateFilter) -> Self {
        Self {
            state: Some(filter),
            ..Self::default()
        }
    }

    fn priority(filter: NullableNumberComparator) -> Self {
        Self {
            priority: Some(filter),
            ..Self::default()
        }
    }

    fn created_at(filter: DateComparator) -> Self {
        Self {
            created_at: Some(filter),
            ..Self::default()
        }
    }

    fn updated_at(filter: DateComparator) -> Self {
        Self {
            updated_at: Some(filter),
            ..Self::default()
        }
    }

    fn title(filter: StringComparator) -> Self {
        Self {
            title: Some(filter),
            ..Self::default()
        }
    }

    fn labels(filter: IssueLabelCollectionFilter) -> Self {
        Self {
            labels: Some(filter),
            ..Self::default()
        }
    }

    fn project(filter: NullableProjectFilter) -> Self {
        Self {
            project: Some(filter),
            ..Self::default()
        }
    }

    fn cycle(filter: NullableCycleFilter) -> Self {
        Self {
            cycle: Some(filter),
            ..Self::default()
        }
    }

    fn creator(filter: NullableUserFilter) -> Self {
        Self {
            creator: Some(filter),
            ..Self::default()
        }
    }
}

/// `team`'s wire filter: an exact key or a substring name match.
fn team_filter(term: &str) -> TeamFilter {
    TeamFilter {
        or: Some(vec![
            TeamFilter {
                key: Some(StringComparator::eq_ignore_case(term)),
                ..TeamFilter::default()
            },
            TeamFilter {
                name: Some(StringComparator::contains_ignore_case(term)),
                ..TeamFilter::default()
            },
        ]),
        ..TeamFilter::default()
    }
}

/// `team`/`assignee`/`state`/`priority` -- the non-string-comparator fields.
fn entity_filters(filter: &IssueFilter) -> Vec<IssueFilterInput> {
    let mut filters = Vec::new();

    if let Some(team) = &filter.team {
        filters.push(IssueFilterInput::team(team_filter(team)));
    }
    if let Some(assignee) = &filter.assignee {
        filters.push(IssueFilterInput::assignee(assignee.to_wire()));
    }
    if let Some(state) = &filter.state {
        filters.push(IssueFilterInput::state(WorkflowStateFilter {
            name: Some(StringComparator::contains_ignore_case(state)),
        }));
    }
    if let Some(priority) = filter.priority {
        filters.push(IssueFilterInput::priority(NullableNumberComparator {
            eq: Some(f64::from(priority.0)),
        }));
    }

    filters
}

/// `createdAt`/`updatedAt`'s inclusive-lower/exclusive-upper bounds, each its
/// own AND-joined comparator (Linear's `DateComparator` takes one field per
/// bound, not a combined `{gte, lt}` in one push).
fn date_filters(filter: &IssueFilter) -> Vec<IssueFilterInput> {
    let bound = |date: &str, gte: bool| DateComparator {
        gte: gte.then(|| DateTimeOrDuration(date.to_string())),
        lt: (!gte).then(|| DateTimeOrDuration(date.to_string())),
    };

    let mut filters = Vec::new();
    if let Some(date) = &filter.created_after {
        filters.push(IssueFilterInput::created_at(bound(date, true)));
    }
    if let Some(date) = &filter.created_before {
        filters.push(IssueFilterInput::created_at(bound(date, false)));
    }
    if let Some(date) = &filter.updated_after {
        filters.push(IssueFilterInput::updated_at(bound(date, true)));
    }
    if let Some(date) = &filter.updated_before {
        filters.push(IssueFilterInput::updated_at(bound(date, false)));
    }
    filters
}

/// `title`/`labels`/`project`/`cycle`/`creator`, plus the free-text `term`
/// (which also maps to a title substring, independently of `title` itself).
fn text_filters(filter: &IssueFilter) -> Vec<IssueFilterInput> {
    let mut filters = Vec::new();

    if let Some(title) = &filter.title {
        filters.push(IssueFilterInput::title(
            StringComparator::contains_ignore_case(title),
        ));
    }
    if let Some(term) = &filter.term {
        filters.push(IssueFilterInput::title(
            StringComparator::contains_ignore_case(term),
        ));
    }
    if let Some(label) = &filter.label {
        filters.push(IssueFilterInput::labels(IssueLabelCollectionFilter {
            name: Some(StringComparator::contains_ignore_case(label)),
        }));
    }
    if let Some(project) = &filter.project {
        filters.push(IssueFilterInput::project(NullableProjectFilter {
            name: Some(StringComparator::contains_ignore_case(project)),
        }));
    }
    if let Some(cycle) = &filter.cycle {
        filters.push(IssueFilterInput::cycle(NullableCycleFilter {
            name: Some(StringComparator::contains_ignore_case(cycle)),
        }));
    }
    if let Some(creator) = &filter.creator {
        filters.push(IssueFilterInput::creator(NullableUserFilter {
            name: Some(StringComparator::contains_ignore_case(creator)),
            ..NullableUserFilter::default()
        }));
    }

    filters
}

impl IssueFilter {
    /// The Linear `IssueFilter` input this filter lowers to. AND-joins every
    /// set field.
    fn to_wire(&self) -> Option<IssueFilterInput> {
        let mut filters = entity_filters(self);
        filters.extend(date_filters(self));
        filters.extend(text_filters(self));

        match filters.len() {
            0 => None,
            1 => filters.pop(),
            _ => Some(IssueFilterInput {
                and: Some(filters),
                ..IssueFilterInput::default()
            }),
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
    pub direction: SortDirection,
}

impl serde::Serialize for IssueSort {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let desc = self.direction == SortDirection::Descending;
        super::build_sort(&self.field, desc).serialize(serializer)
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

#[derive(Default, cynic::QueryFragment)]
pub struct IssueConnection {
    pub nodes: Vec<Issue>,
    pub page_info: PageInfo,
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
}

impl TryFrom<IssuesQuery> for IssueConnection {
    type Error = anyhow::Error;

    fn try_from(op: IssuesQuery) -> anyhow::Result<Self> {
        Ok(op.issues)
    }
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

#[derive(cynic::QueryVariables, Clone, serde::Deserialize)]
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
}

impl TryFrom<IssueUpdateMutation> for Option<Issue> {
    type Error = anyhow::Error;

    fn try_from(op: IssueUpdateMutation) -> anyhow::Result<Self> {
        extract_on_success(
            IssueUpdateMutation::NAME,
            op.issue_update.success,
            op.issue_update.issue,
        )
    }
}

#[derive(cynic::QueryVariables, Clone, serde::Deserialize)]
pub struct IssueCreateVariables {
    pub input: IssueCreateInput,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "IssuePayload")]
pub struct IssuePayload {
    pub success: bool,
    pub issue: Option<Issue>,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Mutation", variables = "IssueCreateVariables")]
pub struct IssueCreateMutation {
    #[arguments(input: $input)]
    pub issue_create: IssuePayload,
}

impl GraphqlOperation for IssueCreateMutation {
    type Variables = IssueCreateVariables;
    type Output = Issue;
    const NAME: &'static str = "issueCreate";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }
}

impl TryFrom<IssueCreateMutation> for Issue {
    type Error = anyhow::Error;

    fn try_from(op: IssueCreateMutation) -> anyhow::Result<Self> {
        ensure_success(IssueCreateMutation::NAME, op.issue_create.success)?;
        op.issue_create
            .issue
            .ok_or_else(|| anyhow::anyhow!("issueCreate returned no entity"))
    }
}

/// A minimal GraphQL issue node matching `wire::Issue`'s deserialization,
/// shared by this module's tests and by `lt-upstream`/`lt-runtime`'s tests
/// (via the `test-util` feature) so the fixture has one definition.
#[cfg(any(test, feature = "test-util"))]
pub fn sample_issue_node(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "identifier": format!("ENG-{id}"), "title": "t",
        "priorityLabel": "High", "priority": 2,
        "state": { "id": "s", "name": "Todo", "position": 1.0 },
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
    use serde_json::json;

    use super::*;

    fn wire(filter: &IssueFilter) -> serde_json::Value {
        serde_json::to_value(filter).unwrap()
    }

    #[test]
    fn issue_filter_wire_omits_all_fields_when_none_set() {
        assert_eq!(wire(&IssueFilter::default()), serde_json::Value::Null);
    }

    #[test]
    fn issue_filter_wire_team_matches_key_or_name() {
        let filter = IssueFilter {
            team: Some("ENG".to_string()),
            ..IssueFilter::default()
        };
        assert_eq!(
            wire(&filter),
            json!({ "team": { "or": [
                { "key": { "eqIgnoreCase": "ENG" } },
                { "name": { "containsIgnoreCase": "ENG" } }
            ] } })
        );
    }

    #[test]
    fn issue_filter_wire_assignee_is_null() {
        let filter = IssueFilter {
            assignee: Some(AssigneeFilter::IsNull),
            ..IssueFilter::default()
        };
        assert_eq!(wire(&filter), json!({ "assignee": { "null": true } }));
    }

    #[test]
    fn issue_filter_wire_assignee_exact_and_contains_match_name_or_email() {
        for assignee in [
            AssigneeFilter::Exact("ada".to_string()),
            AssigneeFilter::Contains("ada".to_string()),
        ] {
            let filter = IssueFilter {
                assignee: Some(assignee),
                ..IssueFilter::default()
            };
            assert_eq!(
                wire(&filter),
                json!({ "assignee": { "or": [
                    { "name": { "containsIgnoreCase": "ada" } },
                    { "email": { "containsIgnoreCase": "ada" } }
                ] } })
            );
        }
    }

    #[test]
    fn issue_filter_wire_state_priority_and_title() {
        assert_eq!(
            wire(&IssueFilter {
                state: Some("Todo".to_string()),
                ..IssueFilter::default()
            }),
            json!({ "state": { "name": { "containsIgnoreCase": "Todo" } } })
        );
        assert_eq!(
            wire(&IssueFilter {
                priority: Some(Priority(2)),
                ..IssueFilter::default()
            }),
            json!({ "priority": { "eq": 2.0 } })
        );
        assert_eq!(
            wire(&IssueFilter {
                title: Some("crash".to_string()),
                ..IssueFilter::default()
            }),
            json!({ "title": { "containsIgnoreCase": "crash" } })
        );
    }

    #[test]
    fn issue_filter_wire_dates_use_gte_and_lt() {
        assert_eq!(
            wire(&IssueFilter {
                created_after: Some("2025-01-01T00:00:00Z".to_string()),
                ..IssueFilter::default()
            }),
            json!({ "createdAt": { "gte": "2025-01-01T00:00:00Z" } })
        );
        assert_eq!(
            wire(&IssueFilter {
                created_before: Some("2025-02-01T00:00:00Z".to_string()),
                ..IssueFilter::default()
            }),
            json!({ "createdAt": { "lt": "2025-02-01T00:00:00Z" } })
        );
        assert_eq!(
            wire(&IssueFilter {
                updated_after: Some("2025-01-01T00:00:00Z".to_string()),
                ..IssueFilter::default()
            }),
            json!({ "updatedAt": { "gte": "2025-01-01T00:00:00Z" } })
        );
        assert_eq!(
            wire(&IssueFilter {
                updated_before: Some("2025-02-01T00:00:00Z".to_string()),
                ..IssueFilter::default()
            }),
            json!({ "updatedAt": { "lt": "2025-02-01T00:00:00Z" } })
        );
    }

    #[test]
    fn issue_filter_wire_label_project_cycle_and_creator() {
        assert_eq!(
            wire(&IssueFilter {
                label: Some("backend".to_string()),
                ..IssueFilter::default()
            }),
            json!({ "labels": { "name": { "containsIgnoreCase": "backend" } } })
        );
        assert_eq!(
            wire(&IssueFilter {
                project: Some("Roadmap".to_string()),
                ..IssueFilter::default()
            }),
            json!({ "project": { "name": { "containsIgnoreCase": "Roadmap" } } })
        );
        assert_eq!(
            wire(&IssueFilter {
                cycle: Some("Q1".to_string()),
                ..IssueFilter::default()
            }),
            json!({ "cycle": { "name": { "containsIgnoreCase": "Q1" } } })
        );
        assert_eq!(
            wire(&IssueFilter {
                creator: Some("Ada".to_string()),
                ..IssueFilter::default()
            }),
            json!({ "creator": { "name": { "containsIgnoreCase": "Ada" } } })
        );
    }

    #[test]
    fn issue_filter_wire_term_maps_to_title_contains_ignore_case() {
        assert_eq!(
            wire(&IssueFilter {
                term: Some("oauth".to_string()),
                ..IssueFilter::default()
            }),
            json!({ "title": { "containsIgnoreCase": "oauth" } })
        );
    }

    #[test]
    fn issue_filter_wire_ands_every_set_field_in_declaration_order() {
        let filter = IssueFilter {
            state: Some("Todo".to_string()),
            title: Some("crash".to_string()),
            label: Some("backend".to_string()),
            ..IssueFilter::default()
        };
        assert_eq!(
            wire(&filter),
            json!({ "and": [
                { "state": { "name": { "containsIgnoreCase": "Todo" } } },
                { "title": { "containsIgnoreCase": "crash" } },
                { "labels": { "name": { "containsIgnoreCase": "backend" } } }
            ] })
        );
    }

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
    fn issues_query_recomposes_into_the_connection() {
        let data = serde_json::json!({
            "issues": {
                "nodes": [sample_issue_node("1")],
                "pageInfo": { "hasNextPage": true, "endCursor": "50" }
            }
        });
        let page: IssueConnection = serde_json::from_value::<IssuesQuery>(data)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(page.nodes.len(), 1);
        assert!(page.page_info.has_next_page);
        assert_eq!(page.page_info.end_cursor.as_deref(), Some("50"));
    }

    #[test]
    fn issue_update_recompose_tolerates_absent_issue() {
        let data = serde_json::json!({
            "issueUpdate": { "success": true, "issue": null }
        });
        let issue: Option<Issue> = serde_json::from_value::<IssueUpdateMutation>(data)
            .unwrap()
            .try_into()
            .unwrap();
        assert!(issue.is_none());
    }

    #[test]
    fn issue_update_recompose_rejects_success_false() {
        let data = serde_json::json!({
            "issueUpdate": { "success": false, "issue": null }
        });
        let Err(err) =
            Option::<Issue>::try_from(serde_json::from_value::<IssueUpdateMutation>(data).unwrap())
        else {
            panic!("expected a success=false error");
        };
        assert!(err.to_string().contains("issueUpdate"));
    }

    #[test]
    fn issue_create_recompose_returns_entity() {
        let data = serde_json::json!({
            "issueCreate": { "success": true, "issue": sample_issue_node("1") }
        });
        let issue: Issue = serde_json::from_value::<IssueCreateMutation>(data)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(issue.identifier, "ENG-1");
    }

    #[test]
    fn issue_create_recompose_rejects_absent_issue_on_success() {
        let data = serde_json::json!({
            "issueCreate": { "success": true, "issue": null }
        });
        let Err(err) =
            Issue::try_from(serde_json::from_value::<IssueCreateMutation>(data).unwrap())
        else {
            panic!("expected a missing-entity error");
        };
        assert!(err.to_string().contains("issueCreate"));
    }
}
