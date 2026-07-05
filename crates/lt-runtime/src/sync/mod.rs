pub mod delta;
pub mod drain;
pub mod full;
pub mod probe;
pub mod service;

use anyhow::Result;
use chrono::Utc;
use lt_storage::db;
use lt_storage::db::Upsert;
use lt_types::issues::{IssuesQuery, IssuesVariables};
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

/// Paginate an `IssuesQuery` refresh to exhaustion, upserting each page as it
/// arrives via [`IssuesQuery`]'s `Upsert` impl, then record the current UTC
/// time as `last_synced_at`.
///
/// `make_vars` builds one page's variables from the previous page's end
/// cursor (`None` for the first page); `full`/`delta` supply the filter.
fn sync_pages<F>(
    conn: &rusqlite::Connection,
    transport: &dyn GraphqlTransport,
    mut make_vars: F,
) -> Result<()>
where
    F: FnMut(Option<&str>) -> IssuesVariables,
{
    let mut cursor: Option<String> = None;
    loop {
        let vars = make_vars(cursor.as_deref());
        let page = execute::<IssuesQuery>(transport, vars.clone())?;
        IssuesQuery::upsert(conn, &vars, &page)?;

        if !page.page_info.has_next_page {
            break;
        }
        cursor = page.page_info.end_cursor;
    }

    let now = Utc::now().to_rfc3339();
    db::set_meta(conn, "last_synced_at", &now)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use lt_types::issues::sample_issue_node;
    use lt_upstream::client::FakeTransport;
    use serde_json::json;

    use super::*;

    fn page(
        nodes: &[serde_json::Value],
        has_next: bool,
        end_cursor: Option<&str>,
    ) -> serde_json::Value {
        json!({
            "issues": {
                "nodes": nodes,
                "pageInfo": { "hasNextPage": has_next, "endCursor": end_cursor }
            }
        })
    }

    fn plain_vars(after: Option<&str>) -> IssuesVariables {
        IssuesVariables {
            filter: None,
            sort: None,
            first: Some(250),
            after: after.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn sync_pages_upserts_each_page_and_paginates_to_exhaustion() {
        let conn = db::Database::memory().unwrap().connect().unwrap();
        let transport = FakeTransport::new(vec![
            page(&[sample_issue_node("1")], true, Some("cur")),
            page(&[sample_issue_node("2")], false, None),
        ]);

        sync_pages(&conn, &transport, plain_vars).unwrap();

        assert!(db::query_issue_by_id(&conn, "1").unwrap().is_some());
        assert!(db::query_issue_by_id(&conn, "2").unwrap().is_some());
        // The second request carried the first page's cursor.
        assert_eq!(transport.variables(1)["after"], json!("cur"));
    }

    #[test]
    fn sync_pages_records_last_synced_at() {
        let conn = db::Database::memory().unwrap().connect().unwrap();
        let transport = FakeTransport::new(vec![page(&[], false, None)]);

        sync_pages(&conn, &transport, plain_vars).unwrap();

        assert!(db::get_meta(&conn, "last_synced_at").unwrap().is_some());
    }
}
