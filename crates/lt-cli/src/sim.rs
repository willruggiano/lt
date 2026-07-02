//! The `lt sim` command: seed the local database from the deterministic
//! generator in `lt-storage` (reached via the runtime seam; reused by tests).

use std::io::Write;

use anyhow::Result;
use chrono::Utc;
use clap::Args;
use lt_runtime::db;
use lt_runtime::sim::generate;

/// Knobs for `lt sim`.
#[derive(Args)]
pub struct SimArgs {
    /// RNG seed; the same seed always produces the same dataset.
    #[arg(long, default_value_t = 0)]
    pub seed: u64,
    /// Number of issues to generate.
    #[arg(long, default_value_t = 100)]
    pub size: usize,
}

/// Generate a dataset and write it into the active profile's local database.
///
/// Marks the cache fresh so the offline list/TUI serve the generated data
/// without attempting a network sync, and records a `viewer_name` (a real
/// assignee from the dataset) so the `--assignee=me` filter resolves.
pub fn run(out: &mut dyn Write, args: &SimArgs) -> Result<()> {
    let dataset = generate(args.seed, args.size);
    let conn = db::open_db(db::db_path()?)?;
    db::upsert_issues(&conn, &dataset.issues)?;
    for (issue_id, comment) in &dataset.comments {
        db::upsert_comments(&conn, issue_id, std::slice::from_ref(comment))?;
    }
    db::set_meta(&conn, "last_synced_at", &Utc::now().to_rfc3339())?;
    if let Some(name) = dataset
        .issues
        .iter()
        .find_map(|i| i.assignee.as_ref().map(|u| u.name.clone()))
    {
        db::set_meta(&conn, "viewer_name", &name)?;
    }
    writeln!(
        out,
        "Seeded {} issues and {} comments (seed={}, size={}).",
        dataset.issues.len(),
        dataset.comments.len(),
        args.seed,
        args.size
    )?;
    Ok(())
}
