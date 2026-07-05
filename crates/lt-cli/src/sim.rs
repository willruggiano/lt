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
/// without attempting a network sync, and records the synced viewer identity
/// (a real assignee from the dataset) so the `--assignee=me` filter resolves.
pub fn run(out: &mut dyn Write, args: &SimArgs) -> Result<()> {
    let dataset = generate(args.seed, args.size);
    let conn = db::open_db(db::db_path()?)?;
    db::upsert_issues(&conn, &dataset.issues)?;
    db::upsert_comments(&conn, &dataset.comments)?;
    // No team-membership API to seed from offline: derive it from the
    // seeded issues' team/assignee and team/creator pairs (ADR "Sim
    // compatibility").
    db::derive_team_memberships_from_issues(&conn)?;
    db::set_meta(&conn, "last_synced_at", &Utc::now().to_rfc3339())?;
    if let Some(assignee) = dataset.issues.iter().find_map(|i| i.assignee.clone()) {
        // `lt sim` has no organization concept to seed; the identity itself
        // is real (a real assignee from the dataset).
        db::set_viewer(
            &conn,
            &lt_types::viewer::User {
                id: assignee.id,
                name: assignee.name,
                organization: lt_types::viewer::Organization {
                    id: String::new().into(),
                    name: String::new(),
                    url_key: String::new(),
                },
            },
        )?;
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
