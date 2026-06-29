use std::io::Write;

use anyhow::Result;
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use tracing::{error, info};

use super::IssueArgs;
use super::display::{print_table, print_table_cached};
use super::filter::build_filter;
use super::sort::build_sort;
use crate::db;
use crate::linear::client::graphql_query;
use crate::linear::types::PageInfo;

/// Cache TTL in seconds (5 minutes).
const CACHE_TTL_SECS: i64 = 300;

pub(crate) const ISSUES_QUERY: &str = r"
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
";

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
    // Nullable in Linear's schema -- unnamed cycles identify by number.
    pub name: Option<String>,
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

/// Convert a fetched `Issue` into a `db::Issue` for caching.
pub(crate) fn to_db_issue(src: &Issue) -> db::Issue {
    let labels = src
        .labels
        .nodes
        .iter()
        .map(|l| l.name.as_str())
        .collect::<Vec<_>>()
        .join(",");
    db::Issue {
        id: src.id.clone(),
        identifier: src.identifier.clone(),
        title: src.title.clone(),
        priority_label: src.priority_label.clone(),
        state_name: src.state.name.clone(),
        assignee_name: src.assignee.as_ref().map(|u| u.name.clone()),
        team_name: src.team.name.clone(),
        team_key: Some(src.team.id.clone()),
        created_at: src.created_at.clone(),
        updated_at: src.updated_at.clone(),
        synced_at: String::new(), // filled by upsert_issues
        description: src.description.clone(),
        labels,
        project_name: src.project.as_ref().map(|p| p.name.clone()),
        cycle_name: src.cycle.as_ref().and_then(|c| c.name.clone()),
        creator_name: src.creator.as_ref().map(|u| u.name.clone()),
        parent_id: src.parent.as_ref().map(|p| p.id.clone()),
        parent_identifier: src.parent.as_ref().map(|p| p.identifier.clone()),
    }
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

/// Resolve `--assignee=me` to the viewer's actual name so the SQL filter can
/// match the cached `assignee_name` column.  Uses the identity cached in
/// `sync_meta` when available, otherwise asks the Linear API and caches it.
fn resolve_me(conn: &rusqlite::Connection, args: &mut IssueArgs) -> Result<()> {
    let is_me = args
        .assignee
        .as_deref()
        .is_some_and(|a| a.eq_ignore_ascii_case("me"));
    if !is_me {
        return Ok(());
    }
    let name = if let Some(n) = db::get_meta(conn, "viewer_name")? {
        n
    } else {
        let token = crate::auth::refresh::load_or_refresh_token()?;
        let viewer = crate::linear::viewer::fetch_viewer(&token.access_token)?;
        db::set_meta(conn, "viewer_name", &viewer.name)?;
        viewer.name
    };
    args.assignee = Some(name);
    Ok(())
}

pub fn run(out: &mut dyn Write, mut args: IssueArgs) -> Result<()> {
    // --live: bypass cache entirely.
    if args.live {
        let (issues, has_next_page, _) = fetch(&args, None)?;
        print_table(out, &issues)?;
        if has_next_page {
            writeln!(out, "\n+more issues")?;
        }
        return Ok(());
    }

    let conn = db::open_db()?;
    resolve_me(&conn, &mut args)?;

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
            print_table_cached(out, &issues, "(cached)")?;
        }
        Some(ref ts) => {
            // Parse the timestamp and check age.
            let age_secs: i64 = chrono::DateTime::parse_from_rfc3339(ts).map_or(i64::MAX, |t| {
                Utc::now().signed_duration_since(t).num_seconds()
            });

            if age_secs < CACHE_TTL_SECS {
                // Fresh cache -- serve immediately.
                let issues = db::query_issues(&conn, &args)?;
                let note = format!("(cached, age {age_secs}s)");
                print_table_cached(out, &issues, &note)?;
            } else {
                // Stale cache -- serve immediately, then run delta sync in background.
                let issues = db::query_issues(&conn, &args)?;
                let note = format!("(stale cache, age {age_secs}s -- syncing in background)");
                print_table_cached(out, &issues, &note)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn label(name: &str) -> LabelNode {
        LabelNode {
            name: name.to_string(),
        }
    }

    #[test]
    fn to_db_issue_maps_and_joins_labels() {
        let issue = Issue {
            id: "1".to_string(),
            identifier: "ENG-1".to_string(),
            title: "Wire it up".to_string(),
            priority_label: "High".to_string(),
            priority: 2,
            state: State {
                id: "s1".to_string(),
                name: "In Progress".to_string(),
            },
            assignee: Some(User {
                id: "u1".to_string(),
                name: "Alice".to_string(),
            }),
            team: Team {
                id: "ENG".to_string(),
                name: "Engineering".to_string(),
            },
            description: Some("body".to_string()),
            labels: LabelConnection {
                nodes: vec![label("bug"), label("backend")],
            },
            project: Some(Project {
                id: "p1".to_string(),
                name: "Platform".to_string(),
            }),
            cycle: Some(Cycle {
                id: "c1".to_string(),
                name: Some("Cycle 7".to_string()),
            }),
            creator: Some(User {
                id: "u2".to_string(),
                name: "Carol".to_string(),
            }),
            parent: Some(Parent {
                id: "9".to_string(),
                identifier: "ENG-9".to_string(),
            }),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
        };

        let row = to_db_issue(&issue);
        assert_eq!(row.identifier, "ENG-1");
        assert_eq!(row.assignee_name.as_deref(), Some("Alice"));
        assert_eq!(row.team_key.as_deref(), Some("ENG"));
        assert_eq!(row.labels, "bug,backend");
        assert_eq!(row.project_name.as_deref(), Some("Platform"));
        assert_eq!(row.cycle_name.as_deref(), Some("Cycle 7"));
        assert_eq!(row.creator_name.as_deref(), Some("Carol"));
        assert_eq!(row.parent_id.as_deref(), Some("9"));
        assert_eq!(row.parent_identifier.as_deref(), Some("ENG-9"));
        // synced_at is filled by upsert_issues, not the mapper.
        assert!(row.synced_at.is_empty());
    }

    #[test]
    fn to_db_issue_handles_absent_optionals() {
        let issue = Issue {
            id: "2".to_string(),
            identifier: "ENG-2".to_string(),
            title: "t".to_string(),
            priority_label: "No priority".to_string(),
            priority: 0,
            state: State {
                id: "s".to_string(),
                name: "Todo".to_string(),
            },
            assignee: None,
            team: Team {
                id: "ENG".to_string(),
                name: "Engineering".to_string(),
            },
            description: None,
            labels: LabelConnection { nodes: Vec::new() },
            project: None,
            cycle: None,
            creator: None,
            parent: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let row = to_db_issue(&issue);
        assert!(row.assignee_name.is_none());
        assert_eq!(row.labels, "");
        assert!(row.project_name.is_none());
        assert!(row.cycle_name.is_none());
        assert!(row.creator_name.is_none());
        assert!(row.parent_id.is_none());
    }
}
