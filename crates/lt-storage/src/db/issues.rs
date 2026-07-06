use anyhow::{Context, Result};
use chrono::Utc;
use lt_types::issues::{IssueConnection, IssuesQuery, IssuesVariables};
use lt_types::pagination::PageInfo;
use lt_types::query::{SortDirection, SortField};
use lt_types::scalars::Priority;
use lt_types::types;
use rusqlite::{Connection, params};

use crate::db::ops::{EntityKey, Read, Upsert};
use crate::db::parse_datetime_column;
use crate::db::sql::{self, BindParams, EntityTable, Sql};

/// Reconstruct a [`types::Issue`] from a row selected by
/// [`sql::QUERY_ISSUE_BY_ID`] (or any other statement or composed query built
/// from the same issue-columns/joins template), reading each field by its
/// column alias.
pub(crate) fn issue_from_row(row: &rusqlite::Row) -> rusqlite::Result<types::Issue> {
    let priority_label: String = row.get("priority_label")?;
    let priority = Priority::from_label(&priority_label);

    let created_at: String = row.get("created_at")?;
    let updated_at: String = row.get("updated_at")?;

    let assignee_id: Option<String> = row.get("assignee_id")?;
    let assignee_name: Option<String> = row.get("assignee_name")?;
    let project_id: Option<String> = row.get("project_id")?;
    let project_name: Option<String> = row.get("project_name")?;
    let cycle_id: Option<String> = row.get("cycle_id")?;
    let cycle_name: Option<String> = row.get("cycle_name")?;
    let creator_id: Option<String> = row.get("creator_id")?;
    let creator_name: Option<String> = row.get("creator_name")?;
    let parent_id: Option<String> = row.get("parent_id")?;
    let parent_identifier: Option<String> = row.get("parent_identifier")?;
    let labels: Option<String> = row.get("labels")?;

    let state_id: String = row.get("state_id")?;
    let team_id: String = row.get("team_id")?;

    Ok(types::Issue {
        id: row.get::<_, String>("id")?.into(),
        identifier: row.get("identifier")?,
        title: row.get("title")?,
        priority_label,
        priority,
        state: types::WorkflowState {
            id: state_id.into(),
            name: row.get("state_name")?,
            position: row.get("state_position")?,
        },
        assignee: assignee_id.map(|id| types::User {
            id: id.into(),
            name: assignee_name.unwrap_or_default(),
        }),
        team: types::Team {
            id: team_id.into(),
            name: row.get("team_name")?,
        },
        description: row.get("description")?,
        labels: types::IssueLabelConnection {
            nodes: labels
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|n| types::IssueLabel {
                    id: String::new().into(),
                    name: n.to_string(),
                })
                .collect(),
        },
        project: project_id.map(|id| types::Project {
            id: id.into(),
            name: project_name.unwrap_or_default(),
        }),
        cycle: cycle_id.map(|id| types::Cycle {
            id: id.into(),
            name: cycle_name,
        }),
        creator: creator_id.map(|id| types::User {
            id: id.into(),
            name: creator_name.unwrap_or_default(),
        }),
        parent: parent_id.map(|id| types::Parent {
            id: id.into(),
            identifier: parent_identifier.unwrap_or_default(),
        }),
        created_at: parse_datetime_column(&created_at)?,
        updated_at: parse_datetime_column(&updated_at)?,
    })
}

