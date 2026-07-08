//! Shared `#[cfg(test)]` fixtures for the T6 emitters (`emit_ddl`,
//! `emit_sql`) and `ref_fragment`: the real `Issue` fragment and its SDL, kept
//! in one place so it is not duplicated per test module (`cpd`/`cargo dupes`).
//!
//! Mirrors `crates/lt-upstream/src/query/types.rs:21-99`: the `Issue`
//! fragment and its `Parent`/`WorkflowState`/`Team`/`IssueLabelConnection`
//! dependencies.

use std::collections::BTreeSet;

pub(crate) const ISSUE_FRAGMENT_SRC: &str = r#"
    #[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
    #[cynic(graphql_type = "IssueLabel")]
    pub struct IssueLabel {
        pub id: cynic::Id,
        pub name: String,
    }

    #[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
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
        pub position: f64,
    }

    #[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
    #[cynic(graphql_type = "Team")]
    pub struct Team {
        pub id: cynic::Id,
        pub name: String,
    }

    #[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
    #[cynic(graphql_type = "IssueLabelConnection")]
    pub struct IssueLabelConnection {
        pub nodes: Vec<IssueLabel>,
    }

    #[derive(cynic::QueryFragment, Debug, Clone, PartialEq)]
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
"#;

pub(crate) const ISSUE_SDL: &str = r"
    interface Node {
        id: ID!
    }

    scalar DateTime

    enum Priority {
        LOW
        MEDIUM
        HIGH
        URGENT
    }

    type WorkflowState implements Node {
        id: ID!
        name: String!
        position: Float!
    }

    type User implements Node {
        id: ID!
        name: String!
    }

    type Team implements Node {
        id: ID!
        name: String!
    }

    type Project implements Node {
        id: ID!
        name: String!
    }

    type Cycle implements Node {
        id: ID!
        name: String
    }

    type IssueLabel implements Node {
        id: ID!
        name: String!
    }

    type IssueLabelConnection {
        nodes: [IssueLabel!]!
    }

    type Issue implements Node {
        id: ID!
        identifier: String!
        title: String!
        priorityLabel: String!
        priority: Priority!
        state: WorkflowState!
        assignee: User
        team: Team!
        description: String
        labels: IssueLabelConnection!
        project: Project
        cycle: Cycle
        creator: User
        parent: Issue
        createdAt: DateTime!
        updatedAt: DateTime!
    }
";

/// The GraphQL object types with a generated table, for classification
/// against [`ISSUE_SDL`]/[`ISSUE_FRAGMENT_SRC`].
pub(crate) fn issue_generated_types() -> BTreeSet<&'static str> {
    BTreeSet::from(["Issue", "WorkflowState", "User", "Team", "Project", "Cycle"])
}
