//! The sole source of SQL statement text in this crate.
//!
//! `Sql` wraps a private `&'static str`, so a fixed statement can only come
//! into existence through the [`statements!`] macro in this module:
//! declaration and registration into the `STATEMENTS` table (used by the
//! gate's validator) happen on the same macro invocation, so an unregistered
//! statement cannot exist. Production code executes fixed statements only
//! through [`prepare`] / [`execute`], which take `Sql`, never `&str`.
//!
//! The one dynamic builder (`filters.rs::build_sql_filter`) selects a runtime
//! slice of registered [`Frag`] conditions and a registered [`SortCol`]; the
//! only way to turn those into SQL text is [`select_issues`], which produces
//! a private [`ComposedSql`] executed only through [`prepare_composed`].
//! There is no free-form SQL text splicing anywhere in `lt-storage`'s
//! production code.
//!
//! See docs/design/type-safe-sql-adr.md ("Statement registry", "Enforcement:
//! the `Sql` newtype", decisions 2-3).

use rusqlite::{Connection, Params, Statement};

/// A registered SQL statement's text. The field is private: constructible
/// only inside this module.
#[derive(Clone, Copy)]
pub(crate) struct Sql(&'static str);

/// Declares one or more registered statements: a `pub(crate) const NAME: Sql`
/// per entry, plus (test-only) a `STATEMENTS` table of `(name, statement,
/// declared param count)` the gate's validator iterates.
macro_rules! statements {
    ($(
        $(#[$meta:meta])*
        $name:ident, $params:expr, $sql:expr;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) const $name: Sql = Sql($sql);
        )*

        /// Every registered statement, for the `sql_validation` gate
        /// (docs/design/type-safe-sql-adr.md, "Validator"): name, statement,
        /// and its declared bind-parameter count.
        #[cfg(test)]
        pub(crate) const STATEMENTS: &[(&str, Sql, usize)] = &[
            $((stringify!($name), $name, $params),)*
        ];
    };
}

/// The fragment-typed read model's column list: every field
/// [`crate::types::Issue`](lt_types::types::Issue) selects, every column
/// explicitly aliased so [`crate::db::issues::issue_from_row`] reads by name
/// (ADR decision 4) rather than positional index. Labels are aggregated by a
/// correlated subquery.
macro_rules! issue_columns {
    () => {
        "i.id AS id, i.identifier AS identifier, i.title AS title, \
         i.priority_label AS priority_label, i.description AS description, \
         i.created_at AS created_at, i.updated_at AS updated_at, \
         i.state_id AS state_id, s.name AS state_name, \
         i.assignee_id AS assignee_id, ua.name AS assignee_name, \
         i.team_id AS team_id, t.name AS team_name, \
         i.project_id AS project_id, p.name AS project_name, \
         i.cycle_id AS cycle_id, c.name AS cycle_name, \
         i.creator_id AS creator_id, uc.name AS creator_name, \
         i.parent_id AS parent_id, pp.identifier AS parent_identifier, \
         (SELECT GROUP_CONCAT(l.name, ',') FROM issue_labels il \
            JOIN labels l ON l.id = il.label_id WHERE il.issue_id = i.id) AS labels"
    };
}

/// The entity joins that reconstruct an issue's referenced rows. The base
/// table is aliased `i`; callers prepend `FROM issues i` (optionally with an
/// FTS join) before this fragment.
macro_rules! issue_joins {
    () => {
        "JOIN workflow_states s ON s.id = i.state_id \
         JOIN teams t            ON t.id = i.team_id \
         LEFT JOIN users ua      ON ua.id = i.assignee_id \
         LEFT JOIN projects p    ON p.id = i.project_id \
         LEFT JOIN cycles c      ON c.id = i.cycle_id \
         LEFT JOIN users uc      ON uc.id = i.creator_id \
         LEFT JOIN issues pp     ON pp.id = i.parent_id"
    };
}

/// The shared template behind the six entity-table upsert statements below:
/// only the table name varies, so it is single-sourced here rather than
/// six near-identical literals (which `cargo dupes` would flag).
macro_rules! entity_upsert_sql {
    ($table:literal) => {
        concat!(
            "INSERT INTO ",
            $table,
            " (id, name) VALUES (?1, ?2) \
             ON CONFLICT(id) DO UPDATE SET name = excluded.name"
        )
    };
}

/// A relational entity table `upsert_named_entity`
/// ([`crate::db::issues::upsert_named_entity`]) can write to. Replaces a
/// stringly-typed table name with a closed set matched against the
/// registered upsert statements below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntityTable {
    Teams,
    Users,
    WorkflowStates,
    Projects,
    Cycles,
    Labels,
}

