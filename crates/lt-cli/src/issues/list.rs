use std::io::Write;

use anyhow::{Result, anyhow};
use chrono::Utc;
use lt_runtime::db;
use lt_runtime::query::IssueQuery;
use tracing::{error, info};

use super::IssueArgs;
use super::display::print_table;

/// Cache TTL in seconds (5 minutes).
const CACHE_TTL_SECS: i64 = 300;

/// Resolve `--assignee=me` to the viewer's name so the SQL filter can match the
/// joined assignee name. The viewer identity is persisted into `sync_meta` at
/// sync time (one viewer per database by definition), so this is a pure local
/// read with no network round-trip.
fn resolve_me(conn: &lt_runtime::db::Connection, query: &mut IssueQuery) -> Result<()> {
    let is_me = query
        .assignee
        .as_deref()
        .is_some_and(|a| a.eq_ignore_ascii_case("me"));
    if !is_me {
        return Ok(());
    }
    let name = db::get_meta(conn, "viewer_name")?
        .ok_or_else(|| anyhow!("`--assignee me` needs a synced viewer; run `lt sync` first"))?;
    query.assignee = Some(name);
    Ok(())
}

pub fn run(out: &mut dyn Write, args: &IssueArgs) -> Result<()> {
    let mut query = args.to_query();

    // --live: bypass cache entirely. The GraphQL filter resolves `me` itself.
    if args.live {
        let (issues, has_next_page, _) = lt_runtime::issues::fetch(&query, None)?;
        print_table(out, &issues, "")?;
        if has_next_page {
            writeln!(out, "\n+more issues")?;
        }
        return Ok(());
    }

    let conn = db::open_db(db::db_path()?)?;

    // Check last_synced_at from sync_meta.
    let last_synced_at = db::get_meta(&conn, "last_synced_at")?;

    match last_synced_at {
        None => {
            // Cache is empty (never synced). Run full sync first -- this also
            // persists the viewer identity that `resolve_me` reads below.
            info!("Cache empty -- running full sync...");
            drop(conn);
            lt_runtime::sync_cmd::full::run()?;
            // Re-open after sync.
            let conn2 = db::open_db(db::db_path()?)?;
            resolve_me(&conn2, &mut query)?;
            let issues = db::query_issues(&conn2, &query)?;
            print_table(out, &issues, "(cached)")?;
        }
        Some(ref ts) => {
            resolve_me(&conn, &mut query)?;

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
                    if let Err(e) = lt_runtime::sync_cmd::delta::run() {
                        error!("background sync error: {}", e);
                    }
                });
            }
        }
    }

    Ok(())
}
