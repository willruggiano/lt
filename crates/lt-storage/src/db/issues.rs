use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::Utc;
use lt_types::query::IssueQuery;
use lt_types::scalars::Priority;
use lt_types::types;
use rusqlite::{Connection, params};

use crate::db::parse_datetime_column;

/// The fragment-typed read model's column list: every field
/// [`types::Issue`] selects, sourced from the relational base via the joins in
/// [`ISSUE_JOINS`]. Labels are aggregated by a correlated subquery.
pub(crate) const ISSUE_COLUMNS: &str =
    "i.id, i.identifier, i.title, i.priority_label, i.description,
            i.created_at, i.updated_at,
            i.state_id, s.name,
            i.assignee_id, ua.name,
            i.team_id, t.name,
            i.project_id, p.name,
            i.cycle_id, c.name,
            i.creator_id, uc.name,
            i.parent_id, pp.identifier,
            (SELECT GROUP_CONCAT(l.name, ',') FROM issue_labels il
               JOIN labels l ON l.id = il.label_id WHERE il.issue_id = i.id)";

/// The entity joins that reconstruct an issue's referenced rows. The base table
/// is aliased `i`; callers prepend `FROM issues i` (optionally with an FTS join)
/// before this fragment.
pub(crate) const ISSUE_JOINS: &str = "JOIN workflow_states s ON s.id = i.state_id
         JOIN teams t            ON t.id = i.team_id
         LEFT JOIN users ua      ON ua.id = i.assignee_id
         LEFT JOIN projects p    ON p.id = i.project_id
         LEFT JOIN cycles c      ON c.id = i.cycle_id
         LEFT JOIN users uc      ON uc.id = i.creator_id
         LEFT JOIN issues pp     ON pp.id = i.parent_id";

/// Reconstruct a [`types::Issue`] from a row in [`ISSUE_COLUMNS`] order.
pub(crate) fn issue_from_row(row: &rusqlite::Row) -> rusqlite::Result<types::Issue> {
    let priority_label: String = row.get(3)?;
    let priority = types::priority_label_to_u8(&priority_label);

    let created_at: String = row.get(5)?;
    let updated_at: String = row.get(6)?;

    let assignee_id: Option<String> = row.get(9)?;
    let assignee_name: Option<String> = row.get(10)?;
    let project_id: Option<String> = row.get(13)?;
    let project_name: Option<String> = row.get(14)?;
    let cycle_id: Option<String> = row.get(15)?;
    let cycle_name: Option<String> = row.get(16)?;
    let creator_id: Option<String> = row.get(17)?;
    let creator_name: Option<String> = row.get(18)?;
    let parent_id: Option<String> = row.get(19)?;
    let parent_identifier: Option<String> = row.get(20)?;
    let labels: Option<String> = row.get(21)?;

    let state_id: String = row.get(7)?;
    let team_id: String = row.get(11)?;

    Ok(types::Issue {
        id: lt_types::Id::new(row.get::<_, String>(0)?),
        identifier: row.get(1)?,
        title: row.get(2)?,
        priority_label,
        priority: Priority(priority),
        state: types::WorkflowState {
            id: lt_types::Id::new(state_id),
            name: row.get(8)?,
        },
        assignee: assignee_id.map(|id| types::User {
            id: lt_types::Id::new(id),
            name: assignee_name.unwrap_or_default(),
        }),
        team: types::Team {
            id: lt_types::Id::new(team_id),
            name: row.get(12)?,
        },
        description: row.get(4)?,
        labels: types::LabelConnection {
            nodes: labels
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|n| types::Label {
                    id: lt_types::Id::new(String::new()),
                    name: n.to_string(),
                })
                .collect(),
        },
        project: project_id.map(|id| types::Project {
            id: lt_types::Id::new(id),
            name: project_name.unwrap_or_default(),
        }),
        cycle: cycle_id.map(|id| types::Cycle {
            id: lt_types::Id::new(id),
            name: cycle_name,
        }),
        creator: creator_id.map(|id| types::User {
            id: lt_types::Id::new(id),
            name: creator_name.unwrap_or_default(),
        }),
        parent: parent_id.map(|id| types::Parent {
            id: lt_types::Id::new(id),
            identifier: parent_identifier.unwrap_or_default(),
        }),
        created_at: parse_datetime_column(&created_at)?,
        updated_at: parse_datetime_column(&updated_at)?,
    })
}