impl EntityTable {
    /// The registered `(id, name)` upsert statement for this table.
    pub(crate) fn upsert_sql(self) -> Sql {
        match self {
            EntityTable::Teams => UPSERT_TEAM,
            EntityTable::Users => UPSERT_USER,
            EntityTable::WorkflowStates => UPSERT_WORKFLOW_STATE,
            EntityTable::Projects => UPSERT_PROJECT,
            EntityTable::Cycles => UPSERT_CYCLE,
            EntityTable::Labels => UPSERT_LABEL,
        }
    }

    /// The table name, for error messages only.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            EntityTable::Teams => "teams",
            EntityTable::Users => "users",
            EntityTable::WorkflowStates => "workflow_states",
            EntityTable::Projects => "projects",
            EntityTable::Cycles => "cycles",
            EntityTable::Labels => "labels",
        }
    }
}

statements! {
    /// Upsert a fetched issue fragment's intrinsic and FK columns.
    UPSERT_ISSUE, 15,
        "INSERT OR REPLACE INTO issues \
            (id, identifier, title, priority_label, description, \
             created_at, updated_at, synced_at, parent_id, \
             team_id, state_id, assignee_id, creator_id, project_id, cycle_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)";

    /// Clear an issue's label links before rebuilding them.
    DELETE_ISSUE_LABELS_FOR_ISSUE, 1,
        "DELETE FROM issue_labels WHERE issue_id = ?1";

    /// Link one label to an issue; a no-op if the link already exists.
    INSERT_ISSUE_LABEL, 2,
        "INSERT OR IGNORE INTO issue_labels (issue_id, label_id) VALUES (?1, ?2)";

    /// Load every pending overlay row, resolving the state/assignee name
    /// through the entity tables in one query.
    LOAD_OVERLAYS, 0,
        "SELECT po.entity_id, po.field, po.value, ws.name, u.name \
         FROM pending_overlay po \
         LEFT JOIN workflow_states ws ON po.field = 'state'    AND ws.id = po.value \
         LEFT JOIN users u           ON po.field = 'assignee' AND u.id  = po.value";

    /// Read one `sync_meta` value by key.
    GET_META, 1,
        "SELECT value FROM sync_meta WHERE key = ?1";

    /// Insert or replace one `sync_meta` key/value pair.
    SET_META, 2,
        "INSERT OR REPLACE INTO sync_meta (key, value) VALUES (?1, ?2)";

    /// FTS5 full-text search: `?1` is the MATCH query, `?2` the row limit.
    SEARCH_ISSUES, 2,
        concat!(
            "SELECT ", issue_columns!(),
            " FROM issues i \
              JOIN issues_fts ON issues_fts.rowid = i.rowid ",
            issue_joins!(),
            " WHERE issues_fts MATCH ?1 ORDER BY rank LIMIT ?2"
        );

    /// Title-substring fallback search when the FTS index is empty: `?1` is
    /// the `LIKE` pattern, `?2` the row limit.
    SEARCH_ISSUES_LIKE, 2,
        concat!(
            "SELECT ", issue_columns!(),
            " FROM issues i ",
            issue_joins!(),
            " WHERE i.title LIKE ?1 LIMIT ?2"
        );

    /// Child issues of `?1`, oldest identifier first.
    QUERY_CHILDREN, 1,
        concat!(
            "SELECT ", issue_columns!(),
            " FROM issues i ",
            issue_joins!(),
            " WHERE i.parent_id = ?1 ORDER BY i.identifier ASC"
        );

    /// A single issue by id.
    QUERY_ISSUE_BY_ID, 1,
        concat!(
            "SELECT ", issue_columns!(),
            " FROM issues i ",
            issue_joins!(),
            " WHERE i.id = ?1"
        );

    /// Count every locally cached issue, regardless of filters.
    COUNT_ISSUES, 0,
        "SELECT COUNT(*) FROM issues";

    /// Count rows in the FTS5 shadow index.
    COUNT_FTS_ROWS, 0,
        "SELECT COUNT(*) FROM issues_fts";

    /// Count every table in the database, including SQLite's own bookkeeping
    /// tables. Used only to detect a pre-versioned database: `sqlite_master`
    /// always exists, so this prepares against any connection.
    COUNT_TABLES, 0,
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'";

    /// Upsert one `(id, name)` row into `teams`.
    UPSERT_TEAM, 2, entity_upsert_sql!("teams");
    /// Upsert one `(id, name)` row into `users`.
    UPSERT_USER, 2, entity_upsert_sql!("users");
    /// Upsert one `(id, name)` row into `workflow_states`.
    UPSERT_WORKFLOW_STATE, 2, entity_upsert_sql!("workflow_states");
    /// Upsert one `(id, name)` row into `projects`.
    UPSERT_PROJECT, 2, entity_upsert_sql!("projects");
    /// Upsert one `(id, name)` row into `cycles`.
    UPSERT_CYCLE, 2, entity_upsert_sql!("cycles");
    /// Upsert one `(id, name)` row into `labels`.
    UPSERT_LABEL, 2, entity_upsert_sql!("labels");

    /// Upsert one `(entity_id, field)` pending-overlay row.
    SET_OVERLAY, 3,
        "INSERT INTO pending_overlay (entity_id, field, value) VALUES (?1, ?2, ?3) \
         ON CONFLICT(entity_id, field) DO UPDATE SET value = excluded.value";

    /// Clear a pending outbox command of `op_type` for `entity_id`, ahead of
    /// re-inserting the coalesced replacement.
    DELETE_SUPERSEDED_PENDING, 2,
        "DELETE FROM outbox WHERE op_type = ?1 AND entity_id = ?2 AND status = 'pending'";

    /// Insert a new pending outbox command.
    INSERT_PENDING, 4,
        "INSERT INTO outbox (op_type, entity_id, variables, status, attempts, created_at) \
         VALUES (?1, ?2, ?3, 'pending', 0, ?4)";

    /// Every `(field, value)` overlay row for one issue.
    OVERLAY_ROWS, 1,
        "SELECT field, value FROM pending_overlay WHERE entity_id = ?1";

    /// Every pending outbox command, in `seq` order.
    PENDING_OPERATIONS, 0,
        "SELECT seq, op_type, entity_id, variables FROM outbox \
         WHERE status = 'pending' ORDER BY seq";

    /// Apply an acked overlay's state onto the base `issues` row.
    ACK_UPDATE_STATE, 2,
        "UPDATE issues SET state_id = ?1 WHERE id = ?2";

    /// Apply an acked overlay's assignee onto the base `issues` row.
    ACK_UPDATE_ASSIGNEE, 2,
        "UPDATE issues SET assignee_id = ?1 WHERE id = ?2";

    /// Apply an acked overlay's priority onto the base `issues` row.
    ACK_UPDATE_PRIORITY, 2,
        "UPDATE issues SET priority_label = ?1 WHERE id = ?2";

    /// Retire every overlay row for an issue once its command is acked.
    DELETE_PENDING_OVERLAY_FOR_ENTITY, 1,
        "DELETE FROM pending_overlay WHERE entity_id = ?1";

    /// Delete an issue row (used to drop the optimistic temp row on create-ack).
    DELETE_ISSUE_BY_ID, 1,
        "DELETE FROM issues WHERE id = ?1";

    /// Delete a comment row by id (used to drop the optimistic temp row on
    /// comment-create-ack).
    DELETE_ISSUE_COMMENT_BY_ID, 1,
        "DELETE FROM issue_comments WHERE id = ?1";

    /// Record a failed drain attempt against a pending outbox command.
    RECORD_ERROR, 2,
        "UPDATE outbox SET attempts = attempts + 1, last_error = ?1 WHERE seq = ?2";

    /// Retire an outbox command once its ack has been applied.
    DELETE_COMMAND, 1,
        "DELETE FROM outbox WHERE seq = ?1";

    /// Insert or replace a comment row.
    UPSERT_COMMENT, 7,
        "INSERT OR REPLACE INTO issue_comments \
            (id, issue_id, body, user_id, created_at, updated_at, synced_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";

    /// A single issue's comments, oldest first, with author name joined in.
    QUERY_COMMENTS, 1,
        "SELECT ic.id AS id, ic.body AS body, ic.created_at AS created_at, \
                ic.updated_at AS updated_at, ic.user_id AS user_id, u.name AS user_name \
         FROM issue_comments ic \
         LEFT JOIN users u ON u.id = ic.user_id \
         WHERE ic.issue_id = ?1 \
         ORDER BY ic.created_at ASC";

    /// Delete the synced comments of an issue, preserving un-acked `local:` rows.
    DELETE_COMMENTS_FOR_ISSUE, 1,
        "DELETE FROM issue_comments WHERE issue_id = ?1 AND id NOT LIKE 'local:%'";

    /// Upsert one workflow state scoped to its team. `position` is `COALESCE`d
    /// against the stored value so an issue-driven upsert (which knows only the
    /// state's team) can pass `NULL` without clobbering a position recorded by
    /// a targeted team sync.
    UPSERT_WORKFLOW_STATE_SCOPED, 4,
        "INSERT INTO workflow_states (id, name, team_id, position) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(id) DO UPDATE SET \
            name = excluded.name, \
            team_id = excluded.team_id, \
            position = COALESCE(excluded.position, workflow_states.position)";

    /// Every team, alphabetically by name.
    QUERY_TEAMS, 0,
        "SELECT id, name FROM teams ORDER BY name";

    /// A team's workflow states in Linear's stored order; states known only
    /// from issue upserts (`position IS NULL`) sort last, by name.
    QUERY_TEAM_STATES, 1,
        "SELECT id, name FROM workflow_states \
         WHERE team_id = ?1 \
         ORDER BY position IS NULL, position, name";

    /// A team's workflow states, carrying `position`, in the same order as
    /// [`QUERY_TEAM_STATES`].
    QUERY_TEAM_STATES_WITH_POSITION, 1,
        "SELECT id, name, position FROM workflow_states \
         WHERE team_id = ?1 \
         ORDER BY position IS NULL, position, name";

    /// A team's members, resolved through `team_memberships`, by name.
    QUERY_TEAM_MEMBERS, 1,
        "SELECT u.id AS id, u.name AS name \
         FROM team_memberships tm \
         JOIN users u ON u.id = tm.user_id \
         WHERE tm.team_id = ?1 \
         ORDER BY u.name";

    /// Clear a team's membership rows ahead of inserting the freshly fetched
    /// set (replace-set semantics: a member no longer on the team is removed).
    DELETE_TEAM_MEMBERSHIPS_FOR_TEAM, 1,
        "DELETE FROM team_memberships WHERE team_id = ?1";

    /// Insert one team membership row.
    INSERT_TEAM_MEMBERSHIP, 2,
        "INSERT OR IGNORE INTO team_memberships (team_id, user_id) VALUES (?1, ?2)";

    /// Sim compatibility (`lt sim`): derive `team_memberships` from the seeded
    /// issues' distinct team/assignee and team/creator pairs, since there is no
    /// real team-membership API to seed from.
    DERIVE_TEAM_MEMBERSHIPS_FROM_ISSUES, 0,
        "INSERT OR IGNORE INTO team_memberships (team_id, user_id) \
         SELECT team_id, assignee_id FROM issues \
            WHERE team_id IS NOT NULL AND assignee_id IS NOT NULL \
         UNION \
         SELECT team_id, creator_id FROM issues \
            WHERE team_id IS NOT NULL AND creator_id IS NOT NULL";
}

