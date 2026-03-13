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
        self.user
            .as_ref()
            .map(|u| u.name.as_str())
            .unwrap_or("unknown")
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
