//! The sole source of hand-written SQL statement text in this crate.
//!
//! `Sql` wraps a private `&'static str`, so a fixed statement can only come
//! into existence through the [`statements!`] macro in this module.
//! Production code executes fixed statements only through [`prepare`] /
//! [`execute`], which take `Sql`, never `&str`.
//!
//! The one dynamic builder (`filters.rs::build_sql_filter`) selects a runtime
//! slice of registered [`Frag`] conditions and a registered [`SortCol`]; the
//! only way to turn those into SQL text is [`select_issues`], which produces
//! a private [`ComposedSql`] executed only through [`prepare_composed`].
//! There is no free-form SQL text splicing anywhere in this module's
//! production code.
//!
//! Per-entity CRUD (upsert/update/delete/select over the tables `build.rs`
//! generates from `lt-upstream`'s entity fragments) is generated SQL text,
//! not hand-written -- see `db::crud`.

use rusqlite::{Connection, Params, Statement};

/// A registered SQL statement's text. The field is private: constructible
/// only inside this module.
#[derive(Clone, Copy)]
pub(crate) struct Sql(&'static str);

/// Declares one or more registered statements: a `pub(crate) const NAME: Sql`
/// per entry.
macro_rules! statements {
    ($(
        $(#[$meta:meta])*
        $name:ident, $sql:expr;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) const $name: Sql = Sql($sql);
        )*
    };
}

/// The fragment-typed read model's column list: every field
/// [`crate::types::Issue`](lt_upstream::query::types::Issue) selects, every
/// column explicitly aliased so [`crate::db::issues::issue_from_row`] reads
/// by name (ADR decision 4) rather than positional index. Labels are
/// aggregated by a correlated subquery.
macro_rules! issue_columns {
    () => {
        "i.id AS id, i.identifier AS identifier, i.title AS title, \
         i.priority_label AS priority_label, i.priority AS priority, \
         i.description AS description, \
         i.created_at AS created_at, i.updated_at AS updated_at, \
         i.state_id AS state_id, s.name AS state_name, s.position AS state_position, \
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
/// FTS join) before this fragment. The state join stays `INNER`: a skeleton
/// issue (not yet synced) has `state_id` pointing at the sentinel row and is
/// dropped from every joined read.
macro_rules! issue_joins {
    () => {
        "JOIN workflow_states s ON s.id = i.state_id \
         JOIN teams t ON t.id = i.team_id \
         LEFT JOIN users ua ON ua.id = i.assignee_id \
         LEFT JOIN projects p ON p.id = i.project_id \
         LEFT JOIN cycles c ON c.id = i.cycle_id \
         LEFT JOIN users uc ON uc.id = i.creator_id \
         LEFT JOIN issues pp ON pp.id = i.parent_id"
    };
}

statements! {
    /// Clear an issue's label links before rebuilding them.
    DELETE_ISSUE_LABELS_FOR_ISSUE,
        "DELETE FROM issue_labels WHERE issue_id = ?1";

    /// Link one label to an issue; a no-op if the link already exists.
    INSERT_ISSUE_LABEL,
        "INSERT OR IGNORE INTO issue_labels (issue_id, label_id) VALUES (?1, ?2)";

    /// Upsert one `(id, name)` row into the reference-data `labels` table.
    /// Not part of the generated entity CRUD: `IssueLabel` implements `Node`
    /// but its storage table is this hand-authored reference table, not a
    /// schema-driven one (`build.rs`).
    UPSERT_LABEL,
        "INSERT INTO labels (id, name) VALUES (?1, ?2) \
         ON CONFLICT(id) DO UPDATE SET name = excluded.name";

    /// Read one `sync_meta` value by key.
    GET_META,
        "SELECT value FROM sync_meta WHERE key = ?1";

    /// Insert or replace one `sync_meta` key/value pair.
    SET_META,
        "INSERT OR REPLACE INTO sync_meta (key, value) VALUES (?1, ?2)";

    /// FTS5 full-text search: `?1` is the MATCH query, `?2` the row limit.
    SEARCH_ISSUES,
        concat!(
            "SELECT ", issue_columns!(),
            " FROM issues i \
              JOIN issues_fts ON issues_fts.rowid = i.rowid ",
            issue_joins!(),
            " WHERE issues_fts MATCH ?1 ORDER BY rank LIMIT ?2"
        );

    /// Title-substring fallback search when the FTS index is empty: `?1` is
    /// the `LIKE` pattern, `?2` the row limit.
    SEARCH_ISSUES_LIKE,
        concat!(
            "SELECT ", issue_columns!(),
            " FROM issues i ",
            issue_joins!(),
            " WHERE i.title LIKE ?1 LIMIT ?2"
        );

    /// Child issues of `?1`, oldest identifier first.
    QUERY_CHILDREN,
        concat!(
            "SELECT ", issue_columns!(),
            " FROM issues i ",
            issue_joins!(),
            " WHERE i.parent_id = ?1 ORDER BY i.identifier ASC"
        );

    /// A single issue by id.
    QUERY_ISSUE_BY_ID,
        concat!(
            "SELECT ", issue_columns!(),
            " FROM issues i ",
            issue_joins!(),
            " WHERE i.id = ?1"
        );

    /// Count every locally cached issue, regardless of filters. A skeleton
    /// row (not yet synced) carries the empty-string title sentinel and is
    /// excluded.
    COUNT_ISSUES,
        "SELECT COUNT(*) FROM issues WHERE title != ''";

    /// Count rows in the FTS5 shadow index.
    COUNT_FTS_ROWS,
        "SELECT COUNT(*) FROM issues_fts";

    /// Upsert the viewer's organization row.
    UPSERT_ORGANIZATION,
        "INSERT INTO organizations (id, name, url_key) VALUES (?1, ?2, ?3) \
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, url_key = excluded.url_key";

    /// Look up a single `users` row by id, for viewer reconstruction.
    QUERY_USER_BY_ID,
        "SELECT id, name FROM users WHERE id = ?1";

    /// Look up a single `organizations` row by id, for viewer reconstruction.
    QUERY_ORGANIZATION_BY_ID,
        "SELECT id, name, url_key FROM organizations WHERE id = ?1";

    /// Insert an optimistic issue-update op, coalesced: a no-op if one is
    /// already pending for this `(operation, id)`.
    INSERT_OP,
        "INSERT INTO op_log (operation, id) SELECT ?1, ?2 \
         WHERE NOT EXISTS (SELECT 1 FROM op_log WHERE operation = ?1 AND id = ?2)";

    /// Record a failed drain attempt; the op stays pending for the next sync.
    RECORD_OP_ERROR,
        "UPDATE op_log SET attempts = attempts + 1, last_error = ?1 WHERE seq = ?2";

    /// Retire an op once its ack has been applied.
    DELETE_OP,
        "DELETE FROM op_log WHERE seq = ?1";

    /// Every pending op, in `seq` order.
    PENDING_OPS,
        "SELECT seq, operation, id FROM op_log ORDER BY seq";

    /// Mint a skeleton team row (real name unknown yet) so a state or issue
    /// FK holds. `name` carries the empty-string placeholder: the generated
    /// `teams` table now declares `name NOT NULL` (`Team.name` is a required
    /// fragment field), so a bare id-only row is no longer possible. An
    /// empty name can never be a real Linear team name, so it stays a safe
    /// "not yet named" signal (see [`crate::db::teams::query_teams`]'s
    /// filter). A later [`crate::db::crud::Insert`] for `Team` overwrites it
    /// with the real name.
    MINT_TEAM,
        "INSERT OR IGNORE INTO teams (id, name) VALUES (?1, '')";

    /// Mint a skeleton user row (real name unknown yet); same empty-name
    /// placeholder rationale as [`MINT_TEAM`].
    MINT_USER,
        "INSERT OR IGNORE INTO users (id, name) VALUES (?1, '')";

    /// Mint the sentinel workflow-state row (`id = ''`, never a real Linear
    /// id) so [`MINT_ISSUE_SKELETON`]'s `state_id` -- `NOT NULL` in the
    /// generated schema -- always resolves, even though a bare FK-anchor
    /// issue carries no real state until it syncs. Idempotent; every value is
    /// a literal, so there is nothing to bind.
    MINT_SENTINEL_STATE,
        "INSERT OR IGNORE INTO workflow_states (id, name, position) VALUES ('', '', 0)";

    /// Mint a skeleton issue row (title empty) so a parent or comment FK
    /// holds, without clobbering an already-cached row. `identifier` carries
    /// `?2` when known (a parent stub's identifier), else the empty-string
    /// placeholder. `state_id`/`team_id` point at the sentinel rows
    /// [`MINT_SENTINEL_STATE`]/[`MINT_TEAM`] mint first -- both `NOT NULL`
    /// foreign keys in the generated schema, and neither is known for a bare
    /// FK anchor. A later real upsert overwrites every placeholder column.
    MINT_ISSUE_SKELETON,
        "INSERT OR IGNORE INTO issues \
            (id, identifier, title, priority_label, priority, state_id, team_id, \
             created_at, updated_at) \
         VALUES (?1, ?2, '', '', 0, '', '', '', '')";

    /// Resolve a workflow state id to itself if cached, else NULL.
    SELECT_STATE_ID_BY_ID,
        "SELECT id FROM workflow_states WHERE id = ?1";

    /// Apply the optimistic edit's state onto the base `issues` row.
    UPDATE_ISSUE_STATE,
        "UPDATE issues SET state_id = ?1 WHERE id = ?2";

    /// Apply the optimistic edit's assignee onto the base `issues` row.
    UPDATE_ISSUE_ASSIGNEE,
        "UPDATE issues SET assignee_id = ?1 WHERE id = ?2";

    /// Apply the optimistic edit's priority onto the base `issues` row: both
    /// the label and the numeric level, kept in lockstep so the read model's
    /// raw `priority` column never drifts from `priority_label`.
    UPDATE_ISSUE_PRIORITY,
        "UPDATE issues SET priority_label = ?1, priority = ?2 WHERE id = ?3";

    /// Stamp `synced_at`: after a generated entity upsert
    /// ([`crate::db::crud::Insert`]) writes an issue's intrinsic/FK columns
    /// (which do not include the storage-only `synced_at`), or to re-stamp
    /// an acked `issueUpdate` whose ack carried no server issue (the
    /// in-place edit already stands).
    SET_ISSUE_SYNCED_AT,
        "UPDATE issues SET synced_at = ?1 WHERE id = ?2";

    /// Attach a create-ack's server identity onto the optimistic row: the id
    /// change cascades (`ON UPDATE CASCADE`) to every referrer.
    ACK_ISSUE_CREATE,
        "UPDATE issues SET \
            id = ?2, identifier = ?3, title = ?4, priority_label = ?5, priority = ?6, \
            description = ?7, created_at = ?8, updated_at = ?9, synced_at = ?10, \
            parent_id = ?11, team_id = ?12, state_id = ?13, assignee_id = ?14, \
            creator_id = ?15, project_id = ?16, cycle_id = ?17 \
         WHERE id = ?1";

    /// Attach a comment create-ack's server identity onto the optimistic row.
    ACK_COMMENT_CREATE,
        "UPDATE issue_comments SET id = ?1, synced_at = ?2 WHERE id = ?3";

    /// The current state/priority/assignee of a pending `issueUpdate`'s row,
    /// for rebuilding its replay variables.
    SELECT_ISSUE_REPLAY_ROW,
        "SELECT id, state_id, priority_label, assignee_id FROM issues WHERE id = ?1";

    /// The current fields of a pending `issueCreate`'s row, for rebuilding
    /// its replay variables.
    SELECT_ISSUE_CREATE_REPLAY_ROW,
        "SELECT title, description, priority_label, team_id, state_id, assignee_id \
         FROM issues WHERE id = ?1";

    /// The current fields of a pending `commentCreate`'s row, for rebuilding
    /// its replay variables.
    SELECT_COMMENT_CREATE_REPLAY_ROW,
        "SELECT body, issue_id FROM issue_comments WHERE id = ?1";

    /// Whether a pending `issueUpdate`'s own issue has synced (is sendable).
    SENDABLE_ISSUE_UPDATE,
        "SELECT (synced_at IS NOT NULL) FROM issues WHERE id = ?1";

    /// Whether an issue is present locally, as a full (not skeleton) row, but
    /// not yet synced upstream. `title != ''` excludes a skeleton row minted
    /// only to anchor an FK (e.g. a comment's issue reference), which also
    /// carries `synced_at IS NULL` but is not an optimistic create. A missing
    /// id is not "locally unsynced" (`EXISTS` is false), distinct from
    /// [`SENDABLE_ISSUE_UPDATE`], which assumes the row exists.
    ISSUE_IS_LOCALLY_UNSYNCED,
        "SELECT EXISTS(SELECT 1 FROM issues \
         WHERE id = ?1 AND synced_at IS NULL AND title != '')";

    /// Whether a pending `commentCreate`'s target issue has synced.
    SENDABLE_COMMENT_CREATE,
        "SELECT (i.synced_at IS NOT NULL) FROM issue_comments c \
         JOIN issues i ON i.id = c.issue_id WHERE c.id = ?1";

    /// Whether a pending `issueCreate` has no locally-created (un-synced)
    /// parent blocking it.
    SENDABLE_ISSUE_CREATE,
        "SELECT NOT EXISTS ( \
            SELECT 1 FROM issues child JOIN issues p ON p.id = child.parent_id \
            WHERE child.id = ?1 AND p.synced_at IS NULL )";

    /// Insert or replace a comment row. `ON CONFLICT DO UPDATE`, not `INSERT
    /// OR REPLACE`: a REPLACE would cascade-delete via the issue FK's rowid
    /// churn.
    UPSERT_COMMENT,
        "INSERT INTO issue_comments \
            (id, issue_id, body, user_id, created_at, updated_at, synced_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(id) DO UPDATE SET \
            issue_id = excluded.issue_id, body = excluded.body, user_id = excluded.user_id, \
            created_at = excluded.created_at, updated_at = excluded.updated_at, \
            synced_at = excluded.synced_at";

    /// A single issue's comments, oldest first, with author name joined in.
    QUERY_COMMENTS,
        "SELECT ic.id AS id, ic.body AS body, ic.created_at AS created_at, \
                ic.updated_at AS updated_at, ic.user_id AS user_id, u.name AS user_name \
         FROM issue_comments ic \
         LEFT JOIN users u ON u.id = ic.user_id \
         WHERE ic.issue_id = ?1 \
         ORDER BY ic.created_at ASC";

    /// Delete the synced comments of an issue, preserving un-acked
    /// (`synced_at IS NULL`) rows.
    DELETE_COMMENTS_FOR_ISSUE,
        "DELETE FROM issue_comments WHERE issue_id = ?1 AND synced_at IS NOT NULL";

    /// Upsert one workflow state scoped to its team. Every caller -- a
    /// targeted team sync or an issue upsert's state fragment -- carries the
    /// state's real `position` (`WorkflowState.position: Float!` on the
    /// wire), so no conflict-time merge is needed.
    UPSERT_WORKFLOW_STATE_SCOPED,
        "INSERT INTO workflow_states (id, name, team_id, position) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(id) DO UPDATE SET \
            name = excluded.name, \
            team_id = excluded.team_id, \
            position = excluded.position";

    /// Every team, alphabetically by name. A skeleton row (minted by a state
    /// or issue FK, not yet named) carries the empty-string placeholder name
    /// and is excluded.
    QUERY_TEAMS,
        "SELECT id, name FROM teams WHERE name != '' ORDER BY name";

    /// A team's workflow states, carrying `position`, in Linear's stored
    /// order (ties broken by name).
    QUERY_TEAM_STATES,
        "SELECT id, name, position FROM workflow_states \
         WHERE team_id = ?1 \
         ORDER BY position, name";

    /// A team's members, resolved through `team_memberships`, by name.
    QUERY_TEAM_MEMBERS,
        "SELECT u.id AS id, u.name AS name \
         FROM team_memberships tm \
         JOIN users u ON u.id = tm.user_id \
         WHERE tm.team_id = ?1 \
         ORDER BY u.name";

    /// Clear a team's membership rows ahead of inserting the freshly fetched
    /// set (replace-set semantics: a member no longer on the team is removed).
    DELETE_TEAM_MEMBERSHIPS_FOR_TEAM,
        "DELETE FROM team_memberships WHERE team_id = ?1";

    /// Insert one team membership row.
    INSERT_TEAM_MEMBERSHIP,
        "INSERT OR IGNORE INTO team_memberships (team_id, user_id) VALUES (?1, ?2)";

    /// Sim compatibility (`lt sim`): derive `team_memberships` from the seeded
    /// issues' distinct team/assignee and team/creator pairs, since there is no
    /// real team-membership API to seed from.
    DERIVE_TEAM_MEMBERSHIPS_FROM_ISSUES,
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
/// per entry.
macro_rules! fragments {
    ($(
        $(#[$meta:meta])*
        $name:ident, $sql:expr;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) const $name: Frag = Frag($sql);
        )*
    };
}

fragments! {
    /// `team`: case-insensitive match against the team name or key.
    FRAG_TEAM_LOWER_OR_ID,
        "(LOWER(t.name) LIKE ? OR LOWER(COALESCE(i.team_id,'')) LIKE ?)";
    /// `assignee`, exact: resolved-viewer or literal-`me` match.
    FRAG_ASSIGNEE_EQ, "ua.name = ?";
    /// `assignee`, substring: case-insensitive match against the name.
    FRAG_ASSIGNEE_LOWER_LIKE, "LOWER(COALESCE(ua.name,'')) LIKE ?";
    /// `assignee`, null: no assignee.
    FRAG_NO_ASSIGNEE, "i.assignee_id IS NULL";
    /// `state`: case-insensitive substring match.
    FRAG_STATE_LOWER_LIKE, "LOWER(s.name) LIKE ?";
    /// `priority`: exact match against the normalised label.
    FRAG_PRIORITY_EQ, "i.priority_label = ?";
    /// `title`: case-insensitive substring match.
    FRAG_TITLE_LIKE, "i.title LIKE ?";
    /// `created_after`.
    FRAG_CREATED_AFTER, "i.created_at >= ?";
    /// `created_before`.
    FRAG_CREATED_BEFORE, "i.created_at < ?";
    /// `updated_after`.
    FRAG_UPDATED_AFTER, "i.updated_at >= ?";
    /// `updated_before`.
    FRAG_UPDATED_BEFORE, "i.updated_at < ?";
    /// `label`: any linked label whose name matches.
    FRAG_LABEL_EXISTS,
        "EXISTS (SELECT 1 FROM issue_labels il JOIN labels lb ON lb.id = il.label_id \
         WHERE il.issue_id = i.id AND LOWER(lb.name) LIKE ?)";
    /// `project`: case-insensitive substring match.
    FRAG_PROJECT_LOWER_LIKE, "LOWER(COALESCE(p.name,'')) LIKE ?";
    /// `cycle`: case-insensitive substring match.
    FRAG_CYCLE_LOWER_LIKE, "LOWER(COALESCE(c.name,'')) LIKE ?";
    /// `creator`: case-insensitive substring match.
    FRAG_CREATOR_LOWER_LIKE, "LOWER(COALESCE(uc.name,'')) LIKE ?";
}

/// A registered `ORDER BY` column, aliased to the read model's joins. The
/// field is private: constructible only inside this module (via
/// [`sort_cols!`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SortCol(&'static str);

/// Declares one or more registered sort columns: a `pub(crate) const NAME:
/// SortCol` per entry.
macro_rules! sort_cols {
    ($(
        $(#[$meta:meta])*
        $name:ident, $sql:expr;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) const $name: SortCol = SortCol($sql);
        )*
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

    let mut clauses: Vec<&str> = Vec::with_capacity(conditions.len() + 2);
    if fts {
        clauses.push("issues_fts MATCH ?");
    }
    clauses.push("i.title != ''");
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