// ---------------------------------------------------------------------------
// Dynamic composition: registered fragments + typed composers
// ---------------------------------------------------------------------------
//
// `filters.rs::build_sql_filter` and `search_query.rs::build_conditions`
// still choose *which* WHERE clauses apply to a query at runtime -- that
// selection is inherently dynamic. What used to be free-form clause text is
// now a closed set of registered `Frag`s; composition itself (assembling
// `SELECT`/`FROM`/`JOIN`/`WHERE`/`ORDER BY`/`LIMIT` around them) happens only
// through `select_issues`/`select_issues_page` below. See
// docs/design/type-safe-sql-adr.md ("Statement registry", decision 2).

/// A registered `WHERE`-clause fragment, referencing the read-model join
/// aliases (`i` issues, `s` state, `t` team, `ua` assignee, `uc` creator,
/// `p` project, `c` cycle). The field is private: constructible only inside
/// this module (via [`fragments!`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Frag(&'static str);

/// Bind parameters for a dynamic builder's selected [`Frag`]s, in the same
/// order. A type alias so `filters.rs::build_sql_filter` and
/// `search_query.rs::build_conditions`'s return types clear
/// `clippy::type_complexity`.
pub(crate) type BindParams = Vec<Box<dyn rusqlite::types::ToSql>>;

