pub mod comments;
pub mod crud;
pub mod filters;
#[cfg(test)]
mod generated_sql_tests;
pub mod issues;
pub mod op_log;
pub(crate) mod sql;
pub mod teams;
pub mod viewer;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
pub use comments::{delete_comments_for_issue, query_comments, upsert_comments};
pub use crud::{Delete, Insert, Select, Update};
pub use issues::{
    count_fts_rows, count_issues, get_meta, issue_is_locally_unsynced, query_children,
    query_issue_by_id, query_issues, search_issues, search_issues_like, set_meta, upsert_issues,
};
pub use rusqlite::Connection;
pub use teams::{
    derive_team_memberships_from_issues, query_team_members, query_team_states, query_teams,
    replace_team_memberships, upsert_team_state, upsert_teams, upsert_users,
};
pub use viewer::{set_viewer, viewer};

/// Parse a stored RFC3339 timestamp column into the wire
/// [`DateTime`](lt_upstream::query::scalars::DateTime) scalar via its
/// `FromStr` impl. Storage always writes
/// [`DateTime::to_rfc3339_millis`](lt_upstream::query::scalars::DateTime::to_rfc3339_millis),
/// so a parse failure here means the row is corrupt; surface it as a
/// `rusqlite` error rather than silently defaulting.
pub(crate) fn parse_datetime_column(
    s: &str,
) -> std::result::Result<lt_upstream::query::scalars::DateTime, rusqlite::types::FromSqlError> {
    s.parse()
        .map_err(|e| rusqlite::types::FromSqlError::Other(Box::new(e)))
}

/// Run a statement with `params`, mapping every `(id, name, <extra>)`-shaped
/// result row through `ctor` -- the shape behind both a team-scoped workflow
/// state (`position` as its extra column) and an organization (`url_key`), so
/// the two near-identical row-mapping call sites share one body. `query` is
/// `(statement, extra_column)`, grouped so the function stays under clippy's
/// too-many-arguments threshold.
pub(crate) fn query_rows_id_name_and<T, E: rusqlite::types::FromSql>(
    conn: &Connection,
    query: (sql::Sql, &str),
    params: impl rusqlite::Params,
    ctor: impl Fn(String, String, E) -> T,
) -> Result<Vec<T>> {
    let (stmt_sql, extra_column) = query;
    let mut stmt = sql::prepare(conn, stmt_sql).context("failed to prepare statement")?;
    let rows = stmt
        .query_map(params, |row| {
            Ok(ctor(
                row.get("id")?,
                row.get("name")?,
                row.get(extra_column)?,
            ))
        })
        .context("failed to execute query")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.context("failed to read row")?);
    }
    Ok(out)
}

pub fn db_path() -> Result<PathBuf> {
    let data_dir = dirs::data_local_dir().context("could not determine local data directory")?;
    // Each profile gets its own database so accounts/workspaces never share
    // state and can run concurrently.
    let lt_dir = lt_config::profile_dir(&data_dir.join("lt"));
    fs::create_dir_all(&lt_dir)
        .with_context(|| format!("could not create directory: {}", lt_dir.display()))?;
    Ok(lt_dir.join("lt.db"))
}

include!(concat!(env!("OUT_DIR"), "/generated_schema.rs"));

/// The `sync_meta` key the generated DDL's hash is stamped under: `open_db`
/// compares it against the current build's hash so a binary upgrade whose
/// generated schema changed rebuilds the cache instead of running against a
/// stale one (the "disposable replica" open path).
const SCHEMA_HASH_KEY: &str = "schema_hash";

/// A stable hash of [`GENERATED_DDL`], recomputed on every open (not stored
/// in source): any change to the generated schema changes this value, which
/// [`migrate_schema`] compares against whatever is stamped into the database
/// it opens.
fn schema_hash() -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    GENERATED_DDL.hash(&mut hasher);
    hasher.finish().to_string()
}

