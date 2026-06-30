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
                        id: String::new(),
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

impl From<crate::linear::types::Issue> for Issue {
    fn from(src: crate::linear::types::Issue) -> Self {
        let labels = src
            .labels
            .nodes
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(",");
        Self {
            id: src.id,
            identifier: src.identifier,
            title: src.title,
            priority_label: src.priority_label,
            state_name: src.state.name,
            assignee_name: src.assignee.map(|u| u.name),
            team_name: src.team.name,
            team_key: Some(src.team.id),
            created_at: src.created_at,
            updated_at: src.updated_at,
            synced_at: String::new(),
            description: src.description,
            labels,
            project_name: src.project.map(|p| p.name),
            cycle_name: src.cycle.and_then(|c| c.name),
            creator_name: src.creator.map(|u| u.name),
            parent_id: src.parent.as_ref().map(|p| p.id.clone()),
            parent_identifier: src.parent.map(|p| p.identifier),
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

/// Upsert one `(id, name)` row into a named entity table, updating the name on
/// id conflict (so a rename touches a single row). `name` is optional because
/// `cycles.name` is nullable; the other tables always pass `Some`.
fn upsert_named_entity(conn: &Connection, table: &str, id: &str, name: Option<&str>) -> Result<()> {
    let sql = format!(
        "INSERT INTO {table} (id, name) VALUES (?1, ?2)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name"
    );
    conn.execute(&sql, params![id, name])
        .with_context(|| format!("failed to upsert {table}"))?;
    Ok(())
}

/// Populate the relational base from fetched issue fragments: upsert each
/// referenced entity, set the issue's FK columns, and rebuild its label links.
///
/// Runs in one transaction per call. The flat `issues` row must already exist
/// (written by [`upsert_issues`]); this fills the FK columns on it. Only the
/// sync layer calls this — it is the sole source of the normalized base.
pub fn upsert_issue_graph(conn: &Connection, issues: &[crate::linear::types::Issue]) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .context("failed to begin issue-graph transaction")?;

    for issue in issues {
        upsert_named_entity(&tx, "teams", &issue.team.id, Some(&issue.team.name))?;
        upsert_named_entity(
            &tx,
            "workflow_states",
            &issue.state.id,
            Some(&issue.state.name),
        )?;
        if let Some(a) = &issue.assignee {
            upsert_named_entity(&tx, "users", &a.id, Some(&a.name))?;
        }
        if let Some(c) = &issue.creator {
            upsert_named_entity(&tx, "users", &c.id, Some(&c.name))?;
        }
        if let Some(p) = &issue.project {
            upsert_named_entity(&tx, "projects", &p.id, Some(&p.name))?;
        }
        if let Some(c) = &issue.cycle {
            upsert_named_entity(&tx, "cycles", &c.id, c.name.as_deref())?;
        }

        tx.execute(
            "UPDATE issues
                SET team_id = ?2, state_id = ?3, assignee_id = ?4,
                    creator_id = ?5, project_id = ?6, cycle_id = ?7
              WHERE id = ?1",
            params![
                issue.id,
                issue.team.id,
                issue.state.id,
                issue.assignee.as_ref().map(|u| &u.id),
                issue.creator.as_ref().map(|u| &u.id),
                issue.project.as_ref().map(|p| &p.id),
                issue.cycle.as_ref().map(|c| &c.id),
            ],
        )
        .context("failed to set issue FK columns")?;

        tx.execute(
            "DELETE FROM issue_labels WHERE issue_id = ?1",
            params![issue.id],
        )
        .context("failed to clear issue labels")?;
        for label in &issue.labels.nodes {
            upsert_named_entity(&tx, "labels", &label.id, Some(&label.name))?;
            tx.execute(
                "INSERT OR IGNORE INTO issue_labels (issue_id, label_id) VALUES (?1, ?2)",
                params![issue.id, label.id],
            )
            .context("failed to link issue label")?;
        }
    }

    tx.commit()
        .context("failed to commit issue-graph transaction")?;
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

    /// A fully-populated API issue (all optionals `Some`, two labels) shared by
    /// the conversion and relational-upsert tests.
    fn sample_api_issue() -> crate::linear::types::Issue {
        use crate::linear::types as api;
        api::Issue {
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
                        id: "l-bug".to_string(),
                        name: "bug".to_string(),
                    },
                    api::Label {
                        id: "l-backend".to_string(),
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
        }
    }

    #[test]
    fn api_issue_into_row_maps_and_joins_labels() {
        let row: Issue = sample_api_issue().into();
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

    fn graph_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        let api = sample_api_issue();
        upsert_issues(&conn, &[api.clone().into()]).unwrap();
        upsert_issue_graph(&conn, &[api]).unwrap();
        conn
    }

    #[test]
    fn upsert_issue_graph_populates_entities_fks_and_links() {
        let conn = graph_db();

        // Entity tables carry id -> name, deduplicated by id.
        let team_name: String = conn
            .query_row("SELECT name FROM teams WHERE id = 'ENG'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(team_name, "Engineering");

        // The issue's FK columns point at those entities.
        let fks: (String, String, String, String, String) = conn
            .query_row(
                "SELECT team_id, state_id, assignee_id, project_id, cycle_id
                 FROM issues WHERE id = '1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            fks,
            (
                "ENG".to_string(),
                "s1".to_string(),
                "u1".to_string(),
                "p1".to_string(),
                "c1".to_string(),
            )
        );

        // The selection reconstructs from joins.
        let (state, team, assignee, labels): (String, String, String, String) = conn
            .query_row(
                "SELECT ws.name, t.name, u.name,
                        (SELECT GROUP_CONCAT(l.name, ',') FROM issue_labels il
                           JOIN labels l ON l.id = il.label_id WHERE il.issue_id = i.id)
                 FROM issues i
                 JOIN teams t            ON t.id = i.team_id
                 JOIN workflow_states ws ON ws.id = i.state_id
                 LEFT JOIN users u       ON u.id = i.assignee_id
                 WHERE i.id = '1'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    ))
                },
            )
            .unwrap();
        assert_eq!(state, "In Progress");
        assert_eq!(team, "Engineering");
        assert_eq!(assignee, "Alice");
        let mut names: Vec<&str> = labels.split(',').collect();
        names.sort_unstable();
        assert_eq!(names, ["backend", "bug"]);
    }

    #[test]
    fn delta_base_write_leaves_pending_overlay_intact() {
        let conn = graph_db();

        // Local intent the UI would record on an edit.
        conn.execute(
            "INSERT INTO pending_overlay (entity_id, field, value) VALUES ('1', 'state', 'Done')",
            [],
        )
        .unwrap();

        // A delta pull rewrites the base (state changed server-side).
        let mut updated = sample_api_issue();
        updated.state = crate::linear::types::State {
            id: "s2".to_string(),
            name: "Canceled".to_string(),
        };
        upsert_issues(&conn, &[updated.clone().into()]).unwrap();
        upsert_issue_graph(&conn, &[updated]).unwrap();

        // Base moved; the overlay row is physically untouched by the base write.
        let base_state: String = conn
            .query_row("SELECT state_id FROM issues WHERE id = '1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(base_state, "s2");
        let overlay: String = conn
            .query_row(
                "SELECT value FROM pending_overlay WHERE entity_id = '1' AND field = 'state'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(overlay, "Done");
    }
}
