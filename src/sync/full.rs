use anyhow::Result;

use crate::db;
use crate::issues::list::fetch;
use crate::issues::{IssueArgs, SortField};

/// Fetch every page from the Linear API and upsert into SQLite.
/// Sets `sync_meta` key='`last_synced_at`' to the current UTC timestamp on success.
pub fn run() -> Result<()> {
    let conn = db::open_db(db::db_path()?)?;

    // Use a default IssueArgs with no filters and max page size.
    let args = IssueArgs {
        limit: 250,
        sort: SortField::Updated,
        desc: true,
        team: None,
        assignee: None,
        no_assignee: false,
        state: None,
        priority: None,
        created_after: None,
        created_before: None,
        updated_after: None,
        updated_before: None,
        title: None,
        live: false,
    };

    super::sync_pages(&conn, |after| fetch(&args, after))
}