/// Declares one or more registered fragments: a `pub(crate) const NAME: Frag`
/// per entry, plus (test-only) a `FRAGMENTS` table of `(name, fragment,
/// declared bind-param count)` the gate's validator iterates.
macro_rules! fragments {
    ($(
        $(#[$meta:meta])*
        $name:ident, $params:expr, $sql:expr;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) const $name: Frag = Frag($sql);
        )*

        /// Every registered fragment, for the `sql_validation` gate: name,
        /// fragment, and its declared bind-parameter count.
        #[cfg(test)]
        pub(crate) const FRAGMENTS: &[(&str, Frag, usize)] = &[
            $((stringify!($name), $name, $params),)*
        ];
    };
}

fragments! {
    /// `team`: case-insensitive match against the team name or key.
    FRAG_TEAM_LOWER_OR_ID, 2,
        "(LOWER(t.name) LIKE ? OR LOWER(COALESCE(i.team_id,'')) LIKE ?)";
    /// `assignee`, exact: resolved-viewer or literal-`me` match.
    FRAG_ASSIGNEE_EQ, 1, "ua.name = ?";
    /// `assignee`, substring: case-insensitive match against the name.
    FRAG_ASSIGNEE_LOWER_LIKE, 1, "LOWER(COALESCE(ua.name,'')) LIKE ?";
    /// `assignee`, null: no assignee.
    FRAG_NO_ASSIGNEE, 0, "i.assignee_id IS NULL";
    /// `state`: case-insensitive substring match.
    FRAG_STATE_LOWER_LIKE, 1, "LOWER(s.name) LIKE ?";
    /// `priority`: exact match against the normalised label.
    FRAG_PRIORITY_EQ, 1, "i.priority_label = ?";
    /// `title`: case-insensitive substring match.
    FRAG_TITLE_LIKE, 1, "i.title LIKE ?";
    /// `created_after`.
    FRAG_CREATED_AFTER, 1, "i.created_at >= ?";
    /// `created_before`.
    FRAG_CREATED_BEFORE, 1, "i.created_at < ?";
    /// `updated_after`.
    FRAG_UPDATED_AFTER, 1, "i.updated_at >= ?";
    /// `updated_before`.
    FRAG_UPDATED_BEFORE, 1, "i.updated_at < ?";
    /// `label`: any linked label whose name matches.
    FRAG_LABEL_EXISTS, 1,
        "EXISTS (SELECT 1 FROM issue_labels il JOIN labels lb ON lb.id = il.label_id \
         WHERE il.issue_id = i.id AND LOWER(lb.name) LIKE ?)";
    /// `project`: case-insensitive substring match.
    FRAG_PROJECT_LOWER_LIKE, 1, "LOWER(COALESCE(p.name,'')) LIKE ?";
    /// `cycle`: case-insensitive substring match.
    FRAG_CYCLE_LOWER_LIKE, 1, "LOWER(COALESCE(c.name,'')) LIKE ?";
    /// `creator`: case-insensitive substring match.
    FRAG_CREATOR_LOWER_LIKE, 1, "LOWER(COALESCE(uc.name,'')) LIKE ?";
}

