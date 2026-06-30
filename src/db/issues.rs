use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, params};

use crate::issues::IssueArgs;

/// A row in the `issues` table.
#[derive(Debug, Clone, PartialEq)]
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
    pub project_name: Option<String>,
    pub cycle_name: Option<String>,
    pub creator_name: Option<String>,
    pub parent_id: Option<String>,
    pub parent_identifier: Option<String>,
}

/// Rehydrate the display `Issue` shown in the list from a cached row. The row
/// stores only names, so the id fields of nested records are left empty.
impl From<Issue> for crate::linear::types::Issue {
    fn from(src: Issue) -> Self {
        use crate::linear::types;
        Self {
            id: src.id,
            identifier: src.identifier,
            title: src.title,
            priority: types::priority_label_to_u8(&src.priority_label),
            priority_label: src.priority_label,
            state: types::State {
                id: String::new(),
                name: src.state_name,
            },
            assignee: src.assignee_name.map(|n| types::User {
                id: String::new(),
                name: n,
            }),
            team: types::Team {
                id: src.team_key.unwrap_or_default(),
                name: src.team_name,
            },
            created_at: src.created_at,
            updated_at: src.updated_at,
            description: src.description,
            labels: types::LabelConnection {
                nodes: src
                    .labels
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(|n| types::Label {
                        name: n.to_string(),
                    })
                    .collect(),
            },
            project: src.project_name.map(|n| types::Project {
                id: String::new(),
                name: n,
            }),
            cycle: src.cycle_name.map(|n| types::Cycle {
                id: String::new(),
                name: Some(n),
            }),
            creator: src.creator_name.map(|n| types::User {
                id: String::new(),
                name: n,
            }),
            parent: src.parent_id.map(|id| types::Parent {
                id,
                identifier: src.parent_identifier.unwrap_or_default(),
            }),
        }
    }
}

/// Flatten a fetched API `Issue` into a cache row. The inverse of the
/// rehydration impl above; `synced_at` is left empty for `upsert_issues` to
/// fill. Both conversions are deliberately anchored on the API `Issue` so the
/// pair reads together (cf. `db::comments`), which means this direction is an
/// `Into` rather than a `From<_> for Issue` -- `from_over_into` is allowed for
/// that reason.
#[allow(clippy::from_over_into)]
impl Into<Issue> for crate::linear::types::Issue {
    fn into(self) -> Issue {
        let labels = self
            .labels
            .nodes
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(",");
        Issue {
            id: self.id,
            identifier: self.identifier,
            title: self.title,
            priority_label: self.priority_label,
            state_name: self.state.name,
            assignee_name: self.assignee.map(|u| u.name),
            team_name: self.team.name,
            team_key: Some(self.team.id),
            created_at: self.created_at,
            updated_at: self.updated_at,
            synced_at: String::new(),
            description: self.description,
            labels,
            project_name: self.project.map(|p| p.name),
            cycle_name: self.cycle.and_then(|c| c.name),
            creator_name: self.creator.map(|u| u.name),
            parent_id: self.parent.as_ref().map(|p| p.id.clone()),
            parent_identifier: self.parent.map(|p| p.identifier),
        }
    }
}

/// Insert or replace a slice of issues, setting `synced_at` to now (UTC).
pub fn upsert_issues(conn: &Connection, issues: &[Issue]) -> Result<()> {
    let synced_at = Utc::now().to_rfc3339();
    let mut stmt = conn
        .prepare(
            "INSERT OR REPLACE INTO issues
             (id, identifier, title, priority_label, state_name,
              assignee_name, team_name, team_key, created_at, updated_at, synced_at,
              description, labels, project_name, cycle_name, creator_name,
              parent_id, parent_identifier)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
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
            issue.project_name,
            issue.cycle_name,
            issue.creator_name,
            issue.parent_id,
            issue.parent_identifier,
        ])
        .context("failed to upsert issue")?;
    }
    Ok(())
}

