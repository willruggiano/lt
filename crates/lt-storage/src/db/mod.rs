pub mod comments;
pub mod filters;
pub mod issues;
pub mod op_log;
pub(crate) mod sql;
pub mod teams;
pub mod viewer;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
pub use comments::{delete_comments_for_issue, query_comments, upsert_comments};
pub use issues::{
    count_fts_rows, count_issues, get_meta, issue_is_locally_unsynced, query_children,
    query_issue_by_id, query_issues, search_issues, search_issues_like, set_meta, upsert_issues,
};
pub use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};
pub use teams::{
    derive_team_memberships_from_issues, query_team_members, query_team_states, query_teams,
    replace_team_memberships, upsert_team_state, upsert_teams, upsert_users,
};
pub use viewer::{set_viewer, viewer};

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

const MIGRATION_2: &str = "\
    ALTER TABLE workflow_states ADD COLUMN team_id TEXT;
    ALTER TABLE workflow_states ADD COLUMN position REAL;
    CREATE INDEX idx_workflow_states_team_id ON workflow_states (team_id);
    CREATE TABLE team_memberships (
        team_id TEXT NOT NULL,
        user_id TEXT NOT NULL,
        PRIMARY KEY (team_id, user_id)
    );";

const MIGRATION_3: &str = "\
    CREATE TABLE organizations (
        id      TEXT PRIMARY KEY,
        name    TEXT NOT NULL,
        url_key TEXT NOT NULL
    );";

const MIGRATION_4: &str = "\
    UPDATE workflow_states SET position = 0 WHERE position IS NULL;";

