use anyhow::Result;
use lt_storage::db;
use lt_storage::query::{IssueQuery, SortField};

use crate::client::HttpTransport;
use crate::list::fetch;

/// Fetch every page from the Linear API and upsert into SQLite.
/// Sets `sync_meta` key='`last_synced_at`' to the current UTC timestamp on success.
pub fn run() -> Result<()> {
    let conn = db::open_db(db::db_path()?)?;

    // Drain queued local mutations before re-fetching the world.
    let token = crate::auth::refresh::load_or_refresh_token()?;
    super::drain::drain(&conn, &HttpTransport::new(token.access_token))?;

    // Use a default query with no filters and max page size.
    let args = IssueQuery {
        limit: 250,
        sort: SortField::Updated,
        desc: true,
        ..IssueQuery::default()
    };

    super::sync_pages(&conn, |after| fetch(&args, after))
}
