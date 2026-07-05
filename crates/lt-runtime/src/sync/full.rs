use anyhow::Result;
use lt_storage::db;
use lt_types::issues::{IssueSort, IssuesVariables};
use lt_types::query::SortField;
use lt_upstream::client::HttpTransport;

/// Fetch every page from the Linear API and upsert into SQLite.
/// Sets `sync_meta` key='`last_synced_at`' to the current UTC timestamp on success.
pub fn run() -> Result<()> {
    let conn = db::open_db(db::db_path()?)?;

    let token = lt_upstream::auth::refresh::load_or_refresh_token()?;
    let transport = HttpTransport::new(token.access_token);

    // Drain queued local mutations before re-fetching the world.
    super::drain::drain(&conn, &transport)?;
    // Persist the viewer so cached reads can resolve `me` offline.
    super::persist_viewer(&conn, &transport)?;

    // No filter, max page size.
    super::sync_pages(&conn, &transport, |after| IssuesVariables {
        filter: None,
        sort: Some(IssueSort {
            field: SortField::Updated,
            desc: true,
        }),
        first: Some(250),
        after: after.map(ToOwned::to_owned),
    })
}
