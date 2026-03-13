use anyhow::Result;
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use tracing::{error, info};

use crate::db;
use crate::linear::client::graphql_query;
use crate::linear::types::PageInfo;

use super::IssueArgs;
use super::display::{print_table, print_table_cached};
use super::filter::build_filter;
use super::sort::build_sort;

/// Cache TTL in seconds (5 minutes).
const CACHE_TTL_SECS: i64 = 300;

const ISSUES_QUERY: &str = r#"
query Issues($filter: IssueFilter, $sort: [IssueSortInput!], $first: Int, $after: String) {
  issues(filter: $filter, sort: $sort, first: $first, after: $after) {
    nodes {
      id
      identifier
      title
      description
      priorityLabel
      priority
      state { id name }
      assignee { id name }
      team { id name }
      labels { nodes { name } }
      project { id name }
      cycle { id name }
      creator { id name }
      parent { id identifier }
      createdAt
      updatedAt
    }
    pageInfo { hasNextPage endCursor }
  }
}
"#;

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
    #[allow(unused)]
    pub id: String,
    pub name: String,
}

#[derive(Deserialize, Clone)]
pub struct Cycle {
    #[allow(unused)]
    pub id: String,
    pub name: String,
}

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

#[derive(Deserialize, Clone)]
pub struct LabelNode {
    pub name: String,
}

#[derive(Deserialize, Clone)]
pub struct LabelConnection {
    pub nodes: Vec<LabelNode>,
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
    let token = crate::auth::refresh::load_or_refresh_token()?;

    let limit = args.limit.min(250);
    let filter = build_filter(args)?;
    let sort = build_sort(&args.sort, args.desc);

    let variables = json!({
        "filter": filter,
        "sort": sort,
        "first": limit,
        "after": after,
    });

    let data: IssuesData = graphql_query(&token.access_token, ISSUES_QUERY, variables)?;

    let conn = data.issues;
    Ok((
        conn.nodes,
        conn.page_info.has_next_page,
        conn.page_info.end_cursor,
    ))
}

pub fn run(args: IssueArgs) -> Result<()> {
    // --live: bypass cache entirely.
    if args.live {
        let (issues, has_next_page, _) = fetch(&args, None)?;
        print_table(&issues);
        if has_next_page {
            println!("\n+more issues");
        }
        return Ok(());
    }

    let conn = db::open_db()?;

    // Check last_synced_at from sync_meta.
    let last_synced_at = db::get_meta(&conn, "last_synced_at")?;

    match last_synced_at {
        None => {
            // Cache is empty (never synced). Run full sync first.
            info!("Cache empty -- running full sync...");
            drop(conn);
            crate::sync::full::run()?;
            // Re-open after sync.
            let conn2 = db::open_db()?;
            let issues = db::query_issues(&conn2, &args)?;
            print_table_cached(&issues, "(cached)");
        }
        Some(ref ts) => {
            // Parse the timestamp and check age.
            let age_secs: i64 = chrono::DateTime::parse_from_rfc3339(ts)
                .map(|t| Utc::now().signed_duration_since(t).num_seconds())
                .unwrap_or(i64::MAX);

            if age_secs < CACHE_TTL_SECS {
                // Fresh cache -- serve immediately.
                let issues = db::query_issues(&conn, &args)?;
                let note = format!("(cached, age {}s)", age_secs);
                print_table_cached(&issues, &note);
            } else {
                // Stale cache -- serve immediately, then run delta sync in background.
                let issues = db::query_issues(&conn, &args)?;
                let note = format!("(stale cache, age {}s -- syncing in background)", age_secs);
                print_table_cached(&issues, &note);

                // Trigger delta sync in a background thread; ignore join errors.
                std::thread::spawn(|| {
                    if let Err(e) = crate::sync::delta::run() {
                        error!("background sync error: {}", e);
                    }
                });
            }
        }
    }

    Ok(())
}
