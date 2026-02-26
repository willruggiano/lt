use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::json;

use crate::config;
use crate::linear::client::graphql_query;
use crate::linear::types::IssueDetail;

const ISSUE_DETAIL_QUERY: &str = r#"
query IssueDetail($id: String!) {
  issue(id: $id) {
    identifier
    title
    description
    priorityLabel
    state { name }
    assignee { name }
    team { name }
    labels { nodes { name } }
    createdAt
    updatedAt
    comments {
      nodes {
        body
        createdAt
        user { name }
      }
    }
  }
}
"#;

#[derive(Deserialize)]
struct IssueDetailData {
    issue: Option<IssueDetail>,
}

pub fn fetch_issue_detail(token: &str, id: &str) -> Result<IssueDetail> {
    let variables = json!({ "id": id });
    let data: IssueDetailData = graphql_query(token, ISSUE_DETAIL_QUERY, variables)?;
    data.issue
        .ok_or_else(|| anyhow!("issue '{}' not found", id))
}

pub fn fetch_issue_detail_with_config(id: &str) -> Result<IssueDetail> {
    let token = config::load_token()?
        .ok_or_else(|| anyhow!("not logged in -- run `lt auth login` first"))?;
    fetch_issue_detail(&token.access_token, id)
}
