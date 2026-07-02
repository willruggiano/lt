pub mod comments;
pub mod filters;
pub mod issues;
pub mod outbox;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
pub use comments::{delete_comments_for_issue, query_comments, upsert_comments};
pub(crate) use issues::{ISSUE_COLUMNS, ISSUE_JOINS, issue_from_row};
pub use issues::{
    get_meta, query_children, query_issue_by_id, query_issues, query_issues_page, search_issues,
    search_issues_like, set_meta, set_synced_viewer, synced_viewer, upsert_issues,
};
pub use rusqlite::Connection;
use rusqlite::Params;

/// Parse a stored RFC3339 timestamp column into the wire [`DateTime`](lt_types::scalars::DateTime)
/// scalar via its `FromStr` impl. Storage always writes
/// [`DateTime::to_rfc3339_millis`](lt_types::scalars::DateTime::to_rfc3339_millis),
/// so a parse failure here means the row is corrupt; surface it as a
/// `rusqlite` error rather than silently defaulting.
pub(crate) fn parse_datetime_column(
    s: &str,
) -> std::result::Result<lt_types::scalars::DateTime, rusqlite::types::FromSqlError> {
    s.parse()
        .map_err(|e| rusqlite::types::FromSqlError::Other(Box::new(e)))
}

/// Run a parameterized write statement, attaching `what` to any error.
///
/// `what` reads as the failed action, e.g. `"set sync_meta"`.
pub(crate) fn execute(conn: &Connection, sql: &str, params: impl Params, what: &str) -> Result<()> {
    conn.execute(sql, params)
        .with_context(|| format!("failed to {what}"))?;
    Ok(())
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

/// Whether `column` exists on `table`.
fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name=?2",
        rusqlite::params![table, column],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
        > 0
}

/// Adds a column to `table` if it does not already exist.
fn add_column_if_absent(
    conn: &Connection,
    table: &str,
    column: &str,
    alter_sql: &str,
) -> Result<()> {
    if !has_column(conn, table, column) {
        conn.execute_batch(alter_sql)
            .with_context(|| format!("failed to add {column} column"))?;
    }
    Ok(())
}

/// Drops a denormalized column left over from an earlier schema. No-op on a
/// fresh database that never had it.
fn drop_column_if_present(conn: &Connection, table: &str, column: &str) -> Result<()> {
    if has_column(conn, table, column) {
        conn.execute_batch(&format!("ALTER TABLE {table} DROP COLUMN {column};"))
            .with_context(|| format!("failed to drop {column} column"))?;
    }
    Ok(())
}

/// Creates the base schema (tables, FTS index, and triggers) if it is absent.
///
/// `issues` holds only the issue's intrinsic columns plus FK columns (added by
/// migrations); referenced entity names live in their own tables and are joined
/// back into the fragment read model.
fn create_base_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS issues (
            id               TEXT PRIMARY KEY,
            identifier       TEXT NOT NULL,
            title            TEXT NOT NULL,
            priority_label   TEXT NOT NULL,
            created_at       TEXT NOT NULL,
            updated_at       TEXT NOT NULL,
            synced_at        TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sync_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS issues_fts USING fts5(
            identifier,
            title,
            content='issues',
            content_rowid='rowid'
        );
        CREATE TRIGGER IF NOT EXISTS issues_ai AFTER INSERT ON issues BEGIN
            INSERT INTO issues_fts(rowid, identifier, title)
            VALUES (new.rowid, new.identifier, new.title);
        END;
        CREATE TRIGGER IF NOT EXISTS issues_ad AFTER DELETE ON issues BEGIN
            INSERT INTO issues_fts(issues_fts, rowid, identifier, title)
            VALUES ('delete', old.rowid, old.identifier, old.title);
        END;
        CREATE TRIGGER IF NOT EXISTS issues_au AFTER UPDATE ON issues BEGIN
            INSERT INTO issues_fts(issues_fts, rowid, identifier, title)
            VALUES ('delete', old.rowid, old.identifier, old.title);
            INSERT INTO issues_fts(rowid, identifier, title)
            VALUES (new.rowid, new.identifier, new.title);
        END;
        CREATE TABLE IF NOT EXISTS issue_comments (
            id          TEXT PRIMARY KEY,
            issue_id    TEXT NOT NULL,
            body        TEXT NOT NULL,
            user_id     TEXT,
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL,
            synced_at   TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_issue_comments_issue_id
            ON issue_comments (issue_id);
        CREATE INDEX IF NOT EXISTS idx_issue_comments_created_at
            ON issue_comments (issue_id, created_at);",
    )
    .context("failed to run migrations")?;
    Ok(())
}