/// Apply every generated DDL statement to a schema-less connection, in
/// dependency order.
fn apply_schema(conn: &Connection) -> Result<()> {
    for ddl in GENERATED_DDL {
        conn.execute_batch(ddl)
            .context("failed to apply generated schema DDL")?;
    }
    Ok(())
}

/// Whether a table named `name` exists -- used to detect a brand-new
/// database, where not even `sync_meta` exists yet to hold the schema hash.
fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |row| row.get(0),
        )
        .context("failed to check for an existing table")?;
    Ok(count > 0)
}

/// Drop every table the database currently has (dropping a table also drops
/// its triggers and indexes), then reapply the current generated schema from
/// scratch. Foreign key enforcement is suspended for the drop: the generated
/// tables reference each other, and respecting that graph while dropping is
/// unnecessary once enforcement is off.
///
/// Called only on a schema-hash mismatch (a binary upgrade whose generated
/// schema changed since this database was last opened) -- the "disposable
/// replica" open path. Whatever `op_log` rows are pending at that point are
/// dropped with the rest of the cache; draining them to the server first, so
/// a schema change never loses an un-synced local edit, is a sync-thread
/// concern outside this crate, wired in a later task.
fn rebuild_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA foreign_keys = OFF;")
        .context("failed to disable foreign keys for schema rebuild")?;

    let names: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            )
            .context("failed to list existing tables")?;
        let rows = stmt
            .query_map([], |row| row.get(0))
            .context("failed to query existing tables")?;
        let mut names = Vec::new();
        for row in rows {
            names.push(row.context("failed to read table name")?);
        }
        names
    };

    for name in names {
        conn.execute(&format!("DROP TABLE IF EXISTS \"{name}\""), [])
            .with_context(|| format!("failed to drop table {name}"))?;
    }

    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .context("failed to re-enable foreign keys after schema rebuild")?;

    apply_schema(conn)
}

/// Ensure `conn`'s schema matches the current build's generated DDL: apply it
/// fresh on an empty database, or rebuild it if the hash stamped by a
/// previous open no longer matches (a binary upgrade, or a database that
/// predates this scheme entirely). A matching hash is a no-op.
fn migrate_schema(conn: &Connection) -> Result<()> {
    let hash = schema_hash();
    if !table_exists(conn, "sync_meta")? {
        apply_schema(conn)?;
        issues::set_meta(conn, SCHEMA_HASH_KEY, &hash)?;
        return Ok(());
    }
    if issues::get_meta(conn, SCHEMA_HASH_KEY)?.as_deref() != Some(hash.as_str()) {
        rebuild_schema(conn)?;
        issues::set_meta(conn, SCHEMA_HASH_KEY, &hash)?;
    }
    Ok(())
}

/// Open a connection to the SQLite database at `uri` -- a filesystem path or a
/// `file:...?mode=memory` URI -- and ensure its schema is current.
pub fn open_db(uri: impl AsRef<Path>) -> Result<Connection> {
    let uri = uri.as_ref();
    let conn = Connection::open(uri)
        .with_context(|| format!("could not open database at {}", uri.display()))?;
    // The TUI and the CLI (e.g. `lt sync`) can open the same per-profile file
    // concurrently; wait out a contending writer instead of failing
    // immediately with SQLITE_BUSY.
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .context("failed to set busy timeout")?;
    conn.pragma_update(None, "foreign_keys", true)
        .context("failed to enable foreign key enforcement")?;
    migrate_schema(&conn)?;
    Ok(conn)
}

/// The connection-acquisition seam every module in this crate uses: [`Sqlite`]
/// opens the per-profile file on disk; [`Memory`] (test-only) opens an
/// isolated, shared-cache in-memory database. Every function elsewhere in
/// `db` already takes `&Connection`, so the trait needs to abstract only
/// acquiring one.
pub trait Storage {
    /// Open a fresh connection to this database.
    fn connect(&self) -> Result<Connection>;
}

/// The SQLite file on disk. Resolving the path and ensuring the schema is
/// current is deferred to `connect()`, so constructing this does no I/O.
pub struct Sqlite;

