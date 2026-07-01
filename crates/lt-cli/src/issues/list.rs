use std::io::Write;

use anyhow::Result;
use chrono::Utc;
use tracing::{error, info};

use lt_storage::db;
use lt_storage::query::IssueQuery;
use lt_sync::client::HttpTransport;
use lt_sync::list::fetch;

use super::IssueArgs;
use super::display::print_table;

/// Cache TTL in seconds (5 minutes).
const CACHE_TTL_SECS: i64 = 300;

/// Resolve `--assignee=me` to the viewer's actual name so the SQL filter can
/// match the joined assignee name. Uses the identity cached in `sync_meta`
/// when available, otherwise asks the Linear API and caches it.
fn resolve_me(conn: &rusqlite::Connection, query: &mut IssueQuery) -> Result<()> {
    let is_me = query
        .assignee
        .as_deref()
        .is_some_and(|a| a.eq_ignore_ascii_case("me"));
    if !is_me {
        return Ok(());
    }
    let name = if let Some(n) = db::get_meta(conn, "viewer_name")? {
        n
    } else {
        let token = lt_sync::auth::refresh::load_or_refresh_token()?;
        let viewer = lt_sync::viewer::fetch_viewer(&HttpTransport::new(token.access_token))?;
        db::set_meta(conn, "viewer_name", &viewer.name)?;
        viewer.name
    };
    query.assignee = Some(name);
    Ok(())
}

pub fn run(out: &mut dyn Write, args: &IssueArgs) -> Result<()> {
    let mut query = args.to_query();

    // --live: bypass cache entirely. The GraphQL filter resolves `me` itself.
    if args.live {
        let (issues, has_next_page, _) = fetch(&query, None)?;
        print_table(out, &issues, "")?;
        if has_next_page {
            writeln!(out, "\n+more issues")?;
        }
        return Ok(());
    }

    let conn = db::open_db(db::db_path()?)?;
    resolve_me(&conn, &mut query)?;

    // Check last_synced_at from sync_meta.
    let last_synced_at = db::get_meta(&conn, "last_synced_at")?;

    match last_synced_at {
        None => {
            // Cache is empty (never synced). Run full sync first.
            info!("Cache empty -- running full sync...");
            drop(conn);
            lt_sync::sync::full::run()?;
            // Re-open after sync.
            let conn2 = db::open_db(db::db_path()?)?;
            let issues = db::query_issues(&conn2, &query)?;
            print_table(out, &issues, "(cached)")?;
        }
        Some(ref ts) => {
            // Parse the timestamp and check age.
            let age_secs: i64 = chrono::DateTime::parse_from_rfc3339(ts).map_or(i64::MAX, |t| {
                Utc::now().signed_duration_since(t).num_seconds()
            });

            if age_secs < CACHE_TTL_SECS {
                // Fresh cache -- serve immediately.
                let issues = db::query_issues(&conn, &query)?;
                let note = format!("(cached, age {age_secs}s)");
                print_table(out, &issues, &note)?;
            } else {
                // Stale cache -- serve immediately, then delta sync in background.
                let issues = db::query_issues(&conn, &query)?;
                let note = format!("(stale cache, age {age_secs}s -- syncing in background)");
                print_table(out, &issues, &note)?;

                std::thread::spawn(|| {
                    if let Err(e) = lt_sync::sync::delta::run() {
                        error!("background sync error: {}", e);
                    }
                });
            }
        }
    }

    Ok(())
}
