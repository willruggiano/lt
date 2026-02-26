use anyhow::Result;
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;

use crate::config;
use crate::db;
use crate::linear::client::graphql_query;
use crate::linear::types::PageInfo;

/// Re-use the same GraphQL query as list.rs but with a filter variable.
const ISSUES_QUERY: &str = r#"
query Issues($filter: IssueFilter, $sort: [IssueSortInput!], $first: Int, $after: String) {
  issues(filter: $filter, sort: $sort, first: $first, after: $after) {
    nodes {
      id
      identifier
      title
      priorityLabel
      priority
      state { id name }
      assignee { id name }
      team { id name }
      createdAt
      updatedAt
    }
    pageInfo { hasNextPage endCursor }
  }
}
"#;

#[derive(Deserialize)]
struct State {
    name: String,
}

#[derive(Deserialize)]
struct User {
    name: String,
}

#[derive(Deserialize)]
struct Team {
    name: String,
}

#[derive(Deserialize)]
struct Issue {
    id: String,
    identifier: String,
    title: String,
    #[serde(rename = "priorityLabel")]
    priority_label: String,
    state: State,
    assignee: Option<User>,
    team: Team,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
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

fn to_db_issue(src: &Issue) -> db::Issue {
    db::Issue {
        id: src.id.clone(),
        identifier: src.identifier.clone(),
        title: src.title.clone(),
        priority_label: src.priority_label.clone(),
        state_name: src.state.name.clone(),
        assignee_name: src.assignee.as_ref().map(|u| u.name.clone()),
        team_name: src.team.name.clone(),
        team_key: None,
        created_at: src.created_at.clone(),
        updated_at: src.updated_at.clone(),
        synced_at: String::new(), // filled by upsert_issues
    }
}

/// Fetch one page of issues updated after `since` (an RFC3339 timestamp).
fn fetch_page(
    token: &str,
    since: &str,
    after: Option<&str>,
) -> Result<(Vec<Issue>, bool, Option<String>)> {
    // Request all states including completed/archived so delta picks up
    // changes to previously-completed issues.
    let filter = json!({
        "updatedAt": { "gt": since }
    });

    let sort = json!([{ "updatedAt": { "order": "Descending" } }]);

    let variables = json!({
        "filter": filter,
        "sort": sort,
        "first": 250,
        "after": after,
    });

    let data: IssuesData = graphql_query(token, ISSUES_QUERY, variables)?;
    let conn = data.issues;
    Ok((
        conn.nodes,
        conn.page_info.has_next_page,
        conn.page_info.end_cursor,
    ))
}

/// Run incremental (delta) sync.
///
/// - If no `last_synced_at` is recorded, delegates to `sync full`.
/// - Otherwise fetches issues where updatedAt > last_synced_at, upserts them,
///   and updates last_synced_at.
pub fn run() -> Result<()> {
    let conn = db::open_db()?;

    let last_synced_at = db::get_meta(&conn, "last_synced_at")?;

    let since = match last_synced_at {
        None => {
            // No previous sync -- fall back to full sync.
            println!("No previous sync found -- running full sync.");
            return super::full::run();
        }
        Some(ts) => ts,
    };

    let token = config::load_token()?
        .ok_or_else(|| anyhow::anyhow!("not logged in -- run `lt auth login` first"))?;

    let mut cursor: Option<String> = None;
    let mut total = 0usize;

    loop {
        let after = cursor.as_deref();
        let (issues, has_next, end_cursor) = fetch_page(&token.access_token, &since, after)?;
        let count = issues.len();

        if count > 0 {
            let db_issues: Vec<db::Issue> = issues.iter().map(to_db_issue).collect();
            db::upsert_issues(&conn, &db_issues)?;
        }

        total += count;

        if !has_next {
            break;
        }
        cursor = end_cursor;
    }

    let now = Utc::now().to_rfc3339();
    db::set_meta(&conn, "last_synced_at", &now)?;

    println!("Delta sync: {} issue(s) updated since {}", total, since);
    Ok(())
}
