pub mod comments;
pub mod delta;
pub mod full;
mod probe;

use std::io::Write;

use anyhow::Result;
use chrono::Utc;
use clap::Subcommand;

use crate::db;
use crate::linear::types::Issue;

/// Paginate through issue pages via `fetch_page`, upserting each page into the
/// local DB, then record the current UTC time as `last_synced_at`.
///
/// `fetch_page` is called with the current cursor and returns
/// `(issues, has_next_page, end_cursor)`.
fn sync_pages<F>(conn: &rusqlite::Connection, mut fetch_page: F) -> Result<()>
where
    F: FnMut(Option<&str>) -> Result<(Vec<Issue>, bool, Option<String>)>,
{
    let mut cursor: Option<String> = None;
    loop {
        let after = cursor.as_deref();
        let (issues, has_next, end_cursor) = fetch_page(after)?;

        if !issues.is_empty() {
            // Upsert fetched fragments into the normalized relational base.
            db::upsert_issues(conn, &issues)?;
        }

        if !has_next {
            break;
        }
        cursor = end_cursor;
    }

    let now = Utc::now().to_rfc3339();
    db::set_meta(conn, "last_synced_at", &now)?;

    Ok(())
}

#[derive(Subcommand)]
pub enum SyncCommands {
    /// Incremental sync: fetch issues updated since last sync (default)
    Delta,
    /// Fetch all issues from Linear and store them in the local SQLite cache
    Full,
    /// Probe the sync API to test whether a token is accepted
    Probe {
        /// Token to test instead of the stored OAuth token (e.g. a personal API key)
        #[arg(long)]
        token: Option<String>,
    },
}

pub fn run(out: &mut dyn Write, cmd: SyncCommands) -> Result<()> {
    match cmd {
        SyncCommands::Delta => {
            delta::run()?;
            writeln!(out, "Sync complete.")?;
            Ok(())
        }
        SyncCommands::Full => {
            full::run()?;
            writeln!(out, "Sync complete.")?;
            Ok(())
        }
        SyncCommands::Probe { token } => probe::run(out, token),
    }
}