/// A registered `ORDER BY` column, aliased to the read model's joins. The
/// field is private: constructible only inside this module (via
/// [`sort_cols!`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SortCol(&'static str);

/// Declares one or more registered sort columns: a `pub(crate) const NAME:
/// SortCol` per entry, plus (test-only) a `SORT_COLS` table the gate's
/// validator iterates.
macro_rules! sort_cols {
    ($(
        $(#[$meta:meta])*
        $name:ident, $sql:expr;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) const $name: SortCol = SortCol($sql);
        )*

        /// Every registered sort column, for the `sql_validation` gate.
        #[cfg(test)]
        pub(crate) const SORT_COLS: &[(&str, SortCol)] = &[
            $((stringify!($name), $name),)*
        ];
    };
}

sort_cols! {
    /// `sort:created` / `--sort created`.
    SORT_CREATED_AT, "i.created_at";
    /// `sort:updated` / `--sort updated` (the default).
    SORT_UPDATED_AT, "i.updated_at";
    /// `sort:priority` / `--sort priority`.
    SORT_PRIORITY_LABEL, "i.priority_label";
    /// `sort:title` / `--sort title`.
    SORT_TITLE, "i.title";
    /// `sort:assignee` / `--sort assignee`.
    SORT_ASSIGNEE_NAME, "ua.name";
    /// `sort:state` / `--sort state`.
    SORT_STATE_NAME, "s.name";
    /// `sort:team` / `--sort team`.
    SORT_TEAM_NAME, "t.name";
}