/// Query issues from the local DB, applying the WHERE clause built from the
/// `IssueArgs` filter fields (bd-2km) and ORDER BY from the sort fields.
///
/// An `--assignee=me` filter must be resolved to the viewer's name by the
/// caller before calling this (see `issues::list::resolve_me`).
pub fn query_issues(conn: &Connection, args: &IssueArgs) -> Result<Vec<Issue>> {
    let (where_clause, mut bind) = crate::db::filters::build_sql_filter(args)?;
    let order = crate::db::filters::build_sql_order(args);
    let where_sql = if where_clause.is_empty() {
        String::new()
    } else {
        format!("WHERE {where_clause} ")
    };
    let limit = i64::from(args.limit.min(250));
    bind.push(Box::new(limit));

    let sql = format!(
        "SELECT id, identifier, title, priority_label, state_name,
                assignee_name, team_name, team_key, created_at, updated_at, synced_at,
                description, labels, project_name, cycle_name, creator_name,
                parent_id, parent_identifier
         FROM issues
         {where_sql}ORDER BY {order}
         LIMIT ?"
    );

    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare query_issues statement")?;

    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(bind.iter().map(std::convert::AsRef::as_ref)),
            issue_from_row,
        )
        .context("failed to execute query_issues")?;

    let mut issues = Vec::new();
    for row in rows {
        issues.push(row.context("failed to read issue row")?);
    }
    Ok(issues)
}

/// Map a row in the canonical issues column order to an Issue.
pub(crate) fn issue_from_row(row: &rusqlite::Row) -> rusqlite::Result<Issue> {
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
        project_name: row.get(13)?,
        cycle_name: row.get(14)?,
        creator_name: row.get(15)?,
        parent_id: row.get(16)?,
        parent_identifier: row.get(17)?,
    })
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
    let cap = args.limit.min(250);
    let fetch_limit = i64::from(cap) + 1;

    let sql = format!(
        "SELECT id, identifier, title, priority_label, state_name,
                assignee_name, team_name, team_key, created_at, updated_at, synced_at,
                description, labels, project_name, cycle_name, creator_name,
                parent_id, parent_identifier
         FROM issues
         WHERE 1=1
         ORDER BY {order_col} {direction}
         LIMIT ?1 OFFSET ?2"
    );

    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare query statement")?;

    let rows = stmt
        .query_map(params![fetch_limit, offset], issue_from_row)
        .context("failed to execute query")?;

    let mut issues = Vec::new();
    for row in rows {
        issues.push(row.context("failed to read issue row")?);
    }

    let cap_rows = usize::try_from(cap).unwrap_or(usize::MAX);
    let has_next = issues.len() > cap_rows;
    if has_next {
        issues.truncate(cap_rows);
    }
    Ok((issues, has_next))
}

/// Run a single-parameter `SELECT` and map each row via `issue_from_row`.
///
/// `what` names the query for error context.
fn query_issues_one(conn: &Connection, sql: &str, param: &str, what: &str) -> Result<Vec<Issue>> {
    let mut stmt = conn
        .prepare(sql)
        .with_context(|| format!("failed to prepare {what} statement"))?;

    let rows = stmt
        .query_map(params![param], issue_from_row)
        .with_context(|| format!("failed to execute {what} query"))?;

    let mut issues = Vec::new();
    for row in rows {
        issues.push(row.context("failed to read issue row")?);
    }
    Ok(issues)
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
                      i.synced_at, i.description, i.labels,
                      i.project_name, i.cycle_name, i.creator_name,
                      i.parent_id, i.parent_identifier
               FROM issues i
               JOIN issues_fts ON issues_fts.rowid = i.rowid
               WHERE issues_fts MATCH ?1
               ORDER BY rank";

    query_issues_one(conn, sql, query, "search_issues")
}

/// Retrieve a value from the `sync_meta` table. Returns None if key is absent.
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

/// Query child issues of a given parent issue.
pub fn query_children(conn: &Connection, parent_id: &str) -> Result<Vec<Issue>> {
    let sql = "SELECT id, identifier, title, priority_label, state_name,
                      assignee_name, team_name, team_key, created_at, updated_at, synced_at,
                      description, labels, project_name, cycle_name, creator_name,
                      parent_id, parent_identifier
               FROM issues
               WHERE parent_id = ?1
               ORDER BY identifier ASC";

    query_issues_one(conn, sql, parent_id, "query_children")
}

