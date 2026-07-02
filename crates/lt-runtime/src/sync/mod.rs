pub mod delta;
pub mod drain;
pub mod full;
pub mod probe;
pub mod service;

use anyhow::Result;
use chrono::Utc;
use lt_storage::db;
use lt_types::issues::IssueConnection;
use lt_types::viewer::ViewerQuery;
use lt_upstream::client::{GraphqlTransport, execute};

/// Persist the authenticated viewer's identity into `sync_meta` so cached reads
/// can resolve `me` without a network round-trip. A database tracks exactly one
/// viewer by definition, so this is an upsert of a stable identity.
fn persist_viewer(conn: &rusqlite::Connection, transport: &dyn GraphqlTransport) -> Result<()> {
    let viewer = execute::<ViewerQuery>(transport, ())?;
    db::set_synced_viewer(conn, viewer.id.inner(), &viewer.name)?;
    Ok(())
}

/// Paginate through issue pages via `fetch_page`, upserting each page into the
/// local DB, then record the current UTC time as `last_synced_at`.
///
/// `fetch_page` is called with the current cursor and returns the next page.
fn sync_pages<F>(conn: &rusqlite::Connection, mut fetch_page: F) -> Result<()>
where
    F: FnMut(Option<&str>) -> Result<IssueConnection>,
{
    let mut cursor: Option<String> = None;
    loop {
        let after = cursor.as_deref();
        let page = fetch_page(after)?;

        if !page.nodes.is_empty() {
            db::upsert_issues(conn, &page.nodes)?;
        }

        if !page.page_info.has_next_page {
            break;
        }
        cursor = page.page_info.end_cursor;
    }

    let now = Utc::now().to_rfc3339();
    db::set_meta(conn, "last_synced_at", &now)?;

    Ok(())
}
