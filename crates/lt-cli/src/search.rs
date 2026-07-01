use std::io::Write;

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::issues::display::print_table;
use lt_storage::db;
use lt_types::types::Issue;

#[derive(Args, Clone)]
pub struct SearchArgs {
    /// Search query (FTS5 syntax: prefix `auth*`, phrase `"oauth token"`, AND of terms)
    pub query: String,

    /// Maximum number of results to return
    #[arg(long, default_value = "20")]
    pub limit: usize,

    /// Bypass local index and use Linear API search (not yet implemented)
    #[arg(long)]
    pub live: bool,
}

pub fn run(out: &mut dyn Write, args: &SearchArgs) -> Result<()> {
    if args.live {
        bail!("--live search via Linear API is not yet implemented");
    }

    let conn = db::open_db(db::db_path()?).context("failed to open local database")?;

    // Check whether any issues exist at all.
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM issues", [], |r| r.get(0))
        .context("failed to count issues")?;

    if total == 0 {
        bail!("Run 'lt sync' to build the local index first.");
    }

    // Check whether the FTS index has any content.
    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM issues_fts", [], |r| r.get(0))
        .unwrap_or(0);

    let note;

    let issues: Vec<Issue> = if fts_count == 0 {
        // FTS index is empty -- fall back to LIKE search on title.
        note = "Note: FTS index is empty or stale. Run 'lt sync full' to rebuild it. \
                Showing approximate results from title search."
            .to_string();
        db::search_issues_like(&conn, &args.query, args.limit)?
    } else {
        note = String::new();
        db::search_issues(&conn, &args.query, args.limit)?
    };

    print_table(out, &issues, &note)?;
    Ok(())
}
