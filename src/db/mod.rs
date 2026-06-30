pub mod comments;
pub mod filters;
pub mod issues;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
pub use comments::{Comment, delete_comments_for_issue, query_comments, upsert_comments};
pub(crate) use issues::issue_from_row;
pub use issues::{
    Issue, get_meta, query_children, query_issues, query_issues_page, search_issues, set_meta,
    upsert_issues,
};
use rusqlite::{Connection, Params};

/// Run a parameterized write statement, attaching `what` to any error.
///
/// `what` reads as the failed action, e.g. `"set sync_meta"`.
pub(crate) fn execute(conn: &Connection, sql: &str, params: impl Params, what: &str) -> Result<()> {
    conn.execute(sql, params)
        .with_context(|| format!("failed to {what}"))?;
    Ok(())
}

pub(crate) fn db_path() -> Result<PathBuf> {
    let data_dir = dirs::data_local_dir().context("could not determine local data directory")?;
    // Each profile gets its own database so accounts/workspaces never share
    // state and can run concurrently.
    let lt_dir = crate::config::profile_dir(&data_dir.join("lt"));
    fs::create_dir_all(&lt_dir)
        .with_context(|| format!("could not create directory: {}", lt_dir.display()))?;
    Ok(lt_dir.join("lt.db"))
}

/// Adds a column to the `issues` table if it does not already exist.
fn add_column_if_absent(conn: &Connection, column: &str, alter_sql: &str) -> Result<()> {
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('issues') WHERE name=?1",
            [column],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;
    if !exists {
        conn.execute_batch(alter_sql)
            .with_context(|| format!("failed to add {column} column"))?;
    }
    Ok(())
}

/// Creates the base schema (tables, FTS index, and triggers) if it is absent.
fn create_base_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS issues (
            id               TEXT PRIMARY KEY,
            identifier       TEXT NOT NULL,
            title            TEXT NOT NULL,
            priority_label   TEXT NOT NULL,
            state_name       TEXT NOT NULL,
            assignee_name    TEXT,
            team_name        TEXT NOT NULL,
            team_key         TEXT,
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
            author_name TEXT,
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

pub(crate) fn run_migrations(conn: &Connection) -> Result<()> {
    create_base_schema(conn)?;

    // Migrations: add columns that were introduced after the initial schema.
    add_column_if_absent(
        conn,
        "description",
        "ALTER TABLE issues ADD COLUMN description TEXT;",
    )?;
    add_column_if_absent(
        conn,
        "labels",
        "ALTER TABLE issues ADD COLUMN labels TEXT NOT NULL DEFAULT '';",
    )?;
    add_column_if_absent(
        conn,
        "project_name",
        "ALTER TABLE issues ADD COLUMN project_name TEXT;",
    )?;
    add_column_if_absent(
        conn,
        "cycle_name",
        "ALTER TABLE issues ADD COLUMN cycle_name TEXT;",
    )?;
    add_column_if_absent(
        conn,
        "creator_name",
        "ALTER TABLE issues ADD COLUMN creator_name TEXT;",
    )?;
    add_column_if_absent(
        conn,
        "parent_id",
        "ALTER TABLE issues ADD COLUMN parent_id TEXT;",
    )?;
    add_column_if_absent(
        conn,
        "parent_identifier",
        "ALTER TABLE issues ADD COLUMN parent_identifier TEXT;",
    )?;

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
    #[cfg(all(test, feature = "sim"))]
    Memory { uri: String, _keepalive: Connection },
}

impl Database {
    /// Build an isolated in-memory database, migrated and ready. Each call gets
    /// a distinct shared cache so concurrent tests never share state.
    #[cfg(all(test, feature = "sim"))]
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
            #[cfg(all(test, feature = "sim"))]
            Database::Memory { uri, .. } => open_db(uri),
        }
    }
}