/// Creates the relational entity tables, the issue/label join table, the
/// pending-overlay table, and the mutation outbox if they are absent.
///
/// The entity tables are the normalized "base" the sync layer populates from
/// fetched issue fragments and the read model joins back into the fragment type.
/// `pending_overlay` is the local-intent half of the base/overlay split: a
/// delta write touches only the base tables, never it. `outbox` is the
/// paired command log the sync drainer replays against the API.
fn create_relational_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS teams (id TEXT PRIMARY KEY, name TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS users (id TEXT PRIMARY KEY, name TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS workflow_states (id TEXT PRIMARY KEY, name TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, name TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS cycles (id TEXT PRIMARY KEY, name TEXT);
        CREATE TABLE IF NOT EXISTS labels (id TEXT PRIMARY KEY, name TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS issue_labels (
            issue_id TEXT NOT NULL,
            label_id TEXT NOT NULL,
            PRIMARY KEY (issue_id, label_id)
        );
        CREATE INDEX IF NOT EXISTS idx_issue_labels_label_id ON issue_labels (label_id);
        CREATE TABLE IF NOT EXISTS pending_overlay (
            entity_id TEXT NOT NULL,
            field     TEXT NOT NULL,
            value     TEXT,
            PRIMARY KEY (entity_id, field)
        );
        CREATE TABLE IF NOT EXISTS outbox (
            seq        INTEGER PRIMARY KEY AUTOINCREMENT,
            op_type    TEXT NOT NULL,
            entity_id  TEXT NOT NULL,
            variables  TEXT NOT NULL,
            status     TEXT NOT NULL DEFAULT 'pending',
            attempts   INTEGER NOT NULL DEFAULT 0,
            last_error TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_outbox_pending ON outbox (status, seq);
        CREATE INDEX IF NOT EXISTS idx_issues_team_id   ON issues (team_id);
        CREATE INDEX IF NOT EXISTS idx_issues_state_id  ON issues (state_id);
        CREATE INDEX IF NOT EXISTS idx_issues_team_state ON issues (team_id, state_id);
        CREATE INDEX IF NOT EXISTS idx_issues_updated_at ON issues (updated_at);",
    )
    .context("failed to create relational schema")?;
    Ok(())
}

pub fn run_migrations(conn: &Connection) -> Result<()> {
    create_base_schema(conn)?;

    // Intrinsic columns added after the initial schema.
    add_column_if_absent(
        conn,
        "issues",
        "description",
        "ALTER TABLE issues ADD COLUMN description TEXT;",
    )?;
    add_column_if_absent(
        conn,
        "issues",
        "parent_id",
        "ALTER TABLE issues ADD COLUMN parent_id TEXT;",
    )?;

    // Relational FK columns: the read model joins through these to the entity
    // tables instead of reading denormalized name columns.
    for (col, sql) in [
        ("team_id", "ALTER TABLE issues ADD COLUMN team_id TEXT;"),
        ("state_id", "ALTER TABLE issues ADD COLUMN state_id TEXT;"),
        (
            "assignee_id",
            "ALTER TABLE issues ADD COLUMN assignee_id TEXT;",
        ),
        (
            "creator_id",
            "ALTER TABLE issues ADD COLUMN creator_id TEXT;",
        ),
        (
            "project_id",
            "ALTER TABLE issues ADD COLUMN project_id TEXT;",
        ),
        ("cycle_id", "ALTER TABLE issues ADD COLUMN cycle_id TEXT;"),
    ] {
        add_column_if_absent(conn, "issues", col, sql)?;
    }

    create_relational_schema(conn)?;

    // Drop the denormalized name columns now that the read model joins. A fresh
    // database never had them; an existing one is migrated in place.
    for col in [
        "state_name",
        "assignee_name",
        "team_name",
        "team_key",
        "labels",
        "project_name",
        "cycle_name",
        "creator_name",
        "parent_identifier",
    ] {
        drop_column_if_present(conn, "issues", col)?;
    }

    // `issue_comments` moved from a flattened `author_name` column to the
    // relational `user_id` FK (the ADR's model): a fresh database never had
    // `author_name`; an existing one is migrated in place. Existing rows lose
    // their author (`user_id` NULL) -- the DB is a resyncable cache, and
    // un-acked `local:` optimistic rows are untouched by this column swap.
    add_column_if_absent(
        conn,
        "issue_comments",
        "user_id",
        "ALTER TABLE issue_comments ADD COLUMN user_id TEXT;",
    )?;
    drop_column_if_present(conn, "issue_comments", "author_name")?;

    Ok(())
}

/// Open a connection to the SQLite database at `uri` -- a filesystem path or a
/// `file:...?mode=memory` URI -- and run migrations.
pub fn open_db(uri: impl AsRef<Path>) -> Result<Connection> {
    let uri = uri.as_ref();
    let conn = Connection::open(uri)
        .with_context(|| format!("could not open database at {}", uri.display()))?;
    run_migrations(&conn)?;
    Ok(conn)
}

/// A handle to the issue database. The set of databases is closed -- the
/// per-profile file on disk in normal use, or an isolated in-memory database in
/// tests -- so it is an enum rather than a trait with two impls. Both are
/// SQLite opened by path; `connect()` opens a fresh connection via `open_db`.
pub enum Database {
    /// The SQLite file on disk. Resolving the path and migrating is deferred to
    /// `connect()`, so constructing this variant does no I/O.
    File,
    /// An isolated, shared-cache in-memory database for tests. SQLite destroys
    /// a shared-cache in-memory database when its last connection closes, so
    /// the handle holds one open connection for its own lifetime.
    #[cfg(any(test, feature = "test-util"))]
    Memory { uri: String, _keepalive: Connection },
}

impl Database {
    /// Build an isolated in-memory database, migrated and ready. Each call gets
    /// a distinct shared cache so concurrent tests never share state.
    #[cfg(any(test, feature = "test-util"))]
    pub fn memory() -> Result<Self> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let uri = format!("file:lt_memdb_{n}?mode=memory&cache=shared");
        let keepalive = open_db(&uri)?;
        Ok(Self::Memory {
            uri,
            _keepalive: keepalive,
        })
    }

    /// Open a fresh connection to this database.
    pub fn connect(&self) -> Result<Connection> {
        match self {
            Database::File => open_db(db_path()?),
            #[cfg(any(test, feature = "test-util"))]
            Database::Memory { uri, .. } => open_db(uri),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_issue_by_id_resolves_and_misses() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        upsert_issues(&conn, &[outbox::sample_base_issue("9")]).unwrap();

        let found = query_issue_by_id(&conn, "9").unwrap().unwrap();
        assert_eq!(found.identifier, "ENG-9");
        assert_eq!(found.title, "issue 9");
        assert_eq!(found.state.name, "Todo");

        assert!(query_issue_by_id(&conn, "absent").unwrap().is_none());
    }
}
