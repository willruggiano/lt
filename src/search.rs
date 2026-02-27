use anyhow::{Context, Result, bail};
use clap::Args;

use crate::db;
use crate::issues::display::print_table_cached;

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

pub fn run(args: SearchArgs) -> Result<()> {
    if args.live {
        bail!("--live search via Linear API is not yet implemented");
    }

    let conn = db::open_db().context("failed to open local database")?;

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
    let issues: Vec<db::Issue>;

    if fts_count == 0 {
        // FTS index is empty -- fall back to LIKE search on title.
        note = "Note: FTS index is empty or stale. Run 'lt sync full' to rebuild it. \
                Showing approximate results from title search."
            .to_string();
        let like_pattern = format!("%{}%", args.query);
        let sql = format!(
            "SELECT id, identifier, title, priority_label, state_name,
                    assignee_name, team_name, team_key, created_at, updated_at, synced_at,
                    description, labels, project_name, cycle_name, creator_name
             FROM issues
             WHERE title LIKE ?1
             LIMIT {}",
            args.limit
        );
        let mut stmt = conn
            .prepare(&sql)
            .context("failed to prepare fallback search statement")?;
        let rows = stmt
            .query_map([&like_pattern], |row| {
                Ok(db::Issue {
                    id: row.get(0)?,
                    identifier: row.get(1)?,
                    title: row.get(2)?,
                    priority_label: row.get(3)?,
                    state_name: row.get(4)?,
                    assignee_name: row.get(5)?,
                    team_name: row.get(6)?,
                    team_key: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                    synced_at: row.get(10)?,
                    description: row.get(11)?,
                    labels: row.get::<_, Option<String>>(12)?.unwrap_or_default(),
                    project_name: row.get(13)?,
                    cycle_name: row.get(14)?,
                    creator_name: row.get(15)?,
                })
            })
            .context("failed to execute fallback search")?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row.context("failed to read fallback row")?);
        }
        issues = result;
    } else {
        note = String::new();
        let mut all = db::search_issues(&conn, &args.query)?;
        all.truncate(args.limit);
        issues = all;
    }

    print_table_cached(&issues, &note);
    Ok(())
}