/// Upsert one `(id, name)` row into `table`, updating the name on id conflict
/// (so a rename touches a single row). `name` is optional because
/// `cycles.name` is nullable; the other tables always pass `Some`.
pub(crate) fn upsert_named_entity(
    conn: &Connection,
    table: EntityTable,
    id: &str,
    name: Option<&str>,
) -> Result<()> {
    sql::execute(
        conn,
        table.upsert_sql(),
        params![id, name],
        &format!("upsert {}", table.as_str()),
    )
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
    upsert_named_entity(
        tx,
        EntityTable::Teams,
        issue.team.id.inner(),
        Some(&issue.team.name),
    )?;
    if let Some(a) = &issue.assignee {
        upsert_named_entity(tx, EntityTable::Users, a.id.inner(), Some(&a.name))?;
    }
    if let Some(c) = &issue.creator {
        upsert_named_entity(tx, EntityTable::Users, c.id.inner(), Some(&c.name))?;
    }
    if let Some(p) = &issue.project {
        upsert_named_entity(tx, EntityTable::Projects, p.id.inner(), Some(&p.name))?;
    }
    if let Some(c) = &issue.cycle {
        upsert_named_entity(tx, EntityTable::Cycles, c.id.inner(), c.name.as_deref())?;
    }

    sql::execute(
        tx,
        sql::UPSERT_ISSUE,
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
        "upsert issue",
    )?;

    sql::execute(
        tx,
        sql::DELETE_ISSUE_LABELS_FOR_ISSUE,
        params![issue.id.inner()],
        "clear issue labels",
    )?;
    for label in &issue.labels.nodes {
        upsert_named_entity(tx, EntityTable::Labels, label.id.inner(), Some(&label.name))?;
        sql::execute(
            tx,
            sql::INSERT_ISSUE_LABEL,
            params![issue.id.inner(), label.id.inner()],
            "link issue label",
        )?;
    }
    Ok(())
}

/// The default page size when `vars.first` is absent.
const DEFAULT_PAGE_SIZE: i32 = 50;

/// Query issues from the local DB: applies the WHERE clause built from
/// `vars.filter` (and its FTS5 term, if set) and the ORDER BY from
/// `vars.sort`, defaulting to `updated DESC`. `vars.after` is a stringified
/// row offset (defaulting to 0); `vars.first` caps the page (defaulting to
/// [`DEFAULT_PAGE_SIZE`], capped at 250). Fetches one extra row to detect
/// `has_next_page`, so filtered and FTS reads paginate the same as an
/// unfiltered read.
///
/// An `--assignee=me` filter must be resolved to the viewer's name by the
/// caller before calling this (see `issues::list::resolve_me`).
pub fn query_issues(conn: &Connection, vars: &IssuesVariables) -> Result<IssueConnection> {
    let (conditions, mut bind, fts_term) = vars
        .filter
        .as_ref()
        .map(crate::db::filters::build_sql_filter)
        .unwrap_or_default();

    let (order, desc) = vars.sort.as_ref().map_or(
        (crate::db::filters::sort_column(&SortField::Updated), true),
        |s| {
            (
                crate::db::filters::sort_column(&s.field),
                s.direction == SortDirection::Descending,
            )
        },
    );

    let cap = vars.first.unwrap_or(DEFAULT_PAGE_SIZE).clamp(0, 250);
    let fetch_limit = i64::from(cap) + 1;
    let offset: i64 = vars
        .after
        .as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let has_fts = fts_term.is_some();
    let composed = sql::select_issues(has_fts, &conditions, order, desc);
    let mut stmt = sql::prepare_composed(conn, &composed)
        .context("failed to prepare query_issues statement")?;

    let mut all_params: BindParams = if let Some(term) = fts_term {
        vec![Box::new(term)]
    } else {
        Vec::new()
    };
    all_params.append(&mut bind);
    all_params.push(Box::new(fetch_limit));
    all_params.push(Box::new(offset));

    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        all_params.iter().map(std::convert::AsRef::as_ref).collect();

    let rows = stmt
        .query_map(param_refs.as_slice(), issue_from_row)
        .context("failed to execute query_issues")?;

    let mut issues = Vec::new();
    for row in rows {
        issues.push(row.context("failed to read issue row")?);
    }

    let cap_rows = usize::try_from(cap).unwrap_or(usize::MAX);
    let has_next_page = issues.len() > cap_rows;
    if has_next_page {
        issues.truncate(cap_rows);
    }

    let end_cursor = has_next_page.then(|| (offset + i64::from(cap)).to_string());
    Ok(IssueConnection {
        nodes: issues,
        page_info: PageInfo {
            has_next_page,
            end_cursor,
        },
    })
}

/// Run a registered issue-shaped `SELECT` and map each row via
/// `issue_from_row`.
///
/// `what` names the query for error context.
fn query_issues_one(
    conn: &Connection,
    sql: Sql,
    params: impl rusqlite::Params,
    what: &str,
) -> Result<Vec<types::Issue>> {
    let mut stmt =
        sql::prepare(conn, sql).with_context(|| format!("failed to prepare {what} statement"))?;

    let rows = stmt
        .query_map(params, issue_from_row)
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
/// Results are returned ordered by FTS5 rank (best match first), capped at
/// `limit` rows.
pub fn search_issues(conn: &Connection, query: &str, limit: usize) -> Result<Vec<types::Issue>> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    query_issues_one(
        conn,
        sql::SEARCH_ISSUES,
        params![query, limit],
        "search_issues",
    )
}

