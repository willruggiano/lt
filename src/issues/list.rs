use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::json;

use crate::config;
use crate::linear::client::graphql_query;
use crate::linear::types::PageInfo;

use super::IssueArgs;
use super::display::print_table;
use super::filter::build_filter;
use super::sort::build_sort;

const ISSUES_QUERY: &str = r#"
query Issues($filter: IssueFilter, $sort: [IssueSortInput!], $first: Int, $after: String) {
  issues(filter: $filter, sort: $sort, first: $first, after: $after) {
    nodes {
      identifier
      title
      priorityLabel
      state { name }
      assignee { name }
      team { name }
      createdAt
      updatedAt
    }
    pageInfo { hasNextPage endCursor }
  }
}
"#;

#[derive(Deserialize)]
pub struct State {
    pub name: String,
}

#[derive(Deserialize)]
pub struct User {
    pub name: String,
}

#[derive(Deserialize)]
pub struct Team {
    pub name: String,
}

#[derive(Deserialize)]
pub struct Issue {
    pub identifier: String,
    pub title: String,
    #[serde(rename = "priorityLabel")]
    pub priority_label: String,
    pub state: State,
    pub assignee: Option<User>,
    pub team: Team,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

#[derive(Deserialize)]
struct IssueConnection {
    nodes: Vec<Issue>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Deserialize)]
struct IssuesData {
    issues: IssueConnection,
}

pub fn fetch(args: &IssueArgs, after: Option<&str>) -> Result<(Vec<Issue>, bool, Option<String>)> {
    let token = config::load_token()?
        .ok_or_else(|| anyhow!("not logged in -- run `lt auth login` first"))?;

    let limit = args.limit.min(250);
    let filter = build_filter(args)?;
    let sort = build_sort(&args.sort, args.desc);

    let variables = json!({
        "filter": filter,
        "sort": sort,
        "first": limit,
        "after": after,
    });

    let data: IssuesData =
        graphql_query(&token.access_token, ISSUES_QUERY, variables)?;

    let conn = data.issues;
    Ok((conn.nodes, conn.page_info.has_next_page, conn.page_info.end_cursor))
}

pub fn run(args: IssueArgs) -> Result<()> {
    let (issues, has_next_page, _) = fetch(&args, None)?;
    print_table(&issues);
    if has_next_page {
        println!("\n+more issues");
    }
    Ok(())
}
