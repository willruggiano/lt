//! The shared entity fragment types and the GraphQL response envelope.

use serde::Deserialize;

use crate::scalars::{DateTime, Priority};
use crate::schema;

#[derive(Deserialize)]
pub struct GraphqlResponse<T> {
    pub data: Option<T>,
    pub errors: Option<Vec<GraphqlError>>,
}

#[derive(Deserialize)]
pub struct GraphqlError {
    pub message: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "IssueLabel")]
pub struct IssueLabel {
    pub id: cynic::Id,
    pub name: String,
}

#[derive(cynic::QueryFragment, Clone, PartialEq)]
#[cynic(graphql_type = "Issue")]
pub struct Parent {
    pub id: cynic::Id,
    pub identifier: String,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "WorkflowState")]
pub struct WorkflowState {
    pub id: cynic::Id,
    pub name: String,
    /// Linear's stored ordering within the team's workflow. Non-null on the
    /// wire (`WorkflowState.position: Float!`); `None` only for a row this
    /// app has never team-synced (`workflow_states.position IS NULL` --
    /// back-filled by an issue upsert that knows no position).
    pub position: Option<f64>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "User")]
pub struct User {
    pub id: cynic::Id,
    pub name: String,
}

#[derive(cynic::QueryFragment, Clone, PartialEq)]
#[cynic(graphql_type = "Team")]
pub struct Team {
    pub id: cynic::Id,
    pub name: String,
}

#[derive(cynic::QueryFragment, Clone, PartialEq)]
#[cynic(graphql_type = "Project")]
pub struct Project {
    pub id: cynic::Id,
    pub name: String,
}

#[derive(cynic::QueryFragment, Clone, PartialEq)]
#[cynic(graphql_type = "Cycle")]
pub struct Cycle {
    pub id: cynic::Id,
    // Nullable in Linear's schema -- unnamed cycles identify by number.
    pub name: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
#[cynic(graphql_type = "IssueLabelConnection")]
pub struct IssueLabelConnection {
    pub nodes: Vec<IssueLabel>,
}

#[derive(cynic::QueryFragment, Clone, PartialEq)]
#[cynic(graphql_type = "Issue")]
pub struct Issue {
    pub id: cynic::Id,
    pub identifier: String,
    pub title: String,
    pub priority_label: String,
    pub priority: Priority,
    pub state: WorkflowState,
    pub assignee: Option<User>,
    pub team: Team,
    pub description: Option<String>,
    pub labels: IssueLabelConnection,
    pub project: Option<Project>,
    pub cycle: Option<Cycle>,
    pub creator: Option<User>,
    pub parent: Option<Parent>,
    pub created_at: DateTime,
    pub updated_at: DateTime,
}
