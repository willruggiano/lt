use std::io::Write;

use anyhow::{Result, anyhow};
use chrono::Utc;
use lt_runtime::{db, load};
use lt_types::issues::{AssigneeFilter, IssueFilter, IssueSort, IssuesQuery, IssuesVariables};
use tracing::info;

use super::IssueArgs;
use super::display::print_table;

/// Lower `args` into the typed variables shared by the cached and `--live`
/// reads, resolving `--assignee=me` against the persisted viewer identity
/// (`lt sync` populates it); `--live` shares this local resolution rather
/// than deferring to a server-side filter.
fn lower(args: &IssueArgs, conn: &db::Connection) -> Result<IssuesVariables> {
    let mut filter = args.literal_filter()?;
    if let Some(AssigneeFilter::Contains(value)) = &filter.assignee
        && value.eq_ignore_ascii_case("me")
    {
        let name = db::viewer(conn)?
            .ok_or_else(|| anyhow!("`--assignee me` needs a synced viewer; run `lt sync` first"))?
            .name;
        filter.assignee = Some(AssigneeFilter::Exact(name));
    }
    let filter = (filter != IssueFilter::default()).then_some(filter);
    let sort = Some(IssueSort {
        field: args.sort.clone(),
        direction: args.sort_direction(),
    });
    Ok(IssuesVariables {
        filter,
        sort,
        first: Some(i32::try_from(args.limit.min(250)).unwrap_or(250)),
        after: None,
    })
}

pub fn run(out: &mut dyn Write, args: &IssueArgs) -> Result<()> {
    let conn = db::open_db(db::db_path()?)?;

    // --live: bypass cache entirely.
    if args.live {
        let vars = lower(args, &conn)?;
        let page = lt_runtime::issues::fetch(vars)?;
        print_table(out, &page.nodes, "")?;
        if page.page_info.has_next_page {
            writeln!(out, "\n+more issues")?;
        }
        return Ok(());
    }

    // Check last_synced_at from sync_meta.
    let last_synced_at = db::get_meta(&conn, "last_synced_at")?;

    match last_synced_at {
        None => {
            // Cache is empty (never synced). Run full sync first -- this also
            // persists the viewer identity that `resolve_assignee` reads below.
            info!("Cache empty -- running full sync...");
            drop(conn);
            let (sync_conn, transport) = lt_runtime::sync::open_production()?;
            lt_runtime::sync::full::run(&sync_conn, transport.as_ref())?;
            // Re-open after sync.
            let conn2 = db::open_db(db::db_path()?)?;
            let vars = lower(args, &conn2)?;
            let page = load::<IssuesQuery>(&conn2, &vars)?;
            print_table(out, &page.nodes, "(cached)")?;
        }
        Some(ref ts) => {
            let vars = lower(args, &conn)?;

            // Parse the timestamp and check age.
            let age_secs: i64 = chrono::DateTime::parse_from_rfc3339(ts).map_or(i64::MAX, |t| {
                Utc::now().signed_duration_since(t).num_seconds()
            });

            let page = load::<IssuesQuery>(&conn, &vars)?;
            let note = format!("(cached, age {age_secs}s)");
            print_table(out, &page.nodes, &note)?;
        }
    }

    Ok(())
}
