//! Viewer identity persistence: the authenticated user's row in `users`, and
//! their organization's row in `organizations`, linked from `sync_meta` by id
//! rather than duplicating the name/organization fields directly.

use anyhow::{Context, Result};
use lt_types::types;
use lt_types::viewer::{Organization, Viewer, ViewerQuery};
use rusqlite::{Connection, params};

use crate::db::issues::{get_meta, set_meta, upsert_named_entity};
use crate::db::ops::{EntityKey, Read, Upsert};
use crate::db::sql::{self, EntityTable};

/// The `sync_meta` keys the viewer identity is stored under. Kept private so
/// every reader/writer goes through [`viewer`] / [`set_viewer`] instead of
/// the raw key strings.
const VIEWER_ID_KEY: &str = "viewer_id";
const ORGANIZATION_ID_KEY: &str = "organization_id";

/// Upsert the viewer's user row, their organization's row, then record both
/// ids in `sync_meta` -- the keys [`viewer`] reads back to reconstruct the
/// identity, rather than duplicating its name/organization fields.
pub fn set_viewer(conn: &Connection, viewer: &Viewer) -> Result<()> {
    upsert_named_entity(
        conn,
        EntityTable::Users,
        viewer.user.id.inner(),
        Some(&viewer.user.name),
    )?;
    sql::execute(
        conn,
        sql::UPSERT_ORGANIZATION,
        params![
            viewer.organization.id.inner(),
            viewer.organization.name,
            viewer.organization.url_key,
        ],
        "upsert organization",
    )?;
    set_meta(conn, VIEWER_ID_KEY, viewer.user.id.inner())?;
    set_meta(conn, ORGANIZATION_ID_KEY, viewer.organization.id.inner())?;
    Ok(())
}

/// A single `users` row's name by id, or `None` if it is not (yet) recorded.
fn query_user_name(conn: &Connection, id: &str) -> Result<Option<String>> {
    let mut stmt = sql::prepare(conn, sql::QUERY_USER_BY_ID)
        .context("failed to prepare user query statement")?;
    stmt.query_map(params![id], |row| row.get("name"))
        .context("failed to execute user query")?
        .next()
        .transpose()
        .context("failed to read user row")
}

/// A single `organizations` row by id, or `None` if it is not (yet) recorded.
fn query_organization(conn: &Connection, id: &str) -> Result<Option<Organization>> {
    Ok(crate::db::query_rows_id_name_and(
        conn,
        (sql::QUERY_ORGANIZATION_BY_ID, "url_key"),
        params![id],
        |id, name, url_key| Organization {
            id: id.into(),
            name,
            url_key,
        },
    )?
    .into_iter()
    .next())
}

/// Look up the persisted viewer identity by resolving its `sync_meta`-stored
/// ids against `users`/`organizations`. `None` when sync has not yet recorded
/// one (pre-first-sync), or -- never expected, since both rows are written
/// together in [`set_viewer`] -- a stored id whose row is gone.
pub fn viewer(conn: &Connection) -> Result<Option<Viewer>> {
    let Some(viewer_id) = get_meta(conn, VIEWER_ID_KEY)? else {
        return Ok(None);
    };
    let Some(organization_id) = get_meta(conn, ORGANIZATION_ID_KEY)? else {
        return Ok(None);
    };
    let Some(name) = query_user_name(conn, &viewer_id)? else {
        return Ok(None);
    };
    let Some(organization) = query_organization(conn, &organization_id)? else {
        return Ok(None);
    };

    Ok(Some(Viewer {
        user: types::User {
            id: viewer_id.into(),
            name,
        },
        organization,
    }))
}

impl Read for ViewerQuery {
    fn read(conn: &Connection, _vars: &Self::Variables) -> Result<Self::Output> {
        viewer(conn)
    }

    fn reads(_vars: &Self::Variables) -> Vec<EntityKey> {
        vec![EntityKey::Viewer]
    }
}

impl Upsert for ViewerQuery {
    fn upsert(
        conn: &Connection,
        _vars: &Self::Variables,
        out: &Self::Output,
    ) -> Result<Vec<EntityKey>> {
        let Some(viewer) = out else {
            return Ok(Vec::new());
        };
        set_viewer(conn, viewer)?;
        Ok(vec![EntityKey::Viewer])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        crate::db::Database::memory().unwrap().connect().unwrap()
    }

    fn ada() -> Viewer {
        Viewer {
            user: types::User {
                id: "u1".into(),
                name: "Ada".to_string(),
            },
            organization: Organization {
                id: "o1".into(),
                name: "Acme".to_string(),
                url_key: "acme".to_string(),
            },
        }
    }

    #[test]
    fn viewer_round_trips_identity_and_organization() {
        let conn = test_db();
        assert!(viewer(&conn).unwrap().is_none());

        set_viewer(&conn, &ada()).unwrap();
        let viewer = viewer(&conn).unwrap().unwrap();
        assert_eq!(viewer.user.id.inner(), "u1");
        assert_eq!(viewer.user.name, "Ada");
        assert_eq!(viewer.organization.id.inner(), "o1");
        assert_eq!(viewer.organization.name, "Acme");
        assert_eq!(viewer.organization.url_key, "acme");
    }

    #[test]
    fn set_viewer_does_not_duplicate_name_and_organization_into_sync_meta() {
        let conn = test_db();
        set_viewer(&conn, &ada()).unwrap();

        assert_eq!(
            get_meta(&conn, VIEWER_ID_KEY).unwrap().as_deref(),
            Some("u1")
        );
        assert_eq!(
            get_meta(&conn, ORGANIZATION_ID_KEY).unwrap().as_deref(),
            Some("o1")
        );
        assert!(get_meta(&conn, "viewer_name").unwrap().is_none());
        assert!(get_meta(&conn, "viewer_org_name").unwrap().is_none());
    }

    #[test]
    fn viewer_query_reads_only_the_viewer_key() {
        assert_eq!(ViewerQuery::reads(&()), vec![EntityKey::Viewer]);
    }

    #[test]
    fn viewer_query_read_is_none_before_any_sync() {
        let conn = test_db();
        assert!(ViewerQuery::read(&conn, &()).unwrap().is_none());
    }

    #[test]
    fn viewer_query_upsert_persists_and_reports_viewer() {
        let conn = test_db();
        let out = Some(ada());
        let touched = ViewerQuery::upsert(&conn, &(), &out).unwrap();
        assert_eq!(touched, vec![EntityKey::Viewer]);
        assert_eq!(
            ViewerQuery::read(&conn, &()).unwrap().unwrap().user.name,
            "Ada"
        );
    }

    #[test]
    fn viewer_query_upsert_of_none_is_a_noop() {
        let conn = test_db();
        assert!(ViewerQuery::upsert(&conn, &(), &None).unwrap().is_empty());
    }
}