/// A composed `SELECT` built from registered pieces: the fixed
/// `ISSUE_COLUMNS`/`ISSUE_JOINS` fragments, a runtime-selected slice of
/// [`Frag`] conditions, and a [`SortCol`]. Constructible only via
/// [`select_issues`] / [`select_issues_page`] in this module.
pub(crate) struct ComposedSql(String);

/// Build the issue-shaped, offset-paginated `SELECT` behind `db::issues::query_issues`.
///
/// `conditions` are AND-joined after `WHERE`. When `fts` is set, the query
/// joins `issues_fts` and `issues_fts MATCH ?` is the first `WHERE` clause
/// (so its bind param precedes `conditions`' binds), with `conditions`
/// AND-joined after it. `LIMIT ? OFFSET ?` are always the two trailing bound
/// params the caller supplies last, in that order.
pub(crate) fn select_issues(
    fts: bool,
    conditions: &[Frag],
    order: SortCol,
    desc: bool,
) -> ComposedSql {
    let dir = if desc { "DESC" } else { "ASC" };
    let fts_join = if fts {
        " JOIN issues_fts ON issues_fts.rowid = i.rowid"
    } else {
        ""
    };

    let mut clauses: Vec<&str> = Vec::with_capacity(conditions.len() + 1);
    if fts {
        clauses.push("issues_fts MATCH ?");
    }
    clauses.extend(conditions.iter().map(|f| f.0));
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };

    ComposedSql(format!(
        "SELECT {cols} FROM issues i{fts_join} {joins}{where_sql} ORDER BY {order} {dir} LIMIT ? OFFSET ?",
        cols = issue_columns!(),
        joins = issue_joins!(),
        order = order.0,
    ))
}

/// Prepare a composed statement. The only way production code turns a
/// [`ComposedSql`] into an executable [`Statement`].
pub(crate) fn prepare_composed<'c>(
    conn: &'c Connection,
    sql: &ComposedSql,
) -> rusqlite::Result<Statement<'c>> {
    conn.prepare(&sql.0)
}

/// The 22 names `ISSUE_COLUMNS` aliases to, in order -- the shape
/// [`crate::db::issues::issue_from_row`] reads by name. Validator-only.
#[cfg(test)]
pub(crate) const ISSUE_COLUMN_NAMES: &[&str] = &[
    "id",
    "identifier",
    "title",
    "priority_label",
    "description",
    "created_at",
    "updated_at",
    "state_id",
    "state_name",
    "assignee_id",
    "assignee_name",
    "team_id",
    "team_name",
    "project_id",
    "project_name",
    "cycle_id",
    "cycle_name",
    "creator_id",
    "creator_name",
    "parent_id",
    "parent_identifier",
    "labels",
];

/// The 6 names [`QUERY_COMMENTS`] aliases to, in order -- the shape
/// `query_comments`'s row mapper reads by name. Validator-only.
#[cfg(test)]
pub(crate) const COMMENT_COLUMN_NAMES: &[&str] = &[
    "id",
    "body",
    "created_at",
    "updated_at",
    "user_id",
    "user_name",
];