/// Approximate fallback search when the FTS index is empty: match `query` as a
/// substring of the title, capped at `limit` rows.
pub fn search_issues_like(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<types::Issue>> {
    let pattern = format!("%{query}%");
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    query_issues_one(
        conn,
        sql::SEARCH_ISSUES_LIKE,
        params![pattern, limit],
        "search_issues_like",
    )
}

/// Retrieve a value from the `sync_meta` table. Returns None if key is absent.
pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt =
        sql::prepare(conn, sql::GET_META).context("failed to prepare get_meta statement")?;

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
    query_issues_one(
        conn,
        sql::QUERY_CHILDREN,
        params![parent_id],
        "query_children",
    )
}

/// Look up a single issue by id, for the issue-detail operation's parent
/// reference. Returns `None` when no issue with that id is cached.
pub fn query_issue_by_id(conn: &Connection, id: &str) -> Result<Option<types::Issue>> {
    let mut issues = query_issues_one(
        conn,
        sql::QUERY_ISSUE_BY_ID,
        params![id],
        "query_issue_by_id",
    )?;
    Ok(issues.pop())
}

/// Insert or replace a key/value pair in the `sync_meta` table.
pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    sql::execute(conn, sql::SET_META, params![key, value], "set sync_meta")
}

/// Run a registered zero-parameter `SELECT COUNT(*)` statement.
///
/// `what` names the query for error context.
fn count_rows(conn: &Connection, sql: Sql, what: &str) -> Result<i64> {
    let mut stmt =
        sql::prepare(conn, sql).with_context(|| format!("failed to prepare {what} statement"))?;
    stmt.query_row([], |r| r.get(0))
        .with_context(|| format!("failed to {what}"))
}

/// Count every locally cached issue, regardless of filters. Used by `lt
/// search` to detect an empty index (ADR decision 6).
pub fn count_issues(conn: &Connection) -> Result<i64> {
    count_rows(conn, sql::COUNT_ISSUES, "count issues")
}

/// Count rows in the FTS5 shadow index. Used by `lt search` to detect an
/// empty or stale index and fall back to `LIKE` search (ADR decision 6).
pub fn count_fts_rows(conn: &Connection) -> Result<i64> {
    count_rows(conn, sql::COUNT_FTS_ROWS, "count fts rows")
}

impl Read for IssuesQuery {
    fn read(conn: &Connection, vars: &Self::Variables) -> Result<Self::Output> {
        query_issues(conn, vars)
    }

    fn reads(_vars: &Self::Variables) -> Vec<EntityKey> {
        vec![EntityKey::Issue]
    }
}

/// The entity keys an issue-fragment upsert touches: `Issue`/`Teams` when
/// `nodes` is non-empty, plus one `WorkflowStates{team_id}` per distinct team
/// among them -- every issue carries a team name and a team-scoped state name
/// (`upsert_issue_tx`). Shared by [`IssuesQuery`]'s and
/// [`lt_types::detail::IssueDetailQuery`]'s `Upsert` impls so both report the
/// same honest set for the same kind of write.
pub(crate) fn issue_upsert_touched(nodes: &[types::Issue]) -> Vec<EntityKey> {
    let mut touched = Vec::new();
    if !nodes.is_empty() {
        touched.push(EntityKey::Issue);
        touched.push(EntityKey::Teams);
    }
    let mut team_ids: Vec<&str> = nodes.iter().map(|i| i.team.id.inner()).collect();
    team_ids.sort_unstable();
    team_ids.dedup();
    touched.extend(
        team_ids
            .into_iter()
            .map(|team_id| EntityKey::WorkflowStates {
                team_id: team_id.to_string(),
            }),
    );
    touched
}

