//! The issue fragment types.

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

#[derive(cynic::QueryFragment, Clone, PartialEq)]
#[cynic(graphql_type = "WorkflowState")]
pub struct WorkflowState {
    pub id: cynic::Id,
    pub name: String,
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

/// Map a Linear priority label to its numeric level. Lossy: any unrecognised
/// label (including "No priority") collapses to 0, so this is a parse, not a
/// `From`.
pub fn priority_label_to_u8(label: &str) -> u8 {
    match label.to_lowercase().as_str() {
        "urgent" => 1,
        "high" => 2,
        "normal" | "medium" => 3,
        "low" => 4,
        _ => 0,
    }
}

/// Map a numeric priority level to its label, matching the popup picker's
/// vocabulary. Used to write a priority overlay back into the `priority_label`
/// base column on ack.
pub fn priority_u8_to_label(priority: u8) -> &'static str {
    match priority {
        1 => "Urgent",
        2 => "High",
        3 => "Normal",
        4 => "Low",
        _ => "No priority",
    }
}

#[cfg(test)]
mod tests {
    use super::priority_u8_to_label;

    #[test]
    fn priority_u8_to_label_covers_all_levels() {
        assert_eq!(priority_u8_to_label(0), "No priority");
        assert_eq!(priority_u8_to_label(1), "Urgent");
        assert_eq!(priority_u8_to_label(2), "High");
        assert_eq!(priority_u8_to_label(3), "Normal");
        assert_eq!(priority_u8_to_label(4), "Low");
        // Out-of-range falls back to "No priority".
        assert_eq!(priority_u8_to_label(9), "No priority");
    }
}
