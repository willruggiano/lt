pub mod delta;
pub mod drain;
pub mod full;
pub mod probe;

use anyhow::Result;
use chrono::Utc;
use lt_storage::db;
use lt_types::types::Issue;
use lt_upstream::client::GraphqlTransport;

/// Persist the authenticated viewer's identity into `sync_meta` so cached reads
/// can resolve `me` without a network round-trip. A database tracks exactly one
/// viewer by definition, so this is an upsert of a stable identity.
fn persist_viewer(conn: &rusqlite::Connection, transport: &dyn GraphqlTransport) -> Result<()> {
    let viewer = lt_upstream::viewer::fetch(transport)?;
    db::set_meta(conn, "viewer_id", viewer.id.inner())?;
    db::set_meta(conn, "viewer_name", &viewer.name)?;
    Ok(())
}

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
