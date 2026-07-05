pub mod comments;
pub mod filters;
pub mod issues;
pub mod ops;
pub mod outbox;
pub(crate) mod sql;
pub mod teams;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
pub use comments::{delete_comments_for_issue, query_comments, upsert_comments};
pub use issues::{
    count_fts_rows, count_issues, get_meta, query_children, query_issue_by_id, query_issues,
    search_issues, search_issues_like, set_meta, set_synced_viewer, synced_viewer, upsert_issues,
};
pub use ops::{EntityKey, Read, Upsert};
pub use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};
pub use teams::{
    derive_team_memberships_from_issues, query_team_members, query_team_states, query_teams,
    replace_team_memberships, upsert_team_state, upsert_teams, upsert_users,
};

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

pub fn db_path() -> Result<PathBuf> {
    let data_dir = dirs::data_local_dir().context("could not determine local data directory")?;
    // Each profile gets its own database so accounts/workspaces never share
    // state and can run concurrently.
    let lt_dir = lt_config::profile_dir(&data_dir.join("lt"));
    fs::create_dir_all(&lt_dir)
        .with_context(|| format!("could not create directory: {}", lt_dir.display()))?;
    Ok(lt_dir.join("lt.db"))
}

const MIGRATION_1: &str = "\
    CREATE TABLE issues (
        id               TEXT PRIMARY KEY,
        identifier       TEXT NOT NULL,
        title            TEXT NOT NULL,
        priority_label   TEXT NOT NULL,
        description      TEXT,
        created_at       TEXT NOT NULL,
        updated_at       TEXT NOT NULL,
        synced_at        TEXT NOT NULL,
        parent_id        TEXT,
        team_id          TEXT,
        state_id         TEXT,
        assignee_id      TEXT,
        creator_id       TEXT,
        project_id       TEXT,
        cycle_id         TEXT
    );
    CREATE TABLE sync_meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE VIRTUAL TABLE issues_fts USING fts5(
        identifier,
        title,
        content='issues',
        content_rowid='rowid'
    );
    CREATE TRIGGER issues_ai AFTER INSERT ON issues BEGIN
        INSERT INTO issues_fts(rowid, identifier, title)
        VALUES (new.rowid, new.identifier, new.title);
    END;
    CREATE TRIGGER issues_ad AFTER DELETE ON issues BEGIN
        INSERT INTO issues_fts(issues_fts, rowid, identifier, title)
        VALUES ('delete', old.rowid, old.identifier, old.title);
    END;
    CREATE TRIGGER issues_au AFTER UPDATE ON issues BEGIN
        INSERT INTO issues_fts(issues_fts, rowid, identifier, title)
        VALUES ('delete', old.rowid, old.identifier, old.title);
        INSERT INTO issues_fts(rowid, identifier, title)
        VALUES (new.rowid, new.identifier, new.title);
    END;
    CREATE TABLE issue_comments (
        id          TEXT PRIMARY KEY,
        issue_id    TEXT NOT NULL,
        body        TEXT NOT NULL,
        user_id     TEXT,
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL,
        synced_at   TEXT NOT NULL
    );
    CREATE INDEX idx_issue_comments_issue_id ON issue_comments (issue_id);
    CREATE INDEX idx_issue_comments_created_at ON issue_comments (issue_id, created_at);
    CREATE TABLE teams (id TEXT PRIMARY KEY, name TEXT NOT NULL);
    CREATE TABLE users (id TEXT PRIMARY KEY, name TEXT NOT NULL);
    CREATE TABLE workflow_states (id TEXT PRIMARY KEY, name TEXT NOT NULL);
    CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT NOT NULL);
    CREATE TABLE cycles (id TEXT PRIMARY KEY, name TEXT);
    CREATE TABLE labels (id TEXT PRIMARY KEY, name TEXT NOT NULL);
    CREATE TABLE issue_labels (
        issue_id TEXT NOT NULL,
        label_id TEXT NOT NULL,
        PRIMARY KEY (issue_id, label_id)
    );
    CREATE INDEX idx_issue_labels_label_id ON issue_labels (label_id);
    CREATE TABLE pending_overlay (
        entity_id TEXT NOT NULL,
        field     TEXT NOT NULL,
        value     TEXT,
        PRIMARY KEY (entity_id, field)
    );
    CREATE TABLE outbox (
        seq        INTEGER PRIMARY KEY AUTOINCREMENT,
        op_type    TEXT NOT NULL,
        entity_id  TEXT NOT NULL,
        variables  TEXT NOT NULL,
        status     TEXT NOT NULL DEFAULT 'pending',
        attempts   INTEGER NOT NULL DEFAULT 0,
        last_error TEXT,
        created_at TEXT NOT NULL
    );
    CREATE INDEX idx_outbox_pending ON outbox (status, seq);
    CREATE INDEX idx_issues_team_id ON issues (team_id);
    CREATE INDEX idx_issues_state_id ON issues (state_id);
    CREATE INDEX idx_issues_team_state ON issues (team_id, state_id);
    CREATE INDEX idx_issues_updated_at ON issues (updated_at);";

