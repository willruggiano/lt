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

#[derive(Deserialize, Debug, Clone)]
pub struct Label {
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

#[derive(Deserialize, Debug, Clone)]
pub struct LabelConnection {
    pub nodes: Vec<Label>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CommentConnection {
    pub nodes: Vec<Comment>,
}

#[derive(Deserialize, Clone)]
pub struct Parent {
    pub id: String,
    pub identifier: String,
}

#[derive(Deserialize, Clone)]
pub struct State {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize, Clone)]
pub struct User {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize, Clone)]
pub struct Team {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize, Clone)]
pub struct Project {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize, Clone)]
pub struct Cycle {
    pub id: String,
    // Nullable in Linear's schema -- unnamed cycles identify by number.
    pub name: Option<String>,
}

/// An issue node from the `issues` list query. The display surfaces (TUI table,
/// CLI table) render these; the local cache rehydrates them via
/// `From<db::Issue>` (see `db::issues`).
#[derive(Deserialize, Clone)]
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

/// Map a Linear priority label to its numeric level. Lossy: any unrecognised
/// label (including "No priority") collapses to 0, so this is a parse, not a
/// `From`.
pub(crate) fn priority_label_to_u8(label: &str) -> u8 {
    match label.to_lowercase().as_str() {
        "urgent" => 1,
        "high" => 2,
        "normal" | "medium" => 3,
        "low" => 4,
        _ => 0,
    }
}