/// Upsert one `(id, name)` row into a named entity table, updating the name on
/// id conflict (so a rename touches a single row). `name` is optional because
/// `cycles.name` is nullable; the other tables always pass `Some`.
pub(crate) fn upsert_named_entity(
    conn: &Connection,
    table: &str,
    id: &str,
    name: Option<&str>,
) -> Result<()> {
    let sql = format!(
        "INSERT INTO {table} (id, name) VALUES (?1, ?2)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name"
    );
    conn.execute(&sql, params![id, name])
        .with_context(|| format!("failed to upsert {table}"))?;
    Ok(())
}

/// Upsert fetched issue fragments into the relational base: upsert each
/// referenced entity, write the issue row with its FK columns, and rebuild its
/// label links. Runs in one transaction per call.
///
/// This is the sole source of the normalized base. A team rename touches one
/// `teams` row; entities the UI later needs are already stored.
pub fn upsert_issues(conn: &Connection, issues: &[types::Issue]) -> Result<()> {
    let synced_at = Utc::now().to_rfc3339();
    let tx = conn
        .unchecked_transaction()
        .context("failed to begin issue upsert transaction")?;

    for issue in issues {
        upsert_issue_tx(&tx, issue, &synced_at)?;
    }

    tx.commit().context("failed to commit issue upsert")?;
    Ok(())
}

/// Upsert a single issue fragment into the relational base within an existing
/// transaction: its referenced entities, the issue row with FK columns, and its
/// label links. Shared by [`upsert_issues`] and the outbox's optimistic create.
pub(crate) fn upsert_issue_tx(
    tx: &Connection,
    issue: &types::Issue,
    synced_at: &str,
) -> Result<()> {
    upsert_named_entity(tx, "teams", issue.team.id.inner(), Some(&issue.team.name))?;
    upsert_named_entity(
        tx,
        "workflow_states",
        issue.state.id.inner(),
        Some(&issue.state.name),
    )?;
    if let Some(a) = &issue.assignee {
        upsert_named_entity(tx, "users", a.id.inner(), Some(&a.name))?;
    }
    if let Some(c) = &issue.creator {
        upsert_named_entity(tx, "users", c.id.inner(), Some(&c.name))?;
    }
    if let Some(p) = &issue.project {
        upsert_named_entity(tx, "projects", p.id.inner(), Some(&p.name))?;
    }
    if let Some(c) = &issue.cycle {
        upsert_named_entity(tx, "cycles", c.id.inner(), c.name.as_deref())?;
    }

    tx.execute(
        "INSERT OR REPLACE INTO issues
            (id, identifier, title, priority_label, description,
             created_at, updated_at, synced_at, parent_id,
             team_id, state_id, assignee_id, creator_id, project_id, cycle_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            issue.id.inner(),
            issue.identifier,
            issue.title,
            issue.priority_label,
            issue.description,
            issue.created_at.to_rfc3339_millis(),
            issue.updated_at.to_rfc3339_millis(),
            synced_at,
            issue.parent.as_ref().map(|p| p.id.inner()),
            issue.team.id.inner(),
            issue.state.id.inner(),
            issue.assignee.as_ref().map(|u| u.id.inner()),
            issue.creator.as_ref().map(|u| u.id.inner()),
            issue.project.as_ref().map(|p| p.id.inner()),
            issue.cycle.as_ref().map(|c| c.id.inner()),
        ],
    )
    .context("failed to upsert issue")?;

    tx.execute(
        "DELETE FROM issue_labels WHERE issue_id = ?1",
        params![issue.id.inner()],
    )
    .context("failed to clear issue labels")?;
    for label in &issue.labels.nodes {
        upsert_named_entity(tx, "labels", label.id.inner(), Some(&label.name))?;
        tx.execute(
            "INSERT OR IGNORE INTO issue_labels (issue_id, label_id) VALUES (?1, ?2)",
            params![issue.id.inner(), label.id.inner()],
        )
        .context("failed to link issue label")?;
    }
    Ok(())
}

/// One pending-overlay row resolved against its referenced entity name.
struct OverlayApply {
    field: String,
    value: Option<String>,
    state_name: Option<String>,
    user_name: Option<String>,
}

