pub mod comments;
pub mod filters;
pub mod issues;
pub mod outbox;
pub(crate) mod sql;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
pub use comments::{delete_comments_for_issue, query_comments, upsert_comments};
pub(crate) use issues::issue_from_row;
pub use issues::{
    count_fts_rows, count_issues, get_meta, query_children, query_issue_by_id, query_issues,
    query_issues_page, search_issues, search_issues_like, set_meta, set_synced_viewer,
    synced_viewer, upsert_issues,
};
pub use rusqlite::Connection;
use rusqlite::Transaction;
use rusqlite_migration::{HookResult, M, Migrations};

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

/// Whether `column` exists on `table`.
fn has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name=?2",
        rusqlite::params![table, column],
        |row| row.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

/// Adds a column to `table` if it does not already exist.
fn add_column_if_absent(
    conn: &Connection,
    table: &str,
    column: &str,
    alter_sql: &str,
) -> rusqlite::Result<()> {
    if !has_column(conn, table, column)? {
        conn.execute_batch(alter_sql)?;
    }
    Ok(())
}

/// Drops a denormalized column left over from an earlier schema. No-op on a
/// fresh database that never had it.
fn drop_column_if_present(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<()> {
    if has_column(conn, table, column)? {
        conn.execute_batch(&format!("ALTER TABLE {table} DROP COLUMN {column};"))?;
    }
    Ok(())
}

/// Migration 1's unconditional DDL: `issues`, `sync_meta`, the FTS5 index and
/// its sync triggers, and `issue_comments`. Runs as the migration's `up` SQL,
/// before [`migrate_v1_hook`].
///
/// `issues` holds only the issue's intrinsic columns plus FK columns (added by
/// the hook); referenced entity names live in their own tables and are joined
/// back into the fragment read model.
const MIGRATION_1_BASE_SCHEMA: &str = "\
    CREATE TABLE IF NOT EXISTS issues (
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
        ON issue_comments (issue_id, created_at);";

/// Adds the intrinsic and relational FK columns `issues` gained after the
/// initial schema. The FK columns are what the read model joins through to
/// the entity tables instead of reading denormalized name columns.
fn migrate_v1_add_issue_columns(tx: &Transaction) -> HookResult {
    add_column_if_absent(
        tx,
        "issues",
        "description",
        "ALTER TABLE issues ADD COLUMN description TEXT;",
    )?;
    add_column_if_absent(
        tx,
        "issues",
        "parent_id",
        "ALTER TABLE issues ADD COLUMN parent_id TEXT;",
    )?;
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
        add_column_if_absent(tx, "issues", col, sql)?;
    }
    Ok(())
}

/// Creates the relational entity tables, the issue/label join table, the
/// pending-overlay table, and the mutation outbox.
///
/// The entity tables are the normalized "base" the sync layer populates from
/// fetched issue fragments and the read model joins back into the fragment
/// type. `pending_overlay` is the local-intent half of the base/overlay
/// split: a delta write touches only the base tables, never it. `outbox` is
/// the paired command log the sync drainer replays against the API. The
/// `issues` indexes here need `team_id`/`state_id` to already exist, hence
/// this runs after [`migrate_v1_add_issue_columns`].
fn migrate_v1_relational_schema(tx: &Transaction) -> HookResult {
    tx.execute_batch(
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
    )?;
    Ok(())
}

/// Drops the denormalized columns superseded by the relational schema: the
/// `issues` name columns (now joined from the entity tables) and
/// `issue_comments.author_name` (now the relational `user_id` FK). A fresh
/// database never had any of them; an existing one is migrated in place.
/// Existing comment rows lose their author (`user_id` NULL) -- the DB is a
/// resyncable cache, and un-acked `local:` optimistic rows are untouched by
/// this column swap.
fn migrate_v1_drop_legacy_columns(tx: &Transaction) -> HookResult {
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
        drop_column_if_present(tx, "issues", col)?;
    }
    add_column_if_absent(
        tx,
        "issue_comments",
        "user_id",
        "ALTER TABLE issue_comments ADD COLUMN user_id TEXT;",
    )?;
    drop_column_if_present(tx, "issue_comments", "author_name")?;
    Ok(())
}

/// The conditional half of migration 1, run as its hook (i.e. after
/// [`MIGRATION_1_BASE_SCHEMA`], inside the same migration transaction):
/// column add/drop probes and the relational schema batch. SQLite has no
/// `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`, so these steps stay
/// hand-written rather than plain `M::up` SQL.
///
/// Migration 1 must be idempotent against *any* prior schema shape: a
/// database from before this crate adopted `rusqlite_migration` arrives at
/// `user_version = 0`, indistinguishable from a fresh database, and this is
/// the DDL the old hand-rolled probing code used to run on every open (see
/// docs/design/type-safe-sql-adr.md, "Migrations"). All future schema changes
/// are plain versioned `M::up` entries appended after this one -- this hook
/// is not a pattern to repeat.
fn migrate_v1_hook(tx: &Transaction) -> HookResult {
    migrate_v1_add_issue_columns(tx)?;
    migrate_v1_relational_schema(tx)?;
    migrate_v1_drop_legacy_columns(tx)?;
    Ok(())
}

/// The migration list: the single schema source for both `open_db()` and the
/// `sql_validation` gate (docs/design/type-safe-sql-adr.md, "Migrations").
fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up_with_hook(
        MIGRATION_1_BASE_SCHEMA,
        migrate_v1_hook,
    )])
}

pub fn run_migrations(conn: &mut Connection) -> Result<()> {
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
    run_migrations(&mut conn)?;
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
        let mut conn = Connection::open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
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

    /// A legacy database from before this crate adopted `rusqlite_migration`
    /// arrives with `user_version = 0`, indistinguishable from a fresh
    /// database, so running migration 1 a second time against an
    /// already-migrated database must be a no-op, not an error.
    #[test]
    fn migration_1_is_idempotent_against_an_already_migrated_database() {
        let mut conn = Connection::open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();

        conn.pragma_update(None, "user_version", 0).unwrap();
        run_migrations(&mut conn).unwrap();

        assert!(has_column(&conn, "issues", "team_id").unwrap());
        let sync_meta_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'sync_meta'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sync_meta_exists, 1);
    }
}
