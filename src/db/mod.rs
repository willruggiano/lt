pub mod filters;
pub mod issues;

pub use issues::{Issue, get_meta, query_issues, search_issues, set_meta, upsert_issues};

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::fs;
use std::path::PathBuf;

fn db_path() -> Result<PathBuf> {
    let data_dir = dirs::data_local_dir().context("could not determine local data directory")?;
    let lt_dir = data_dir.join("lt");
    fs::create_dir_all(&lt_dir)
        .with_context(|| format!("could not create directory: {}", lt_dir.display()))?;
    Ok(lt_dir.join("lt.db"))
}

fn run_migrations(conn: &Connection) -> Result<()> {
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
        END;",
    )
    .context("failed to run migrations")?;
    Ok(())
}

pub fn open_db() -> Result<Connection> {
    let path = db_path()?;
    let conn = Connection::open(&path)
        .with_context(|| format!("could not open database at {}", path.display()))?;
    run_migrations(&conn)?;
    Ok(conn)
}