impl Storage for Sqlite {
    fn connect(&self) -> Result<Connection> {
        open_db(db_path()?)
    }
}

/// An isolated, shared-cache in-memory database for tests. SQLite destroys a
/// shared-cache in-memory database when its last connection closes, so the
/// handle holds one open connection for its own lifetime.
#[cfg(any(test, feature = "test-util"))]
pub struct Memory {
    uri: String,
    _keepalive: Connection,
}

#[cfg(any(test, feature = "test-util"))]
impl Memory {
    /// Build an isolated in-memory database, schema-current and ready. Each
    /// call gets a distinct shared cache so concurrent tests never share
    /// state.
    pub fn new() -> Result<Self> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let uri = format!("file:lt_memdb_{n}?mode=memory&cache=shared");
        let keepalive = open_db(&uri)?;
        Ok(Self {
            uri,
            _keepalive: keepalive,
        })
    }

    /// Open another handle onto the same database: a second keepalive
    /// connection on the same shared-cache URI, so a second owner (e.g. a
    /// test's `Runtime`) reads and writes the exact rows the first sees.
    /// Neither handle's lifetime depends on the other's.
    pub fn share(&self) -> Result<Self> {
        let keepalive = open_db(&self.uri)?;
        Ok(Self {
            uri: self.uri.clone(),
            _keepalive: keepalive,
        })
    }
}

#[cfg(any(test, feature = "test-util"))]
impl Storage for Memory {
    fn connect(&self) -> Result<Connection> {
        open_db(&self.uri)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_issue_by_id_resolves_and_misses() {
        let db = Memory::new().unwrap();
        let conn = db.connect().unwrap();
        // `sample_base_issue`'s state must already be locally known (sync
        // owns workflow states; issue upserts never write them).
        teams::upsert_team_state(
            &conn,
            "ENG",
            &lt_upstream::query::types::WorkflowState {
                id: "s-todo".into(),
                name: "Todo".to_string(),
                position: 1.0,
            },
        )
        .unwrap();
        upsert_issues(&conn, &[op_log::sample_base_issue("9")]).unwrap();

        let found = query_issue_by_id(&conn, "9").unwrap().unwrap();
        assert_eq!(found.identifier, "ENG-9");
        assert_eq!(found.title, "issue 9");
        assert_eq!(found.state.name, "Todo");

        assert!(query_issue_by_id(&conn, "absent").unwrap().is_none());
    }

    #[test]
    fn fresh_database_applies_schema_and_stamps_the_hash() {
        let db = Memory::new().unwrap();
        let conn = db.connect().unwrap();
        assert_eq!(
            issues::get_meta(&conn, SCHEMA_HASH_KEY).unwrap().as_deref(),
            Some(schema_hash().as_str())
        );
    }

    /// A stamped hash that no longer matches the generated DDL (simulating a
    /// binary upgrade, or a database that predates this scheme) must drop
    /// the previous cache and rebuild it from scratch, re-stamping the
    /// current hash.
    #[test]
    fn schema_hash_mismatch_rebuilds_the_cache_and_drops_the_previous_data() {
        let db = Memory::new().unwrap();
        let conn = db.connect().unwrap();
        teams::upsert_team_state(
            &conn,
            "ENG",
            &lt_upstream::query::types::WorkflowState {
                id: "s-todo".into(),
                name: "Todo".to_string(),
                position: 1.0,
            },
        )
        .unwrap();
        upsert_issues(&conn, &[op_log::sample_base_issue("9")]).unwrap();
        assert!(query_issue_by_id(&conn, "9").unwrap().is_some());

        issues::set_meta(&conn, SCHEMA_HASH_KEY, "stale-hash").unwrap();
        drop(conn);

        let rebuilt = open_db(&db.uri).unwrap();
        assert!(query_issue_by_id(&rebuilt, "9").unwrap().is_none());
        assert_eq!(
            issues::get_meta(&rebuilt, SCHEMA_HASH_KEY)
                .unwrap()
                .as_deref(),
            Some(schema_hash().as_str())
        );
    }
}
