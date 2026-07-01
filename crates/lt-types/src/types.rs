#![allow(dead_code)]

use serde::Deserialize;

#[derive(Deserialize)]
pub struct GraphqlResponse<T> {
    pub data: Option<T>,
    pub errors: Option<Vec<GraphqlError>>,
}

#[derive(Deserialize)]
pub struct GraphqlError {
    pub message: String,
}

#[derive(Deserialize)]
pub struct PageInfo {
    #[serde(rename = "hasNextPage")]
    pub has_next_page: bool,
    #[serde(rename = "endCursor")]
    pub end_cursor: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Comment {
    pub body: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    pub user: Option<CommentUser>,
}

impl Comment {
    pub fn author(&self) -> &str {
        self.user.as_ref().map_or("unknown", |u| u.name.as_str())
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct CommentUser {
    pub name: String,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Label {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct IssueRef {
    pub identifier: String,
    pub title: String,
    pub state_name: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct IssueDetail {
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    #[serde(rename = "priorityLabel")]
    pub priority_label: String,
    pub state: IssueDetailState,
    pub assignee: Option<IssueDetailUser>,
    pub team: IssueDetailTeam,
    pub labels: LabelConnection,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    pub comments: CommentConnection,
    #[serde(skip)]
    pub parent: Option<IssueRef>,
    #[serde(skip)]
    pub children: Vec<IssueRef>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct IssueDetailState {
    pub name: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct IssueDetailUser {
    pub name: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct IssueDetailTeam {
    pub name: String,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct LabelConnection {
    pub nodes: Vec<Label>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CommentConnection {
    pub nodes: Vec<Comment>,
}

#[derive(Deserialize, Clone, PartialEq)]
pub struct Parent {
    pub id: String,
    pub identifier: String,
}

#[derive(Deserialize, Clone, PartialEq)]
pub struct State {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize, Clone, PartialEq)]
pub struct User {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize, Clone, PartialEq)]
pub struct Team {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize, Clone, PartialEq)]
pub struct Project {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize, Clone, PartialEq)]
pub struct Cycle {
    pub id: String,
    // Nullable in Linear's schema -- unnamed cycles identify by number.
    pub name: Option<String>,
}

#[derive(Deserialize, Clone, PartialEq)]
pub struct Issue {
    pub id: String,
    pub identifier: String,
    pub title: String,
    #[serde(rename = "priorityLabel")]
    pub priority_label: String,
    pub priority: u8,
    pub state: State,
    pub assignee: Option<User>,
    pub team: Team,
    pub description: Option<String>,
    pub labels: LabelConnection,
    pub project: Option<Project>,
    pub cycle: Option<Cycle>,
    pub creator: Option<User>,
    pub parent: Option<Parent>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

/// One page of the `issues` list query: the issue nodes plus pagination info.
#[derive(Deserialize)]
pub struct IssueConnection {
    pub nodes: Vec<Issue>,
    #[serde(rename = "pageInfo")]
    pub page_info: PageInfo,
}

#[derive(Deserialize)]
pub struct IssuesData {
    pub issues: IssueConnection,
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