/// Team-scoped cache: `workflow_states` gains the columns the state/assignee
/// pickers need (`docs/design/tui-app-event-queue-adr.md`, "Decision 4"), and
/// `team_memberships` records who is on which team -- not inferrable from
/// issues (an assignee is not provably a member).
const MIGRATION_2: &str = "\
    ALTER TABLE workflow_states ADD COLUMN team_id TEXT;
    ALTER TABLE workflow_states ADD COLUMN position REAL;
    CREATE INDEX idx_workflow_states_team_id ON workflow_states (team_id);
    CREATE TABLE team_memberships (
        team_id TEXT NOT NULL,
        user_id TEXT NOT NULL,
        PRIMARY KEY (team_id, user_id)
    );";

/// The migration list: the single schema source for both `open_db()` and the
/// `sql_validation` gate (docs/design/type-safe-sql-adr.md, "Migrations").
fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(MIGRATION_1), M::up(MIGRATION_2)])
}

/// Refuse a pre-versioning database (`user_version` 0 but tables exist): delete it and re-sync.
fn guard_against_legacy_database(conn: &Connection, path: &Path) -> Result<()> {
    let user_version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .context("failed to read user_version")?;
    if user_version != 0 {
        return Ok(());
    }

    let table_count: i64 = sql::prepare(conn, sql::COUNT_TABLES)
        .and_then(|mut stmt| stmt.query_row([], |row| row.get(0)))
        .context("failed to check for existing tables")?;
    if table_count > 0 {
        bail!(
            "database at {} predates versioned migrations; delete it and re-run 'lt sync' to rebuild the cache",
            path.display()
        );
    }
    Ok(())
}

/// Migrate `conn` to the latest schema, guarding against a legacy database at
/// `path` first. Private: migrations run exactly once, from [`open_db`].
fn run_migrations(conn: &mut Connection, path: &Path) -> Result<()> {
    guard_against_legacy_database(conn, path)?;
    migrations()
        .to_latest(conn)
        .context("failed to run migrations")
}

/// Open a connection to the SQLite database at `uri` -- a filesystem path or a
/// `file:...?mode=memory` URI -- and run migrations.
pub fn open_db(uri: impl AsRef<Path>) -> Result<Connection> {
    let uri = uri.as_ref();
    let mut conn = Connection::open(uri)
        .with_context(|| format!("could not open database at {}", uri.display()))?;
    run_migrations(&mut conn, uri)?;
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

    /// Open another handle onto the same database: for `File`, the same path
    /// (there is only one); for `Memory`, a second keepalive connection on the
    /// same shared-cache URI, so a second owner (e.g. a test's fake
    /// `SyncService`) reads and writes the exact rows the first sees. Neither
    /// handle's lifetime depends on the other's.
    #[cfg(any(test, feature = "test-util"))]
    pub fn share(&self) -> Result<Self> {
        match self {
            Database::File => Ok(Database::File),
            Database::Memory { uri, .. } => {
                let keepalive = open_db(uri)?;
                Ok(Database::Memory {
                    uri: uri.clone(),
                    _keepalive: keepalive,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_issue_by_id_resolves_and_misses() {
        let db = Database::memory().unwrap();
        let conn = db.connect().unwrap();
        upsert_issues(&conn, &[outbox::sample_base_issue("9")]).unwrap();

        let found = query_issue_by_id(&conn, "9").unwrap().unwrap();
        assert_eq!(found.identifier, "ENG-9");
        assert_eq!(found.title, "issue 9");
        assert_eq!(found.state.name, "Todo");

        assert!(query_issue_by_id(&conn, "absent").unwrap().is_none());
    }

    #[test]
    fn migrations_are_valid() {
        migrations().validate().unwrap();
    }

    /// We are pre-1.0 and keep no legacy-compatibility migration: a database
    /// from before this crate adopted `rusqlite_migration` sits at
    /// `user_version = 0` with tables already present. The guard must reject
    /// it with an actionable message rather than silently patching it (or
    /// worse, silently reinterpreting its rows under the new schema).
    #[test]
    fn legacy_database_is_rejected_with_an_actionable_error() {
        let mut conn = Connection::open_in_memory().unwrap();
        // A pre-versioned database, as the old hand-rolled probing code would
        // have left behind: some tables, `user_version` untouched at 0.
        conn.execute_batch("CREATE TABLE issues (id TEXT PRIMARY KEY);")
            .unwrap();

        let path = Path::new("/tmp/legacy-lt.db");
        let err = run_migrations(&mut conn, path).unwrap_err();

        let message = err.to_string();
        assert!(
            message.contains("delete"),
            "error should tell the user to delete the database: {message}"
        );
        assert!(
            message.contains("/tmp/legacy-lt.db"),
            "error should name the database path: {message}"
        );
    }
}