/// Insert or replace a key/value pair in the `sync_meta` table.
pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    crate::db::execute(
        conn,
        "INSERT OR REPLACE INTO sync_meta (key, value) VALUES (?1, ?2)",
        params![key, value],
        "set sync_meta",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_issue(id: &str, assignee: Option<&str>, state: &str) -> Issue {
        Issue {
            id: id.to_string(),
            identifier: format!("ENG-{id}"),
            title: format!("issue {id}"),
            priority_label: "Medium".to_string(),
            state_name: state.to_string(),
            assignee_name: assignee.map(std::string::ToString::to_string),
            team_name: "Engineering".to_string(),
            team_key: Some("ENG".to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            synced_at: String::new(),
            description: None,
            labels: String::new(),
            project_name: None,
            cycle_name: None,
            creator_name: None,
            parent_id: None,
            parent_identifier: None,
        }
    }

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        upsert_issues(
            &conn,
            &[
                test_issue("1", Some("Alice"), "Todo"),
                test_issue("2", Some("Bob"), "In Progress"),
                test_issue("3", None, "Todo"),
            ],
        )
        .unwrap();
        conn
    }

    #[test]
    fn api_issue_into_row_maps_and_joins_labels() {
        use crate::linear::types as api;
        let issue = api::Issue {
            id: "1".to_string(),
            identifier: "ENG-1".to_string(),
            title: "Wire it up".to_string(),
            priority_label: "High".to_string(),
            priority: 2,
            state: api::State {
                id: "s1".to_string(),
                name: "In Progress".to_string(),
            },
            assignee: Some(api::User {
                id: "u1".to_string(),
                name: "Alice".to_string(),
            }),
            team: api::Team {
                id: "ENG".to_string(),
                name: "Engineering".to_string(),
            },
            description: Some("body".to_string()),
            labels: api::LabelConnection {
                nodes: vec![
                    api::Label {
                        name: "bug".to_string(),
                    },
                    api::Label {
                        name: "backend".to_string(),
                    },
                ],
            },
            project: Some(api::Project {
                id: "p1".to_string(),
                name: "Platform".to_string(),
            }),
            cycle: Some(api::Cycle {
                id: "c1".to_string(),
                name: Some("Cycle 7".to_string()),
            }),
            creator: Some(api::User {
                id: "u2".to_string(),
                name: "Carol".to_string(),
            }),
            parent: Some(api::Parent {
                id: "9".to_string(),
                identifier: "ENG-9".to_string(),
            }),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
        };

        let row: Issue = issue.into();
        assert_eq!(row.identifier, "ENG-1");
        assert_eq!(row.assignee_name.as_deref(), Some("Alice"));
        assert_eq!(row.team_key.as_deref(), Some("ENG"));
        assert_eq!(row.labels, "bug,backend");
        assert_eq!(row.project_name.as_deref(), Some("Platform"));
        assert_eq!(row.cycle_name.as_deref(), Some("Cycle 7"));
        assert_eq!(row.creator_name.as_deref(), Some("Carol"));
        assert_eq!(row.parent_id.as_deref(), Some("9"));
        assert_eq!(row.parent_identifier.as_deref(), Some("ENG-9"));
        // synced_at is filled by upsert_issues, not the conversion.
        assert!(row.synced_at.is_empty());
    }

    #[test]
    fn api_issue_into_row_handles_absent_optionals() {
        use crate::linear::types as api;
        let issue = api::Issue {
            id: "2".to_string(),
            identifier: "ENG-2".to_string(),
            title: "t".to_string(),
            priority_label: "No priority".to_string(),
            priority: 0,
            state: api::State {
                id: "s".to_string(),
                name: "Todo".to_string(),
            },
            assignee: None,
            team: api::Team {
                id: "ENG".to_string(),
                name: "Engineering".to_string(),
            },
            description: None,
            labels: api::LabelConnection { nodes: Vec::new() },
            project: None,
            cycle: None,
            creator: None,
            parent: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let row: Issue = issue.into();
        assert!(row.assignee_name.is_none());
        assert_eq!(row.labels, "");
        assert!(row.project_name.is_none());
        assert!(row.cycle_name.is_none());
        assert!(row.creator_name.is_none());
        assert!(row.parent_id.is_none());
    }

    #[test]
    fn query_issues_applies_assignee_filter() {
        let conn = test_db();
        let args = crate::issues::IssueArgs {
            assignee: Some("alice".to_string()),
            ..Default::default()
        };
        let issues = query_issues(&conn, &args).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].assignee_name.as_deref(), Some("Alice"));
    }

    #[test]
    fn query_issues_applies_no_assignee_filter() {
        let conn = test_db();
        let args = crate::issues::IssueArgs {
            no_assignee: true,
            ..Default::default()
        };
        let issues = query_issues(&conn, &args).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "3");
    }

    #[test]
    fn query_issues_applies_state_filter_and_limit() {
        let conn = test_db();
        let mut args = crate::issues::IssueArgs {
            state: Some("todo".to_string()),
            ..Default::default()
        };
        let issues = query_issues(&conn, &args).unwrap();
        assert_eq!(issues.len(), 2);

        args.limit = 1;
        let issues = query_issues(&conn, &args).unwrap();
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn query_issues_without_filters_returns_all() {
        let conn = test_db();
        let args = crate::issues::IssueArgs::default();
        let issues = query_issues(&conn, &args).unwrap();
        assert_eq!(issues.len(), 3);
    }
}
