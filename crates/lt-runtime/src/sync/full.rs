use anyhow::Result;
use lt_storage::db::EntityKey;
use lt_types::issues::{IssueSort, IssuesVariables};
use lt_types::query::{SortDirection, SortField};
use lt_upstream::client::GraphqlTransport;

/// Fetch every page from the Linear API and upsert into SQLite over `conn`,
/// using `transport` for every request. Sets `sync_meta`
/// key='`last_synced_at`' to the current UTC timestamp on success, and
/// returns the union of entity keys the sync touched
/// (docs/design/operation-seam-adr.md, "Decision 5").
pub fn run(
    conn: &rusqlite::Connection,
    transport: &dyn GraphqlTransport,
) -> Result<Vec<EntityKey>> {
    // Drain queued local mutations before re-fetching the world.
    let mut touched = super::drain::drain(conn, transport)?;
    // Persist the viewer so cached reads can resolve `me` offline.
    touched.extend(super::persist_viewer(conn, transport)?);
    // Teams, then every workflow state across every team, before any issue
    // page: an issue's `state_id` must already be locally known.
    touched.extend(super::sync_reference_data(conn, transport)?);

    // No filter, max page size.
    touched.extend(super::sync_pages(conn, transport, |after| {
        IssuesVariables {
            filter: None,
            sort: Some(IssueSort {
                field: SortField::Updated,
                direction: SortDirection::Descending,
            }),
            first: Some(250),
            after: after.map(ToOwned::to_owned),
        }
    })?);
    Ok(touched)
}
