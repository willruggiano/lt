//! The domain entity types storage and the TUI hold and render, decoded from
//! their `crate::wire` counterparts by the `impl From<wire::X>` beside each.
//! The GraphQL response envelope also lives here since it is transport-level,
//! not an entity.

use serde::Deserialize;

use crate::scalars::{DateTime, Priority};
use crate::wire;

#[derive(Deserialize)]
pub struct GraphqlResponse<T> {
    pub data: Option<T>,
    pub errors: Option<Vec<GraphqlError>>,
}

#[derive(Deserialize)]
pub struct GraphqlError {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IssueLabel {
    pub id: cynic::Id,
    pub name: String,
}

impl From<wire::IssueLabel> for IssueLabel {
    fn from(w: wire::IssueLabel) -> Self {
        Self {
            id: w.id,
            name: w.name,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Parent {
    pub id: cynic::Id,
    pub identifier: String,
}

impl From<wire::Parent> for Parent {
    fn from(w: wire::Parent) -> Self {
        Self {
            id: w.id,
            identifier: w.identifier,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowState {
    pub id: cynic::Id,
    pub name: String,
    pub position: f64,
}

impl From<wire::WorkflowState> for WorkflowState {
    fn from(w: wire::WorkflowState) -> Self {
        Self {
            id: w.id,
            name: w.name,
            position: w.position,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub id: cynic::Id,
    pub name: String,
}

impl From<wire::User> for User {
    fn from(w: wire::User) -> Self {
        Self {
            id: w.id,
            name: w.name,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Team {
    pub id: cynic::Id,
    pub name: String,
}

impl From<wire::Team> for Team {
    fn from(w: wire::Team) -> Self {
        Self {
            id: w.id,
            name: w.name,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub id: cynic::Id,
    pub name: String,
}

impl From<wire::Project> for Project {
    fn from(w: wire::Project) -> Self {
        Self {
            id: w.id,
            name: w.name,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cycle {
    pub id: cynic::Id,
    pub name: Option<String>,
}

impl From<wire::Cycle> for Cycle {
    fn from(w: wire::Cycle) -> Self {
        Self {
            id: w.id,
            name: w.name,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IssueLabelConnection {
    pub nodes: Vec<IssueLabel>,
}

impl From<wire::IssueLabelConnection> for IssueLabelConnection {
    fn from(w: wire::IssueLabelConnection) -> Self {
        Self {
            nodes: w.nodes.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
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

impl From<wire::Issue> for Issue {
    fn from(w: wire::Issue) -> Self {
        Self {
            id: w.id,
            identifier: w.identifier,
            title: w.title,
            priority_label: w.priority_label,
            priority: w.priority,
            state: w.state.into(),
            assignee: w.assignee.map(Into::into),
            team: w.team.into(),
            description: w.description,
            labels: w.labels.into(),
            project: w.project.map(Into::into),
            cycle: w.cycle.map(Into::into),
            creator: w.creator.map(Into::into),
            parent: w.parent.map(Into::into),
            created_at: w.created_at,
            updated_at: w.updated_at,
        }
    }
}