impl Upsert for IssuesQuery {
    /// An issue upsert also writes its referenced team and workflow-state
    /// rows; see [`issue_upsert_touched`].
    fn upsert(
        conn: &Connection,
        _vars: &Self::Variables,
        out: &Self::Output,
    ) -> Result<Vec<EntityKey>> {
        upsert_issues(conn, &out.nodes)?;
        Ok(issue_upsert_touched(&out.nodes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A list-shaped issue: state/assignee/team carry ids equal to their names
    /// so the relational upsert produces one entity row per distinct name.
    fn test_issue(id: &str, assignee: Option<&str>, state: &str) -> types::Issue {
        types::Issue {
            id: id.into(),
            identifier: format!("ENG-{id}"),
            title: format!("issue {id}"),
            priority_label: "Medium".to_string(),
            priority: Priority(3),
            state: types::WorkflowState {
                id: state.into(),
                name: state.to_string(),
                position: 1.0,
            },
            assignee: assignee.map(|n| types::User {
                id: n.into(),
                name: n.to_string(),
            }),
            team: types::Team {
                id: "ENG".into(),
                name: "Engineering".to_string(),
            },
            description: None,
            labels: types::IssueLabelConnection { nodes: Vec::new() },
            project: None,
            cycle: None,
            creator: None,
            parent: None,
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-02T00:00:00Z".parse().unwrap(),
        }
    }

    /// Seed a team-scoped workflow state (`id`/`name` share the given
    /// value) -- sync owns workflow states, so every state a fixture's
    /// issues reference must already be locally known (issue upserts never
    /// write them) for the read model's `JOIN` to resolve the row.
    fn seed_state(conn: &Connection, team_id: &str, name: &str, position: f64) {
        crate::db::teams::upsert_team_state(
            conn,
            team_id,
            &types::WorkflowState {
                id: name.into(),
                name: name.to_string(),
                position,
            },
        )
        .unwrap();
    }

    fn test_db() -> Connection {
        let db = crate::db::Database::memory().unwrap();
        let conn = db.connect().unwrap();
        seed_state(&conn, "ENG", "Todo", 1.0);
        seed_state(&conn, "ENG", "In Progress", 2.0);
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
            id: "1".into(),
            identifier: "ENG-1".to_string(),
            title: "Wire it up".to_string(),
            priority_label: "High".to_string(),
            priority: Priority(2),
            state: types::WorkflowState {
                id: "s1".into(),
                name: "In Progress".to_string(),
                position: 1.0,
            },
            assignee: Some(types::User {
                id: "u1".into(),
                name: "Alice".to_string(),
            }),
            team: types::Team {
                id: "ENG".into(),
                name: "Engineering".to_string(),
            },
            description: Some("body".to_string()),
            labels: types::IssueLabelConnection {
                nodes: vec![
                    types::IssueLabel {
                        id: "l-bug".into(),
                        name: "bug".to_string(),
                    },
                    types::IssueLabel {
                        id: "l-backend".into(),
                        name: "backend".to_string(),
                    },
                ],
            },
            project: Some(types::Project {
                id: "p1".into(),
                name: "Platform".to_string(),
            }),
            cycle: Some(types::Cycle {
                id: "c1".into(),
                name: Some("Cycle 7".to_string()),
            }),
            creator: Some(types::User {
                id: "u2".into(),
                name: "Carol".to_string(),
            }),
            parent: Some(types::Parent {
                id: "9".into(),
                identifier: "ENG-9".to_string(),
            }),
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-02T00:00:00Z".parse().unwrap(),
        }
    }

    fn graph_db() -> Connection {
        let db = crate::db::Database::memory().unwrap();
        let conn = db.connect().unwrap();
        crate::db::teams::upsert_team_state(
            &conn,
            "ENG",
            &types::WorkflowState {
                id: "s1".into(),
                name: "In Progress".to_string(),
                position: 1.0,
            },
        )
        .unwrap();
        // The parent referenced by sample_api_issue must exist for the parent
        // self-join to resolve its identifier.
        let mut parent = sample_api_issue();
        parent.id = "9".into();
        parent.identifier = "ENG-9".to_string();
        parent.parent = None;
        upsert_issues(&conn, &[parent, sample_api_issue()]).unwrap();
        conn
    }

    /// `IssuesVariables` selecting `filter`, with no sort/pagination override.
    fn vars(filter: lt_types::issues::IssueFilter) -> IssuesVariables {
        IssuesVariables {
            filter: Some(filter),
            sort: None,
            first: Some(250),
            after: None,
        }
    }

    #[test]
    fn reconstructs_issue_fragment_from_joins() {
        let conn = graph_db();
        let page = query_issues(
            &conn,
            &vars(lt_types::issues::IssueFilter {
                title: Some("Wire it up".to_string()),
                ..Default::default()
            }),
        )
        .unwrap();
        let issues = page.nodes;
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
        let issues = query_issues(
            &conn,
            &vars(lt_types::issues::IssueFilter {
                assignee: Some(lt_types::issues::AssigneeFilter::Contains(
                    "alice".to_string(),
                )),
                ..Default::default()
            }),
        )
        .unwrap()
        .nodes;
        assert_eq!(issues.len(), 1);
        assert_eq!(
            issues[0].assignee.as_ref().map(|u| u.name.as_str()),
            Some("Alice")
        );
    }

    #[test]
    fn query_issues_applies_no_assignee_filter() {
        let conn = test_db();
        let issues = query_issues(
            &conn,
            &vars(lt_types::issues::IssueFilter {
                assignee: Some(lt_types::issues::AssigneeFilter::IsNull),
                ..Default::default()
            }),
        )
        .unwrap()
        .nodes;
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id.inner(), "3");
    }

    #[test]
    fn query_issues_applies_state_filter_and_limit() {
        let conn = test_db();
        let filter = lt_types::issues::IssueFilter {
            state: Some("todo".to_string()),
            ..Default::default()
        };
        let issues = query_issues(&conn, &vars(filter.clone())).unwrap().nodes;
        assert_eq!(issues.len(), 2);

        let limited_vars = IssuesVariables {
            filter: Some(filter),
            sort: None,
            first: Some(1),
            after: None,
        };
        let issues = query_issues(&conn, &limited_vars).unwrap().nodes;
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn query_issues_without_filters_returns_all() {
        let conn = test_db();
        let issues = query_issues(
            &conn,
            &IssuesVariables {
                filter: None,
                sort: None,
                first: Some(250),
                after: None,
            },
        )
        .unwrap()
        .nodes;
        assert_eq!(issues.len(), 3);
    }

    #[test]
    fn query_issues_paginates_with_has_next_and_end_cursor() {
        let conn = test_db();
        let page = query_issues(
            &conn,
            &IssuesVariables {
                filter: None,
                sort: None,
                first: Some(2),
                after: None,
            },
        )
        .unwrap();
        assert_eq!(page.nodes.len(), 2);
        assert!(page.page_info.has_next_page);
        assert_eq!(page.page_info.end_cursor.as_deref(), Some("2"));

        let next = query_issues(
            &conn,
            &IssuesVariables {
                filter: None,
                sort: None,
                first: Some(2),
                after: page.page_info.end_cursor.as_deref().map(str::to_string),
            },
        )
        .unwrap();
        assert_eq!(next.nodes.len(), 1);
        assert!(!next.page_info.has_next_page);
        assert!(next.page_info.end_cursor.is_none());
    }

    #[test]
    fn query_issues_paginates_with_a_filter_active() {
        let conn = test_db();
        let filter = lt_types::issues::IssueFilter {
            state: Some("todo".to_string()),
            ..Default::default()
        };
        let page = query_issues(
            &conn,
            &IssuesVariables {
                filter: Some(filter),
                sort: None,
                first: Some(1),
                after: None,
            },
        )
        .unwrap();
        assert_eq!(page.nodes.len(), 1);
        assert!(page.page_info.has_next_page);
        assert_eq!(page.page_info.end_cursor.as_deref(), Some("1"));
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
        use lt_types::inputs::{Field, IssueUpdateInput};
        use lt_types::issues::{IssueUpdateMutation, IssueUpdateVariables};

        use crate::db::ops::Mutate;

        let conn = graph_db();
        // The state a picker offers is already cached by that picker's own
        // `Upsert` (`TeamStatesQuery`); mirror that precondition here.
        crate::db::teams::upsert_team_state(
            &conn,
            "ENG",
            &types::WorkflowState {
                id: "s-done".into(),
                name: "Done".to_string(),
                position: 2.0,
            },
        )
        .unwrap();

        // Enqueue a state + assignee-clear edit; the read model must render the
        // overlay values, not the base.
        IssueUpdateMutation::enqueue(
            &conn,
            IssueUpdateVariables {
                id: "1".to_string(),
                input: IssueUpdateInput {
                    state_id: Some("s-done".to_string()),
                    ..Default::default()
                },
            },
        )
        .unwrap();
        IssueUpdateMutation::enqueue(
            &conn,
            IssueUpdateVariables {
                id: "1".to_string(),
                input: IssueUpdateInput {
                    assignee_id: Field::Null,
                    ..Default::default()
                },
            },
        )
        .unwrap();

        let issues = query_issues(
            &conn,
            &vars(lt_types::issues::IssueFilter {
                title: Some("Wire it up".to_string()),
                ..Default::default()
            }),
        )
        .unwrap()
        .nodes;
        let issue = issues.iter().find(|i| i.id.inner() == "1").unwrap();
        assert_eq!(issue.state.name, "Done");
        assert_eq!(issue.state.position.to_bits(), 2.0_f64.to_bits());
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
    fn state_filter_matches_an_issue_whose_overlay_moved_it_into_the_filtered_state() {
        use lt_types::inputs::IssueUpdateInput;
        use lt_types::issues::{IssueUpdateMutation, IssueUpdateVariables};

        use crate::db::ops::Mutate;

        let conn = graph_db();
        // The issue's base state is "In Progress" (graph_db/sample_api_issue);
        // a state overlay moves it to "Done".
        seed_state(&conn, "ENG", "Done", 2.0);
        IssueUpdateMutation::enqueue(
            &conn,
            IssueUpdateVariables {
                id: "1".to_string(),
                input: IssueUpdateInput {
                    state_id: Some("Done".to_string()),
                    ..Default::default()
                },
            },
        )
        .unwrap();

        // A `state:done` filter must match the overlaid state, not the base.
        let matched = query_issues(
            &conn,
            &vars(lt_types::issues::IssueFilter {
                state: Some("done".to_string()),
                ..Default::default()
            }),
        )
        .unwrap()
        .nodes;
        assert!(matched.iter().any(|i| i.id.inner() == "1"));

        // The old base state no longer matches.
        let stale = query_issues(
            &conn,
            &vars(lt_types::issues::IssueFilter {
                state: Some("in progress".to_string()),
                ..Default::default()
            }),
        )
        .unwrap()
        .nodes;
        assert!(!stale.iter().any(|i| i.id.inner() == "1"));
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
            id: "s2".into(),
            name: "Canceled".to_string(),
            position: 3.0,
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

    #[test]
    fn issues_query_reads_only_the_issue_key() {
        let vars = IssuesVariables {
            filter: None,
            sort: None,
            first: None,
            after: None,
        };
        assert_eq!(IssuesQuery::reads(&vars), vec![EntityKey::Issue]);
    }

    #[test]
    fn issues_query_upsert_reports_issue_teams_and_workflow_states() {
        let conn = crate::db::Database::memory().unwrap().connect().unwrap();
        // The invariant this reports on: sync established the state before
        // the issue page lands.
        seed_state(&conn, "ENG", "Todo", 1.0);
        let vars = IssuesVariables {
            filter: None,
            sort: None,
            first: None,
            after: None,
        };
        let out = IssueConnection {
            nodes: vec![test_issue("1", Some("Alice"), "Todo")],
            page_info: PageInfo {
                has_next_page: false,
                end_cursor: None,
            },
        };
        let touched = IssuesQuery::upsert(&conn, &vars, &out).unwrap();
        assert_eq!(
            touched,
            vec![
                EntityKey::Issue,
                EntityKey::Teams,
                EntityKey::WorkflowStates {
                    team_id: "ENG".to_string()
                },
            ]
        );
        assert_eq!(
            query_issue_by_id(&conn, "1").unwrap().unwrap().id.inner(),
            "1"
        );
    }

    #[test]
    fn issues_query_upsert_reports_no_keys_for_an_empty_page() {
        let conn = crate::db::Database::memory().unwrap().connect().unwrap();
        let vars = IssuesVariables {
            filter: None,
            sort: None,
            first: None,
            after: None,
        };
        let out = IssueConnection {
            nodes: Vec::new(),
            page_info: PageInfo {
                has_next_page: false,
                end_cursor: None,
            },
        };
        assert!(IssuesQuery::upsert(&conn, &vars, &out).unwrap().is_empty());
    }
}