const MIGRATION_5: &str = "\
    DROP TRIGGER IF EXISTS issues_ai;
    DROP TRIGGER IF EXISTS issues_ad;
    DROP TRIGGER IF EXISTS issues_au;
    DROP TABLE   IF EXISTS issues_fts;
    DROP TABLE   IF EXISTS pending_overlay;
    DROP TABLE   IF EXISTS outbox;
    DROP TABLE   IF EXISTS issue_labels;
    DROP TABLE   IF EXISTS issue_comments;
    DROP TABLE   IF EXISTS team_memberships;
    DROP TABLE   IF EXISTS workflow_states;
    DROP TABLE   IF EXISTS issues;
    DROP TABLE   IF EXISTS teams;
    DROP TABLE   IF EXISTS users;
    DROP TABLE   IF EXISTS projects;
    DROP TABLE   IF EXISTS cycles;
    DROP TABLE   IF EXISTS labels;
    DROP TABLE   IF EXISTS organizations;
    DROP TABLE   IF EXISTS sync_meta;

    CREATE TABLE teams    (id TEXT PRIMARY KEY, name TEXT);
    CREATE TABLE users    (id TEXT PRIMARY KEY, name TEXT);
    CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT);
    CREATE TABLE cycles   (id TEXT PRIMARY KEY, name TEXT);
    CREATE TABLE labels   (id TEXT PRIMARY KEY, name TEXT);

    CREATE TABLE workflow_states (
        id       TEXT PRIMARY KEY,
        name     TEXT,
        team_id  TEXT REFERENCES teams(id) ON UPDATE CASCADE,
        position REAL
    );
    CREATE INDEX idx_workflow_states_team_id ON workflow_states (team_id);

    CREATE TABLE issues (
        id             TEXT PRIMARY KEY,
        identifier     TEXT,
        title          TEXT,
        priority_label TEXT,
        description    TEXT,
        created_at     TEXT,
        updated_at     TEXT,
        synced_at      TEXT,
        parent_id      TEXT REFERENCES issues(id)          ON UPDATE CASCADE ON DELETE SET NULL,
        team_id        TEXT REFERENCES teams(id)           ON UPDATE CASCADE,
        state_id       TEXT REFERENCES workflow_states(id) ON UPDATE CASCADE,
        assignee_id    TEXT REFERENCES users(id)           ON UPDATE CASCADE,
        creator_id     TEXT REFERENCES users(id)           ON UPDATE CASCADE,
        project_id     TEXT REFERENCES projects(id)        ON UPDATE CASCADE,
        cycle_id       TEXT REFERENCES cycles(id)          ON UPDATE CASCADE
    );
    CREATE INDEX idx_issues_team_id    ON issues (team_id);
    CREATE INDEX idx_issues_state_id   ON issues (state_id);
    CREATE INDEX idx_issues_team_state ON issues (team_id, state_id);
    CREATE INDEX idx_issues_parent_id  ON issues (parent_id);
    CREATE INDEX idx_issues_updated_at ON issues (updated_at);

    CREATE VIRTUAL TABLE issues_fts USING fts5(
        identifier,
        title,
        content='issues',
        content_rowid='rowid'
    );
    CREATE TRIGGER issues_ai AFTER INSERT ON issues WHEN new.title IS NOT NULL BEGIN
        INSERT INTO issues_fts(rowid, identifier, title)
        VALUES (new.rowid, new.identifier, new.title);
    END;
    CREATE TRIGGER issues_ad AFTER DELETE ON issues WHEN old.title IS NOT NULL BEGIN
        INSERT INTO issues_fts(issues_fts, rowid, identifier, title)
        VALUES ('delete', old.rowid, old.identifier, old.title);
    END;
    CREATE TRIGGER issues_au_del AFTER UPDATE ON issues WHEN old.title IS NOT NULL BEGIN
        INSERT INTO issues_fts(issues_fts, rowid, identifier, title)
        VALUES ('delete', old.rowid, old.identifier, old.title);
    END;
    CREATE TRIGGER issues_au_ins AFTER UPDATE ON issues WHEN new.title IS NOT NULL BEGIN
        INSERT INTO issues_fts(rowid, identifier, title)
        VALUES (new.rowid, new.identifier, new.title);
    END;

    CREATE TABLE issue_comments (
        id         TEXT PRIMARY KEY,
        issue_id   TEXT NOT NULL REFERENCES issues(id) ON UPDATE CASCADE ON DELETE CASCADE,
        body       TEXT NOT NULL,
        user_id    TEXT REFERENCES users(id) ON UPDATE CASCADE,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        synced_at  TEXT
    );
    CREATE INDEX idx_issue_comments_issue_id   ON issue_comments (issue_id);
    CREATE INDEX idx_issue_comments_created_at ON issue_comments (issue_id, created_at);

    CREATE TABLE issue_labels (
        issue_id TEXT NOT NULL REFERENCES issues(id) ON UPDATE CASCADE ON DELETE CASCADE,
        label_id TEXT NOT NULL REFERENCES labels(id) ON UPDATE CASCADE ON DELETE CASCADE,
        PRIMARY KEY (issue_id, label_id)
    );
    CREATE INDEX idx_issue_labels_label_id ON issue_labels (label_id);

    CREATE TABLE team_memberships (
        team_id TEXT NOT NULL REFERENCES teams(id) ON UPDATE CASCADE ON DELETE CASCADE,
        user_id TEXT NOT NULL REFERENCES users(id) ON UPDATE CASCADE ON DELETE CASCADE,
        PRIMARY KEY (team_id, user_id)
    );

    CREATE TABLE organizations (
        id      TEXT PRIMARY KEY,
        name    TEXT,
        url_key TEXT
    );

    CREATE TABLE sync_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

    CREATE TABLE op_log (
        seq        INTEGER PRIMARY KEY AUTOINCREMENT,
        operation  TEXT NOT NULL,
        id         TEXT NOT NULL,
        attempts   INTEGER NOT NULL DEFAULT 0,
        last_error TEXT
    );
    CREATE INDEX idx_op_log_operation_id ON op_log (operation, id);";

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(MIGRATION_1),
        M::up(MIGRATION_2),
        M::up(MIGRATION_3),
        M::up(MIGRATION_4),
        M::up(MIGRATION_5),
    ])
}

fn run_migrations(conn: &mut Connection) -> Result<()> {
    migrations()
        .to_latest(conn)
        .context("failed to run migrations")
}

pub fn open_db(uri: impl AsRef<Path>) -> Result<Connection> {
    let uri = uri.as_ref();
    let path = uri.to_string_lossy();
    tracing::info!(db = %path, "opening database");
    let mut conn = Connection::open(uri)
        .with_context(|| format!("could not open database at {}", uri.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .context("failed to set busy timeout")?;
    conn.pragma_update(None, "foreign_keys", true)
        .context("failed to enable foreign key enforcement")?;
    run_migrations(&mut conn)?;
    Ok(conn)
}

pub enum Database {
    File,
    #[cfg(any(test, feature = "test-util"))]
    Memory {
        uri: String,
        _keepalive: Connection,
    },
}

impl Database {
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
        teams::upsert_team_state(
            &conn,
            "ENG",
            &lt_types::types::WorkflowState {
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
    fn migrations_are_valid() {
        migrations().validate().unwrap();
    }
}
