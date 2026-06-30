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
use crate::linear::client::{GraphqlTransport, HttpTransport, query_as};
use crate::linear::types::{Issue, PageInfo};

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
    fetch_with(&HttpTransport::new(token.access_token), args, after)
}

/// Fetch one page of issues through `transport`. Splitting this from `fetch`
/// keeps the request building and page-info extraction testable with a fake
/// transport, free of the token-load IO.
pub fn fetch_with(
    transport: &dyn GraphqlTransport,
    args: &IssueArgs,
    after: Option<&str>,
) -> Result<(Vec<Issue>, bool, Option<String>)> {
    let limit = args.limit.min(250);
    let filter = build_filter(args)?;
    let sort = build_sort(&args.sort, args.desc);

    let variables = json!({
        "filter": filter,
        "sort": sort,
        "first": limit,
        "after": after,
    });

    let data: IssuesData = query_as(transport, ISSUES_QUERY, variables)?;

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
        let viewer = crate::linear::viewer::fetch_viewer(&HttpTransport::new(token.access_token))?;
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

/// A minimal GraphQL issue node matching [`Issue`]'s deserialization, shared by
/// the issue-fetch tests in this module and in `sync::delta`.
#[cfg(test)]
pub(crate) fn sample_issue_node(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "identifier": format!("ENG-{id}"), "title": "t",
        "priorityLabel": "High", "priority": 2,
        "state": { "id": "s", "name": "Todo" },
        "assignee": null,
        "team": { "id": "ENG", "name": "Engineering" },
        "description": null,
        "labels": { "nodes": [] },
        "project": null, "cycle": null, "creator": null, "parent": null,
        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::client::FakeTransport;

    #[test]
    fn fetch_with_maps_nodes_and_sends_pagination_vars() {
        let transport = FakeTransport::new(vec![serde_json::json!({
            "issues": {
                "nodes": [sample_issue_node("1")],
                "pageInfo": { "hasNextPage": true, "endCursor": "50" }
            }
        })]);
        let args = IssueArgs::default();
        let (issues, has_next, end) = fetch_with(&transport, &args, Some("0")).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].identifier, "ENG-1");
        assert!(has_next);
        assert_eq!(end.as_deref(), Some("50"));

        let vars = transport.variables(0);
        assert_eq!(vars["first"], serde_json::json!(50));
        assert_eq!(vars["after"], serde_json::json!("0"));
    }
}
