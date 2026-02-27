use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, params};

use crate::issues::IssueArgs;

/// A row in the `issues` table.
#[derive(Debug, Clone)]
pub struct Issue {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub priority_label: String,
    pub state_name: String,
    pub assignee_name: Option<String>,
    pub team_name: String,
    pub team_key: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[allow(dead_code)]
    pub synced_at: String,
    pub description: Option<String>,
    pub labels: String,
}

/// Insert or replace a slice of issues, setting synced_at to now (UTC).
pub fn upsert_issues(conn: &Connection, issues: &[Issue]) -> Result<()> {
    let synced_at = Utc::now().to_rfc3339();
    let mut stmt = conn
        .prepare(
            "INSERT OR REPLACE INTO issues
             (id, identifier, title, priority_label, state_name,
              assignee_name, team_name, team_key, created_at, updated_at, synced_at,
              description, labels)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )
        .context("failed to prepare upsert statement")?;

    for issue in issues {
        stmt.execute(params![
            issue.id,
            issue.identifier,
            issue.title,
            issue.priority_label,
            issue.state_name,
            issue.assignee_name,
            issue.team_name,
            issue.team_key,
            issue.created_at,
            issue.updated_at,
            synced_at,
            issue.description,
            issue.labels,
        ])
        .context("failed to upsert issue")?;
    }
    Ok(())
}

/// Query issues from the local DB, applying ORDER BY from IssueArgs.
///
/// Filtering is intentionally minimal here.
/// TODO(bd-2km): replace with build_sql_filter(args) for the WHERE clause.
pub fn query_issues(conn: &Connection, args: &IssueArgs) -> Result<Vec<Issue>> {
    let (issues, _) = query_issues_page(conn, args, 0)?;
    Ok(issues)
}

/// Query issues with an explicit row offset for pagination.
///
/// Returns up to `args.limit` rows starting at `offset`, plus a boolean
/// indicating whether a next page exists.
pub fn query_issues_page(
    conn: &Connection,
    args: &IssueArgs,
    offset: i64,
) -> Result<(Vec<Issue>, bool)> {
    let order_col = match args.sort {
        crate::issues::SortField::Created => "created_at",
        crate::issues::SortField::Updated => "updated_at",
        crate::issues::SortField::Priority => "priority_label",
        crate::issues::SortField::Title => "title",
        crate::issues::SortField::Assignee => "assignee_name",
        crate::issues::SortField::State => "state_name",
        crate::issues::SortField::Team => "team_name",
    };
    let direction = if args.desc { "DESC" } else { "ASC" };
    // Fetch one extra row to detect whether there is a next page.
    let limit = args.limit.min(250) as i64;
    let fetch_limit = limit + 1;

    let sql = format!(
        "SELECT id, identifier, title, priority_label, state_name,
                assignee_name, team_name, team_key, created_at, updated_at, synced_at,
                description, labels
         FROM issues
         WHERE 1=1
         ORDER BY {order_col} {direction}
         LIMIT ?1 OFFSET ?2"
    );

    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare query statement")?;

    let rows = stmt
        .query_map(params![fetch_limit, offset], |row| {
            Ok(Issue {
                id: row.get(0)?,
                identifier: row.get(1)?,
                title: row.get(2)?,
                priority_label: row.get(3)?,
                state_name: row.get(4)?,
                assignee_name: row.get(5)?,
                team_name: row.get(6)?,
                team_key: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                synced_at: row.get(10)?,
                description: row.get(11)?,
                labels: row.get::<_, Option<String>>(12)?.unwrap_or_default(),
            })
        })
        .context("failed to execute query")?;

    let mut issues = Vec::new();
    for row in rows {
        issues.push(row.context("failed to read issue row")?);
    }

    let has_next = issues.len() > limit as usize;
    if has_next {
        issues.truncate(limit as usize);
    }
    Ok((issues, has_next))
}

/// Search issues using FTS5 full-text search.
///
/// `query` supports FTS5 syntax: prefix queries (`oauth*`), phrase queries
/// (`"oauth token"`), and boolean operators (`oauth AND token`).
///
/// Results are returned ordered by FTS5 rank (best match first).
pub fn search_issues(conn: &Connection, query: &str) -> Result<Vec<Issue>> {
    let sql = "SELECT i.id, i.identifier, i.title, i.priority_label, i.state_name,
                      i.assignee_name, i.team_name, i.team_key, i.created_at, i.updated_at,
                      i.synced_at, i.description, i.labels
               FROM issues i
               JOIN issues_fts ON issues_fts.rowid = i.rowid
               WHERE issues_fts MATCH ?1
               ORDER BY rank";

    let mut stmt = conn
        .prepare(sql)
        .context("failed to prepare search_issues statement")?;

    let rows = stmt
        .query_map(params![query], |row| {
            Ok(Issue {
                id: row.get(0)?,
                identifier: row.get(1)?,
                title: row.get(2)?,
                priority_label: row.get(3)?,
                state_name: row.get(4)?,
                assignee_name: row.get(5)?,
                team_name: row.get(6)?,
                team_key: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                synced_at: row.get(10)?,
                description: row.get(11)?,
                labels: row.get::<_, Option<String>>(12)?.unwrap_or_default(),
            })
        })
        .context("failed to execute search_issues query")?;

    let mut issues = Vec::new();
    for row in rows {
        issues.push(row.context("failed to read issue row")?);
    }
    Ok(issues)
}

/// Retrieve a value from the sync_meta table. Returns None if key is absent.
pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT value FROM sync_meta WHERE key = ?1")
        .context("failed to prepare get_meta statement")?;

    let mut rows = stmt
        .query(params![key])
        .context("failed to query sync_meta")?;

    if let Some(row) = rows.next().context("failed to read sync_meta row")? {
        let value: String = row.get(0).context("failed to read sync_meta value")?;
        Ok(Some(value))
    } else {
        Ok(None)
    }
}

/// Insert or replace a key/value pair in the sync_meta table.
pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO sync_meta (key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .context("failed to set sync_meta")?;
    Ok(())
}