/// Load every pending overlay row, resolving the state/assignee name through
/// the entity tables in one query. The set is small (only un-synced edits), so
/// it is read whole and grouped in memory rather than filtered per issue list.
fn load_overlays(conn: &Connection) -> Result<HashMap<String, Vec<OverlayApply>>> {
    let mut stmt = conn
        .prepare(
            "SELECT po.entity_id, po.field, po.value, ws.name, u.name
             FROM pending_overlay po
             LEFT JOIN workflow_states ws ON po.field = 'state'    AND ws.id = po.value
             LEFT JOIN users u           ON po.field = 'assignee' AND u.id  = po.value",
        )
        .context("failed to prepare overlay merge query")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                OverlayApply {
                    field: r.get(1)?,
                    value: r.get(2)?,
                    state_name: r.get(3)?,
                    user_name: r.get(4)?,
                },
            ))
        })
        .context("failed to query overlays")?;
    let mut map: HashMap<String, Vec<OverlayApply>> = HashMap::new();
    for row in rows {
        let (id, apply) = row.context("failed to read overlay row")?;
        map.entry(id).or_default().push(apply);
    }
    Ok(map)
}

/// Merge the pending overlay over the base issues: overlay wins per field. This
/// is the read half of the base/overlay split -- un-synced local intent renders
/// immediately without ever being written into the base.
fn apply_overlays(conn: &Connection, issues: &mut [types::Issue]) -> Result<()> {
    let map = load_overlays(conn)?;
    if map.is_empty() {
        return Ok(());
    }
    for issue in issues {
        let Some(rows) = map.get(issue.id.inner()) else {
            continue;
        };
        for o in rows {
            match o.field.as_str() {
                "state" => {
                    if let Some(id) = &o.value {
                        issue.state = types::WorkflowState {
                            id: lt_types::Id::new(id.clone()),
                            name: o.state_name.clone().unwrap_or_default(),
                        };
                    }
                }
                "priority" => {
                    if let Some(p) = o.value.as_deref().and_then(|v| v.parse::<u8>().ok()) {
                        issue.priority = Priority(p);
                        issue.priority_label = types::priority_u8_to_label(p).to_string();
                    }
                }
                "assignee" => {
                    issue.assignee = o.value.as_ref().map(|id| types::User {
                        id: lt_types::Id::new(id.clone()),
                        name: o.user_name.clone().unwrap_or_default(),
                    });
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Query issues from the local DB, applying the WHERE clause built from the
/// `IssueQuery` filter fields (bd-2km) and ORDER BY from the sort fields.
///
/// An `--assignee=me` filter must be resolved to the viewer's name by the
/// caller before calling this (see `issues::list::resolve_me`).
pub fn query_issues(conn: &Connection, args: &IssueQuery) -> Result<Vec<types::Issue>> {
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
        "SELECT {ISSUE_COLUMNS}
         FROM issues i
         {ISSUE_JOINS}
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
    apply_overlays(conn, &mut issues)?;
    Ok(issues)
}

/// Query issues with an explicit row offset for pagination.
///
/// Returns up to `args.limit` rows starting at `offset`, plus a boolean
/// indicating whether a next page exists.
pub fn query_issues_page(
    conn: &Connection,
    args: &IssueQuery,
    offset: i64,
) -> Result<(Vec<types::Issue>, bool)> {
    let order_col = crate::db::filters::sort_column(&args.sort);
    let direction = if args.desc { "DESC" } else { "ASC" };
    // Fetch one extra row to detect whether there is a next page.
    let cap = args.limit.min(250);
    let fetch_limit = i64::from(cap) + 1;

    let sql = format!(
        "SELECT {ISSUE_COLUMNS}
         FROM issues i
         {ISSUE_JOINS}
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
    apply_overlays(conn, &mut issues)?;
    Ok((issues, has_next))
}

/// Run a single-parameter `SELECT` and map each row via `issue_from_row`.
///
/// `what` names the query for error context.
fn query_issues_one(
    conn: &Connection,
    sql: &str,
    param: &str,
    what: &str,
) -> Result<Vec<types::Issue>> {
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
    apply_overlays(conn, &mut issues)?;
    Ok(issues)
}

/// Search issues using FTS5 full-text search.
///
/// `query` supports FTS5 syntax: prefix queries (`oauth*`), phrase queries
/// (`"oauth token"`), and boolean operators (`oauth AND token`).
///
/// Results are returned ordered by FTS5 rank (best match first), capped at
/// `limit` rows.
pub fn search_issues(conn: &Connection, query: &str, limit: usize) -> Result<Vec<types::Issue>> {
    let sql = format!(
        "SELECT {ISSUE_COLUMNS}
         FROM issues i
         JOIN issues_fts ON issues_fts.rowid = i.rowid
         {ISSUE_JOINS}
         WHERE issues_fts MATCH ?1
         ORDER BY rank
         LIMIT {limit}"
    );
    query_issues_one(conn, &sql, query, "search_issues")
}

/// Approximate fallback search when the FTS index is empty: match `query` as a
/// substring of the title, capped at `limit` rows.
pub fn search_issues_like(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<types::Issue>> {
    let pattern = format!("%{query}%");
    let sql = format!(
        "SELECT {ISSUE_COLUMNS}
         FROM issues i
         {ISSUE_JOINS}
         WHERE i.title LIKE ?1
         LIMIT {limit}"
    );
    query_issues_one(conn, &sql, &pattern, "search_issues_like")
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
pub fn query_children(conn: &Connection, parent_id: &str) -> Result<Vec<types::Issue>> {
    let sql = format!(
        "SELECT {ISSUE_COLUMNS}
         FROM issues i
         {ISSUE_JOINS}
         WHERE i.parent_id = ?1
         ORDER BY i.identifier ASC"
    );
    query_issues_one(conn, &sql, parent_id, "query_children")
}

/// Look up a single issue by id, for the detail pane's parent reference.
/// Returns `None` when no issue with that id is cached.
pub fn query_issue_by_id(conn: &Connection, id: &str) -> Result<Option<types::Issue>> {
    let sql = format!(
        "SELECT {ISSUE_COLUMNS}
         FROM issues i
         {ISSUE_JOINS}
         WHERE i.id = ?1"
    );
    let mut issues = query_issues_one(conn, &sql, id, "query_issue_by_id")?;
    Ok(issues.pop())
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

    /// A list-shaped issue: state/assignee/team carry ids equal to their names
    /// so the relational upsert produces one entity row per distinct name.
    fn test_issue(id: &str, assignee: Option<&str>, state: &str) -> types::Issue {
        types::Issue {
            id: lt_types::Id::new(id),
            identifier: format!("ENG-{id}"),
            title: format!("issue {id}"),
            priority_label: "Medium".to_string(),
            priority: Priority(3),
            state: types::WorkflowState {
                id: lt_types::Id::new(state),
                name: state.to_string(),
            },
            assignee: assignee.map(|n| types::User {
                id: lt_types::Id::new(n),
                name: n.to_string(),
            }),
            team: types::Team {
                id: lt_types::Id::new("ENG"),
                name: "Engineering".to_string(),
            },
            description: None,
            labels: types::LabelConnection { nodes: Vec::new() },
            project: None,
            cycle: None,
            creator: None,
            parent: None,
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-02T00:00:00Z".parse().unwrap(),
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
    /// the reconstruction and relational-upsert tests.
    fn sample_api_issue() -> types::Issue {
        types::Issue {
            id: lt_types::Id::new("1"),
            identifier: "ENG-1".to_string(),
            title: "Wire it up".to_string(),
            priority_label: "High".to_string(),
            priority: Priority(2),
            state: types::WorkflowState {
                id: lt_types::Id::new("s1"),
                name: "In Progress".to_string(),
            },
            assignee: Some(types::User {
                id: lt_types::Id::new("u1"),
                name: "Alice".to_string(),
            }),
            team: types::Team {
                id: lt_types::Id::new("ENG"),
                name: "Engineering".to_string(),
            },
            description: Some("body".to_string()),
            labels: types::LabelConnection {
                nodes: vec![
                    types::Label {
                        id: lt_types::Id::new("l-bug"),
                        name: "bug".to_string(),
                    },
                    types::Label {
                        id: lt_types::Id::new("l-backend"),
                        name: "backend".to_string(),
                    },
                ],
            },
            project: Some(types::Project {
                id: lt_types::Id::new("p1"),
                name: "Platform".to_string(),
            }),
            cycle: Some(types::Cycle {
                id: lt_types::Id::new("c1"),
                name: Some("Cycle 7".to_string()),
            }),
            creator: Some(types::User {
                id: lt_types::Id::new("u2"),
                name: "Carol".to_string(),
            }),
            parent: Some(types::Parent {
                id: lt_types::Id::new("9"),
                identifier: "ENG-9".to_string(),
            }),
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-02T00:00:00Z".parse().unwrap(),
        }
    }

    fn graph_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        // The parent referenced by sample_api_issue must exist for the parent
        // self-join to resolve its identifier.
        let mut parent = sample_api_issue();
        parent.id = lt_types::Id::new("9");
        parent.identifier = "ENG-9".to_string();
        parent.parent = None;
        upsert_issues(&conn, &[parent, sample_api_issue()]).unwrap();
        conn
    }

    #[test]
    fn reconstructs_issue_fragment_from_joins() {
        let conn = graph_db();
        let args = IssueQuery {
            title: Some("Wire it up".to_string()),
            ..Default::default()
        };
        let issues = query_issues(&conn, &args).unwrap();
        let issue = issues.iter().find(|i| i.id.inner() == "1").unwrap();

        assert_eq!(issue.identifier, "ENG-1");
        assert_eq!(issue.priority, Priority(2));
        assert_eq!(issue.state.name, "In Progress");
        assert_eq!(
            issue.assignee.as_ref().map(|u| u.name.as_str()),
            Some("Alice")
        );
        assert_eq!(issue.team.name, "Engineering");
        assert_eq!(
            issue.project.as_ref().map(|p| p.name.as_str()),
            Some("Platform")
        );
        assert_eq!(
            issue.cycle.as_ref().and_then(|c| c.name.as_deref()),
            Some("Cycle 7")
        );
        assert_eq!(
            issue.creator.as_ref().map(|u| u.name.as_str()),
            Some("Carol")
        );
        assert_eq!(
            issue.parent.as_ref().map(|p| p.identifier.as_str()),
            Some("ENG-9")
        );
        let mut names: Vec<&str> = issue.labels.nodes.iter().map(|l| l.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["backend", "bug"]);
    }

    #[test]
    fn query_issues_applies_assignee_filter() {
        let conn = test_db();
        let args = IssueQuery {
            assignee: Some("alice".to_string()),
            ..Default::default()
        };
        let issues = query_issues(&conn, &args).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(
            issues[0].assignee.as_ref().map(|u| u.name.as_str()),
            Some("Alice")
        );
    }

    #[test]
    fn query_issues_applies_no_assignee_filter() {
        let conn = test_db();
        let args = IssueQuery {
            no_assignee: true,
            ..Default::default()
        };
        let issues = query_issues(&conn, &args).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id.inner(), "3");
    }

    #[test]
    fn query_issues_applies_state_filter_and_limit() {
        let conn = test_db();
        let mut args = IssueQuery {
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
        let args = IssueQuery::default();
        let issues = query_issues(&conn, &args).unwrap();
        assert_eq!(issues.len(), 3);
    }

    #[test]
    fn upsert_populates_entities_fks_and_links() {
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

        // The issue carries no denormalized name columns anymore.
        let has_state_name: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('issues') WHERE name = 'state_name'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_state_name, 0);
    }

    #[test]
    fn read_model_merges_pending_overlay_over_base() {
        let conn = graph_db();
        // Enqueue a state + assignee-clear edit; the read model must render the
        // overlay values, not the base.
        crate::db::outbox::enqueue_state_change(&conn, "1", "s-done", "Done").unwrap();
        crate::db::outbox::enqueue_assignee_change(&conn, "1", None).unwrap();

        let args = IssueQuery {
            title: Some("Wire it up".to_string()),
            ..Default::default()
        };
        let issues = query_issues(&conn, &args).unwrap();
        let issue = issues.iter().find(|i| i.id.inner() == "1").unwrap();
        assert_eq!(issue.state.name, "Done");
        assert!(issue.assignee.is_none());

        // The base row is untouched by the overlay.
        let base_state: String = conn
            .query_row("SELECT state_id FROM issues WHERE id = '1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(base_state, "s1");
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
        updated.state = types::WorkflowState {
            id: lt_types::Id::new("s2"),
            name: "Canceled".to_string(),
        };
        upsert_issues(&conn, &[updated]).unwrap();

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
