//! The wire-typed cynic `QueryFragment`s: what actually decodes off the
//! Linear GraphQL API. Every operation's document embeds these; the domain
//! projection each recomposes into (`crate::types` and each operation
//! module's own domain output) is a plain struct with an `impl From<wire::X>`.

use crate::pagination::PageInfo;
use crate::scalars::{DateTime, Priority};
use crate::schema;

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
    /// Linear's stored ordering within the team's workflow
    /// (`WorkflowState.position: Float!`).
    pub position: f64,
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

#[derive(Default, cynic::QueryFragment)]
pub struct IssueConnection {
    pub nodes: Vec<Issue>,
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

#[derive(Default, cynic::QueryFragment)]
pub struct CommentConnection {
    pub nodes: Vec<Comment>,
    pub page_info: PageInfo,
}

#[derive(Default, cynic::QueryFragment)]
pub struct TeamConnection {
    pub nodes: Vec<Team>,
}

#[derive(Default, cynic::QueryFragment)]
pub struct WorkflowStateConnection {
    pub nodes: Vec<WorkflowState>,
}

#[derive(Default, cynic::QueryFragment)]
pub struct UserConnection {
    pub nodes: Vec<User>,
}

/// Every `(team_id, WorkflowState)` pair the org-wide workflow-states fetch
/// selects, carrying its own team id so that fetch can upsert each state
/// team-scoped without a second, per-team round trip.
#[derive(Default, cynic::QueryFragment)]
#[cynic(graphql_type = "WorkflowStateConnection")]
pub struct WorkflowStateWithTeamConnection {
    pub nodes: Vec<WorkflowStateWithTeam>,
    pub page_info: PageInfo,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "WorkflowState")]
pub struct WorkflowStateWithTeam {
    pub id: cynic::Id,
    pub name: String,
    pub position: f64,
    pub team: TeamRef,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Team")]
pub struct TeamRef {
    pub id: cynic::Id,
}
