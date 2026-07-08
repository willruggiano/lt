use anyhow::Result;
use lt_upstream::query::issues::{IssueSort, IssuesVariables};
use lt_upstream::query::{SortDirection, SortField};
use lt_upstream::transport::Transport;

/// Fetch every page from the Linear API and upsert into SQLite over `conn`,
/// using `transport` for every request. Sets `sync_meta`
/// key='`last_synced_at`' to the current UTC timestamp on success.
pub fn run(conn: &rusqlite::Connection, transport: &dyn Transport) -> Result<()> {
    // Drain queued local mutations before re-fetching the world.
    super::drain::drain(conn, transport)?;
    // Persist the viewer so cached reads can resolve `me` offline.
    super::persist_viewer(conn, transport)?;
    // Teams, then every workflow state across every team, before any issue
    // page: an issue's `state_id` must already be locally known.
    super::sync_reference_data(conn, transport)?;

    // No filter, max page size.
    super::sync_pages(conn, transport, |after| IssuesVariables {
        filter: None,
        sort: Some(IssueSort {
            field: SortField::Updated,
            direction: SortDirection::Descending,
        }),
        first: Some(250),
        after: after.map(ToOwned::to_owned),
    })
}