/// Prepare a registered statement. The only way production code turns a
/// [`Sql`] into an executable [`Statement`].
pub(crate) fn prepare(conn: &Connection, sql: Sql) -> rusqlite::Result<Statement<'_>> {
    conn.prepare(sql.0)
}

/// Run a parameterized write statement, attaching `what` to any error.
///
/// `what` reads as the failed action, e.g. `"set sync_meta"`.
pub(crate) fn execute(
    conn: &Connection,
    sql: Sql,
    params: impl Params,
    what: &str,
) -> anyhow::Result<()> {
    use anyhow::Context;
    conn.execute(sql.0, params)
        .with_context(|| format!("failed to {what}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrated_conn() -> Connection {
        let db = crate::db::Database::memory().unwrap();
        db.connect().unwrap()
    }

    /// The fixed-statement slice of the gate's schema-adherence validator
    /// (docs/design/type-safe-sql-adr.md, "Validator"): every registered
    /// statement must prepare against the real, migrated schema (P1), and
    /// its declared bind-parameter count must match what SQLite reports
    /// (P2, const side).
    #[test]
    fn every_registered_statement_prepares_and_matches_its_declared_param_count() {
        let conn = migrated_conn();
        for (name, sql, declared_params) in STATEMENTS {
            let stmt = conn
                .prepare(sql.0)
                .unwrap_or_else(|e| panic!("failed to prepare {name}: {e}"));
            assert_eq!(
                stmt.parameter_count(),
                *declared_params,
                "{name}: declared param count does not match the prepared statement"
            );
        }
    }

    #[test]
    fn query_issue_by_id_columns_match_the_named_row_mapping() {
        let conn = migrated_conn();
        let stmt = conn.prepare(QUERY_ISSUE_BY_ID.0).unwrap();
        assert_eq!(stmt.column_names().as_slice(), ISSUE_COLUMN_NAMES);
    }

    #[test]
    fn query_comments_columns_match_the_named_row_mapping() {
        let conn = migrated_conn();
        let stmt = conn.prepare(QUERY_COMMENTS.0).unwrap();
        assert_eq!(stmt.column_names().as_slice(), COMMENT_COLUMN_NAMES);
    }

    /// The fragment slice of the validator: every registered `Frag` must
    /// prepare inside [`select_issues`]'s template (P1), and the composed
    /// statement's parameter count must equal the fragment's declared count
    /// plus one (the trailing `LIMIT` bind).
    #[test]
    fn every_fragment_prepares_inside_select_issues() {
        let conn = migrated_conn();
        for (name, frag, declared_params) in FRAGMENTS {
            let composed = select_issues(false, std::slice::from_ref(frag), SORT_UPDATED_AT, true);
            let stmt = conn
                .prepare(&composed.0)
                .unwrap_or_else(|e| panic!("failed to prepare fragment {name}: {e}"));
            assert_eq!(
                stmt.parameter_count(),
                *declared_params + 2,
                "{name}: declared param count + 2 (LIMIT, OFFSET) does not match the composed statement"
            );
        }
    }

    /// The FTS template: `select_issues(true, &[], ...)` must prepare with
    /// exactly the `MATCH`, `LIMIT`, and `OFFSET` binds (no conditions).
    #[test]
    fn fts_template_prepares_with_match_limit_and_offset_params() {
        let conn = migrated_conn();
        let composed = select_issues(true, &[], SORT_UPDATED_AT, true);
        let stmt = conn.prepare(&composed.0).unwrap();
        assert_eq!(stmt.parameter_count(), 3);
    }

    /// Every registered sort column must prepare, with `LIMIT`/`OFFSET` as
    /// the only params when there are no conditions.
    #[test]
    fn every_sort_col_prepares_with_limit_and_offset_params() {
        let conn = migrated_conn();
        for (name, col) in SORT_COLS {
            let composed = select_issues(false, &[], *col, true);
            let stmt = conn
                .prepare(&composed.0)
                .unwrap_or_else(|e| panic!("failed to prepare {name} via select_issues: {e}"));
            assert_eq!(stmt.parameter_count(), 2);
        }
    }
}
