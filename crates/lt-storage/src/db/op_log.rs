//! The local write path: `op_log`, the queue the sync drainer replays.
//!
//! An `issueUpdate` edit is applied **in place** onto the row it targets,
//! leaving `synced_at` untouched (the row was already synced, so it stays
//! immediately sendable). An `issueCreate`/`commentCreate` inserts an
//! optimistic row under a [`fabricate_id`]d id with `synced_at NULL`. Either
//! way, one op is recorded into `op_log` keyed by that id. The op log stores
//! no variables: replay (`*_replay_vars`) re-reads the row's current state to
//! build the wire variables, so a later in-place edit is automatically
//! included even if it landed after the op was first enqueued. The sync
//! drainer is the single writer that replays `op_log` against the API and
//! reconciles the row on ack (`ack_*`). Each `enqueue_*`/`*_replay_vars`/
//! `ack_*` triple here backs one op-log mutation's `lt_runtime::ops::Mutation`
//! impl (docs/design/unified-execute-adr.md, "Decision 2").

use anyhow::{Context, Result, bail};
use chrono::Utc;
use lt_upstream::query::comments::{Comment, CommentCreateMutation, CommentCreateVariables};
use lt_upstream::query::graphql::GraphqlOperation;
use lt_upstream::query::inputs::{Field, IssueCreateInput, IssueUpdateInput};
use lt_upstream::query::issues::{
    IssueCreateMutation, IssueCreateVariables, IssueUpdateMutation, IssueUpdateVariables,
};
use lt_upstream::query::scalars::Priority;
use lt_upstream::query::types;
use rusqlite::{Connection, params};

use crate::db::crud::Insert;
use crate::db::sql;

/// The optimistic identifier every locally-created issue carries until the
/// drainer's ack replaces it with the server's real one.
pub const OPTIMISTIC_ISSUE_IDENTIFIER: &str = "NEW";

/// One pending op, in `seq` order. No stored variables: replay re-reads the
/// row.
pub struct PendingOp {
    pub seq: i64,
    /// `== GraphqlOperation::NAME` of the mutation this op replays.
    pub operation: String,
    /// The `issues.id` / `issue_comments.id` the op replays.
    pub id: String,
}

/// A client-side fabricated id for an optimistic create. A plain random
/// string with no prefix; local rows are distinguished by `synced_at IS
/// NULL`.
pub fn fabricate_id() -> String {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use rand::Rng as _;
    let mut bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Ensure a skeleton user row exists for `id`, without touching `name` --
/// unlike [`crate::db::crud::Insert`], which would blank out an
/// already-known name when the caller has none to offer (an FK id offered by
/// an edit's input, not a fetched fragment).
fn mint_user(tx: &Connection, id: &str) -> Result<()> {
    sql::execute(tx, sql::MINT_USER, params![id], "mint user skeleton")
}

fn delete_op(tx: &Connection, seq: i64) -> Result<()> {
    sql::execute(tx, sql::DELETE_OP, params![seq], "delete op")
}

/// Resolve `state_id` if given and cached, falling back to the team's first
/// cached state (`query_team_states` order) -- the same default an
/// issue-create picker offers when the user leaves the state unset. Errors
/// only if the team has no cached states at all (a never-synced cache).
fn resolve_or_default_state(
    tx: &Connection,
    team_id: &str,
    state_id: Option<&str>,
) -> Result<String> {
    if let Some(id) = state_id
        && let Some(id) = crate::db::issues::resolve_state_id(tx, id)?
    {
        return Ok(id);
    }
    crate::db::teams::query_team_states(tx, team_id)?
        .into_iter()
        .next()
        .map(|s| s.id.into_inner())
        .with_context(|| format!("no workflow states cached for team {team_id} -- run `lt sync`"))
}

// ---------------------------------------------------------------------------
// IssueUpdateMutation
// ---------------------------------------------------------------------------

/// Apply each set field of `vars.input` in place (by id), minting a skeleton
/// user for a new assignee and resolving the state (sync-owned; errors if not
/// cached). Records one coalesced `issueUpdate` op. Leaves `synced_at`
/// untouched.
pub fn enqueue_issue_update(conn: &Connection, vars: IssueUpdateVariables) -> Result<String> {
    let tx = conn.unchecked_transaction()?;
    let IssueUpdateVariables { id, input } = vars;
    if let Some(state_id) = &input.state_id {
        crate::db::issues::resolve_state_id(&tx, state_id)?
            .with_context(|| format!("workflow state {state_id} not cached -- run `lt sync`"))?;
        sql::execute(
            &tx,
            sql::UPDATE_ISSUE_STATE,
            params![state_id, id],
            "apply issue state edit",
        )?;
    }
    if let Some(priority) = input.priority {
        let priority = Priority(u8::try_from(priority).unwrap_or(0));
        sql::execute(
            &tx,
            sql::UPDATE_ISSUE_PRIORITY,
            params![priority.label(), priority.0, id],
            "apply issue priority edit",
        )?;
    }
    match &input.assignee_id {
        Field::Value(uid) => {
            mint_user(&tx, uid)?;
            sql::execute(
                &tx,
                sql::UPDATE_ISSUE_ASSIGNEE,
                params![uid, id],
                "apply issue assignee edit",
            )?;
        }
        Field::Null => sql::execute(
            &tx,
            sql::UPDATE_ISSUE_ASSIGNEE,
            params![Option::<&str>::None, id],
            "clear issue assignee",
        )?,
        Field::Absent => {}
    }
    sql::execute(
        &tx,
        sql::INSERT_OP,
        params![IssueUpdateMutation::NAME, id],
        "enqueue issue update op",
    )?;
    tx.commit().context("failed to commit issue update")?;
    Ok(id)
}

/// Rebuild wire vars for a pending `issueUpdate` from the row's current state.
pub fn issue_update_replay_vars(conn: &Connection, id: &str) -> Result<IssueUpdateVariables> {
    let (row_id, state_id, priority_label, assignee_id): (
        String,
        Option<String>,
        String,
        Option<String>,
    ) = sql::prepare(conn, sql::SELECT_ISSUE_REPLAY_ROW)
        .context("failed to prepare issue replay lookup")?
        .query_row(params![id], |r| {
            Ok((
                r.get("id")?,
                r.get("state_id")?,
                r.get("priority_label")?,
                r.get("assignee_id")?,
            ))
        })
        .with_context(|| format!("issue {id} not found for replay"))?;
    Ok(IssueUpdateVariables {
        id: row_id,
        input: IssueUpdateInput {
            state_id,
            priority: Some(i32::from(Priority::from_label(&priority_label).0)),
            assignee_id: match assignee_id {
                Some(u) => Field::Value(u),
                None => Field::Null,
            },
        },
    })
}

/// Ack: re-stamp `synced_at` (server-issue upsert, or bare stamp) and retire
/// the op.
pub fn ack_issue_update(
    conn: &Connection,
    seq: i64,
    id: &str,
    out: Option<&types::Issue>,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let synced_at = Utc::now().to_rfc3339();
    if let Some(issue) = out {
        crate::db::issues::upsert_issue_tx(&tx, issue, &synced_at)?;
    } else {
        sql::execute(
            &tx,
            sql::SET_ISSUE_SYNCED_AT,
            params![synced_at, id],
            "mark issue synced",
        )?;
    }
    delete_op(&tx, seq)?;
    tx.commit().context("failed to commit issue-update ack")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// IssueCreateMutation
// ---------------------------------------------------------------------------

/// Insert an optimistic issue row (fabricated id, `synced_at NULL`), minting
/// the team/assignee skeletons and resolving/defaulting the state. Records an
/// `issueCreate` op. Returns the fabricated id.
pub fn enqueue_issue_create(conn: &Connection, vars: &IssueCreateVariables) -> Result<String> {
    let tx = conn.unchecked_transaction()?;
    let input = &vars.input;
    crate::db::teams::mint_team(&tx, &input.team_id)?;
    let state_id = resolve_or_default_state(&tx, &input.team_id, input.state_id.as_deref())?;
    if let Some(aid) = input.assignee_id.as_deref() {
        mint_user(&tx, aid)?;
    }
    let priority = Priority(
        input
            .priority
            .and_then(|p| u8::try_from(p).ok())
            .unwrap_or(0),
    );
    let now = lt_upstream::query::scalars::DateTime(Utc::now());
    let id = fabricate_id();

    // `team`/`assignee`/`state` carry only the ids `mint_team`/`mint_user`/
    // `resolve_or_default_state` already anchored above -- an empty `name`
    // placeholder, never upserted, so a real (already-synced) entity's name
    // is not clobbered by this optimistic row's `Insert`.
    let issue = types::Issue {
        id: id.clone().into(),
        identifier: OPTIMISTIC_ISSUE_IDENTIFIER.to_string(),
        title: input.title.clone(),
        priority_label: priority.label().to_string(),
        priority,
        state: types::WorkflowState {
            id: state_id.into(),
            name: String::new(),
            position: 0.0,
        },
        assignee: input.assignee_id.clone().map(|aid| types::User {
            id: aid.into(),
            name: String::new(),
        }),
        team: types::Team {
            id: input.team_id.clone().into(),
            name: String::new(),
        },
        description: input.description.clone(),
        labels: types::IssueLabelConnection { nodes: Vec::new() },
        project: None,
        cycle: None,
        creator: None,
        parent: None,
        created_at: now,
        updated_at: now,
    };
    issue.insert(&tx)?;

    sql::execute(
        &tx,
        sql::INSERT_OP,
        params![IssueCreateMutation::NAME, id],
        "enqueue issue create op",
    )?;
    tx.commit().context("failed to commit issue create")?;
    Ok(id)
}

/// Rebuild wire vars for a pending `issueCreate` from the row's current
/// state.
pub fn issue_create_replay_vars(conn: &Connection, id: &str) -> Result<IssueCreateVariables> {
    let (title, description, priority_label, team_id, state_id, assignee_id): (
        String,
        Option<String>,
        String,
        String,
        Option<String>,
        Option<String>,
    ) = sql::prepare(conn, sql::SELECT_ISSUE_CREATE_REPLAY_ROW)
        .context("failed to prepare issue-create replay lookup")?
        .query_row(params![id], |r| {
            Ok((
                r.get("title")?,
                r.get("description")?,
                r.get("priority_label")?,
                r.get("team_id")?,
                r.get("state_id")?,
                r.get("assignee_id")?,
            ))
        })
        .with_context(|| format!("issue {id} not found for create replay"))?;
    Ok(IssueCreateVariables {
        input: IssueCreateInput {
            title,
            team_id,
            description,
            state_id,
            priority: Some(i32::from(Priority::from_label(&priority_label).0)),
            assignee_id,
        },
    })
}

/// Attach the server identity onto the optimistic row: `UPDATE issues SET
/// id=…` (SQLite cascades the id to children's `parent_id` / comments'
/// `issue_id` / labels' `issue_id`), stamping `synced_at`. Rebuild labels
/// under the new id. Retire the op by `seq` (`op_log.id` is not an FK, so it
/// does not follow the cascade).
pub fn ack_issue_create(conn: &Connection, seq: i64, id: &str, issue: &types::Issue) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    crate::db::issues::ensure_issue_fks(&tx, issue)?;
    let synced_at = Utc::now().to_rfc3339();
    sql::execute(
        &tx,
        sql::ACK_ISSUE_CREATE,
        params![
            id,
            issue.id.inner(),
            issue.identifier,
            issue.title,
            issue.priority_label,
            issue.priority.0,
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
        "attach issue identity",
    )?;
    sql::execute(
        &tx,
        sql::DELETE_ISSUE_LABELS_FOR_ISSUE,
        params![issue.id.inner()],
        "clear issue labels",
    )?;
    for label in &issue.labels.nodes {
        crate::db::issues::upsert_label(&tx, label.id.inner(), &label.name)?;
        sql::execute(
            &tx,
            sql::INSERT_ISSUE_LABEL,
            params![issue.id.inner(), label.id.inner()],
            "link issue label",
        )?;
    }
    delete_op(&tx, seq)?;
    tx.commit().context("failed to commit issue-create ack")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// CommentCreateMutation
// ---------------------------------------------------------------------------

/// Insert an optimistic comment row (fabricated id, `synced_at NULL`),
/// minting the parent issue skeleton so the FK holds; author is the
/// persisted viewer, if any.
pub fn enqueue_comment_create(conn: &Connection, vars: &CommentCreateVariables) -> Result<String> {
    let tx = conn.unchecked_transaction()?;
    crate::db::issues::mint_issue_skeleton(&tx, &vars.input.issue_id, None)?;
    let user = crate::db::viewer::viewer(&tx)?.map(|v| v.user);
    if let Some(u) = &user {
        u.insert(&tx)?;
    }
    let now = lt_upstream::query::scalars::DateTime(Utc::now()).to_rfc3339_millis();
    let id = fabricate_id();
    sql::execute(
        &tx,
        sql::UPSERT_COMMENT,
        params![
            id,
            vars.input.issue_id,
            vars.input.body,
            user.as_ref().map(|u| u.id.inner()),
            now,
            now,
            Option::<&str>::None, // synced_at NULL
        ],
        "insert optimistic comment",
    )?;
    sql::execute(
        &tx,
        sql::INSERT_OP,
        params![CommentCreateMutation::NAME, id],
        "enqueue comment create op",
    )?;
    tx.commit().context("failed to commit comment create")?;
    Ok(id)
}

/// Rebuild wire vars for a pending `commentCreate`. The parent's create-ack
/// has already cascaded `issue_id` fab→server; the sendability gate blocks
/// replay until then.
pub fn comment_create_replay_vars(conn: &Connection, id: &str) -> Result<CommentCreateVariables> {
    let (body, issue_id): (String, String) =
        sql::prepare(conn, sql::SELECT_COMMENT_CREATE_REPLAY_ROW)
            .context("failed to prepare comment-create replay lookup")?
            .query_row(params![id], |r| Ok((r.get("body")?, r.get("issue_id")?)))
            .with_context(|| format!("comment {id} not found for replay"))?;
    Ok(CommentCreateVariables {
        input: lt_upstream::query::inputs::CommentCreateInput { issue_id, body },
    })
}

/// Attach the server comment id and stamp `synced_at`; retire the op.
pub fn ack_comment_create(conn: &Connection, seq: i64, id: &str, comment: &Comment) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let synced_at = Utc::now().to_rfc3339();
    sql::execute(
        &tx,
        sql::ACK_COMMENT_CREATE,
        params![comment.id.inner(), synced_at, id],
        "attach comment identity",
    )?;
    delete_op(&tx, seq)?;
    tx.commit().context("failed to commit comment-create ack")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Drain support (called by the sync drainer)
// ---------------------------------------------------------------------------

/// Every pending op, in `seq` order.
pub fn pending_operations(conn: &Connection) -> Result<Vec<PendingOp>> {
    let mut stmt =
        sql::prepare(conn, sql::PENDING_OPS).context("failed to prepare op log query")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(PendingOp {
                seq: r.get(0)?,
                operation: r.get(1)?,
                id: r.get(2)?,
            })
        })
        .context("failed to query op log")?;
    let mut ops = Vec::new();
    for row in rows {
        ops.push(row.context("failed to read op log row")?);
    }
    Ok(ops)
}

/// Record a failed drain attempt; the op stays pending for the next sync.
pub fn record_error(conn: &Connection, seq: i64, error: &str) -> Result<()> {
    sql::execute(
        conn,
        sql::RECORD_OP_ERROR,
        params![error, seq],
        "record op log error",
    )
}

/// Sendable iff every id the op references is upstream (`synced_at IS NOT
/// NULL`): own issue for `issueUpdate`; target issue for `commentCreate`;
/// locally-created parent (if any) for `issueCreate`. Consumed by the R3
/// drainer.
pub fn op_is_sendable(conn: &Connection, op: &PendingOp) -> Result<bool> {
    let stmt = match op.operation.as_str() {
        n if n == IssueUpdateMutation::NAME => sql::SENDABLE_ISSUE_UPDATE,
        n if n == IssueCreateMutation::NAME => sql::SENDABLE_ISSUE_CREATE,
        n if n == CommentCreateMutation::NAME => sql::SENDABLE_COMMENT_CREATE,
        other => bail!("unknown op operation: {other}"),
    };
    sql::prepare(conn, stmt)
        .context("failed to prepare sendability check")?
        .query_row(params![op.id], |r| r.get(0))
        .with_context(|| format!("failed to check sendability of {}", op.operation))
}

/// A minimal base issue for the write-path tests, shared with the sync
/// drainer tests so the fixture is defined once.
#[cfg(any(test, feature = "test-util"))]
pub fn sample_base_issue(id: &str) -> types::Issue {
    types::Issue {
        id: id.into(),
        identifier: format!("ENG-{id}"),
        title: format!("issue {id}"),
        priority_label: "Normal".to_string(),
        priority: lt_upstream::query::scalars::Priority(3),
        state: types::WorkflowState {
            id: "s-todo".into(),
            name: "Todo".to_string(),
            position: 1.0,
        },
        assignee: None,
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
        created_at: "2026-01-01T00:00:00Z".parse().unwrap_or_default(),
        updated_at: "2026-01-02T00:00:00Z".parse().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::{sample_base_issue as base_issue, *};
    use crate::db::Storage;

    /// A fresh in-memory database seeded with the `ENG`/`s-todo` workflow
    /// state -- sync owns workflow states, so every fixture issue's state
    /// must already be locally known before an upsert or an optimistic
    /// create resolves it.
    fn db_with_todo_state() -> Connection {
        let db = crate::db::Memory::new().unwrap();
        let conn = db.connect().unwrap();
        crate::db::teams::upsert_team_state(
            &conn,
            "ENG",
            &types::WorkflowState {
                id: "s-todo".into(),
                name: "Todo".to_string(),
                position: 1.0,
            },
        )
        .unwrap();
        conn
    }

    fn db_with_issue(id: &str) -> Connection {
        let conn = db_with_todo_state();
        crate::db::upsert_issues(&conn, &[base_issue(id)]).unwrap();
        conn
    }

    fn pending(conn: &Connection) -> Vec<PendingOp> {
        pending_operations(conn).unwrap()
    }

    fn update_vars(id: &str, input: IssueUpdateInput) -> IssueUpdateVariables {
        IssueUpdateVariables {
            id: id.to_string(),
            input,
        }
    }

    #[test]
    fn per_field_edits_coalesce_into_one_op_applied_in_place() {
        let conn = db_with_issue("1");
        enqueue_issue_update(
            &conn,
            update_vars(
                "1",
                IssueUpdateInput {
                    state_id: Some("s-todo".to_string()),
                    ..Default::default()
                },
            ),
        )
        .unwrap();
        enqueue_issue_update(
            &conn,
            update_vars(
                "1",
                IssueUpdateInput {
                    priority: Some(1),
                    ..Default::default()
                },
            ),
        )
        .unwrap();
        // Clearing the assignee must survive coalescing as an explicit null.
        enqueue_issue_update(
            &conn,
            update_vars(
                "1",
                IssueUpdateInput {
                    assignee_id: Field::Null,
                    ..Default::default()
                },
            ),
        )
        .unwrap();

        let ops = pending(&conn);
        assert_eq!(ops.len(), 1, "three edits coalesce into one issueUpdate op");
        assert_eq!(ops[0].operation, IssueUpdateMutation::NAME);
        assert_eq!(ops[0].id, "1");

        let (state_id, priority_label, assignee_id, synced_at): (
            String,
            String,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT state_id, priority_label, assignee_id, synced_at FROM issues WHERE id = '1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(state_id, "s-todo");
        assert_eq!(priority_label, "Urgent");
        assert_eq!(assignee_id, None);
        assert!(
            synced_at.is_some(),
            "an in-place edit on an already-synced issue must not clear synced_at"
        );
    }

    #[test]
    fn issue_update_replay_vars_reads_the_rows_current_state() {
        let conn = db_with_issue("1");
        enqueue_issue_update(
            &conn,
            update_vars(
                "1",
                IssueUpdateInput {
                    priority: Some(1),
                    assignee_id: Field::Null,
                    ..Default::default()
                },
            ),
        )
        .unwrap();

        let vars = issue_update_replay_vars(&conn, "1").unwrap();
        assert_eq!(vars.id, "1");
        assert_eq!(vars.input.priority, Some(1));
        assert_eq!(vars.input.assignee_id, Field::Null);
    }

    #[test]
    fn ack_issue_update_with_no_server_issue_stamps_synced_at() {
        let conn = db_with_issue("1");
        enqueue_issue_update(
            &conn,
            update_vars(
                "1",
                IssueUpdateInput {
                    priority: Some(1),
                    ..Default::default()
                },
            ),
        )
        .unwrap();

        let seq = pending(&conn)[0].seq;
        ack_issue_update(&conn, seq, "1", None).unwrap();

        let synced_at: Option<String> = conn
            .query_row("SELECT synced_at FROM issues WHERE id = '1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(synced_at.is_some());
        assert!(pending(&conn).is_empty());
    }

    #[test]
    fn ack_issue_update_with_server_issue_upserts_it() {
        let conn = db_with_issue("1");
        enqueue_issue_update(
            &conn,
            update_vars(
                "1",
                IssueUpdateInput {
                    priority: Some(1),
                    ..Default::default()
                },
            ),
        )
        .unwrap();

        let seq = pending(&conn)[0].seq;
        // The server issue's state must already be locally known (sync owns
        // workflow states; the ack's upsert never mints one).
        crate::db::teams::upsert_team_state(
            &conn,
            "ENG",
            &types::WorkflowState {
                id: "s-merged".into(),
                name: "Merged".to_string(),
                position: 2.0,
            },
        )
        .unwrap();
        let mut server_issue = base_issue("1");
        server_issue.state = types::WorkflowState {
            id: "s-merged".into(),
            name: "Merged".to_string(),
            position: 2.0,
        };
        ack_issue_update(&conn, seq, "1", Some(&server_issue)).unwrap();

        let state_id: String = conn
            .query_row("SELECT state_id FROM issues WHERE id = '1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state_id, "s-merged");
        assert!(pending(&conn).is_empty());
    }

    #[test]
    fn enqueue_create_inserts_an_unsynced_fabricated_row_and_records_an_op() {
        let conn = db_with_todo_state();
        let input = IssueCreateInput {
            title: "New".to_string(),
            team_id: "ENG".to_string(),
            description: None,
            state_id: Some("s-todo".to_string()),
            priority: None,
            assignee_id: None,
        };
        let id = enqueue_issue_create(&conn, &IssueCreateVariables { input }).unwrap();
        assert!(
            !id.contains(':'),
            "a fabricated id carries no local: prefix"
        );

        let ops = pending(&conn);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].operation, IssueCreateMutation::NAME);
        assert_eq!(ops[0].id, id);

        let synced_at: Option<String> = conn
            .query_row(
                "SELECT synced_at FROM issues WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(synced_at, None);
    }

    #[test]
    fn ack_create_cascades_the_fabricated_id_to_server_id() {
        let conn = db_with_todo_state();
        let input = IssueCreateInput {
            title: "New".to_string(),
            team_id: "ENG".to_string(),
            description: None,
            state_id: None,
            priority: None,
            assignee_id: None,
        };
        let id = enqueue_issue_create(&conn, &IssueCreateVariables { input }).unwrap();

        // A child minted before the parent syncs must have its `parent_id`
        // carried to the server id by `ON UPDATE CASCADE`.
        let mut child = base_issue("child-1");
        child.parent = Some(types::Parent {
            id: id.clone().into(),
            identifier: OPTIMISTIC_ISSUE_IDENTIFIER.to_string(),
        });
        crate::db::upsert_issues(&conn, &[child]).unwrap();

        let seq = pending(&conn)[0].seq;
        let mut server_issue = base_issue("real-1");
        server_issue.identifier = "ENG-42".to_string();
        ack_issue_create(&conn, seq, &id, &server_issue).unwrap();

        let (identifier, synced_at): (String, Option<String>) = conn
            .query_row(
                "SELECT identifier, synced_at FROM issues WHERE id = 'real-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(identifier, "ENG-42");
        assert!(synced_at.is_some());

        let child_parent: String = conn
            .query_row(
                "SELECT parent_id FROM issues WHERE id = 'child-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            child_parent, "real-1",
            "ON UPDATE CASCADE must carry the fab->server id rewrite to the child"
        );
        assert!(pending(&conn).is_empty());
    }

    #[test]
    fn enqueue_comment_tags_author_from_the_persisted_viewer() {
        let conn = db_with_issue("1");
        crate::db::viewer::set_viewer(
            &conn,
            &lt_upstream::query::viewer::Viewer {
                user: types::User {
                    id: "u-ada".into(),
                    name: "Ada".to_string(),
                },
                organization: lt_upstream::query::viewer::Organization {
                    id: "org-1".into(),
                    name: "Acme".to_string(),
                    url_key: "acme".to_string(),
                },
            },
        )
        .unwrap();
        let input = lt_upstream::query::inputs::CommentCreateInput {
            issue_id: "1".to_string(),
            body: "hi".to_string(),
        };
        let id = enqueue_comment_create(&conn, &CommentCreateVariables { input }).unwrap();

        let ops = pending(&conn);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].operation, CommentCreateMutation::NAME);
        assert_eq!(ops[0].id, id);

        let rows = crate::db::query_comments(&conn, "1").unwrap();
        assert_eq!(rows[0].author(), "Ada");
    }

    #[test]
    fn comment_create_replay_vars_reads_the_rows_current_state() {
        let conn = db_with_issue("1");
        let input = lt_upstream::query::inputs::CommentCreateInput {
            issue_id: "1".to_string(),
            body: "hi".to_string(),
        };
        let id = enqueue_comment_create(&conn, &CommentCreateVariables { input }).unwrap();

        let vars = comment_create_replay_vars(&conn, &id).unwrap();
        assert_eq!(vars.input.issue_id, "1");
        assert_eq!(vars.input.body, "hi");
    }

    #[test]
    fn ack_comment_create_attaches_server_id_and_stamps_synced_at() {
        let conn = db_with_issue("1");
        let input = lt_upstream::query::inputs::CommentCreateInput {
            issue_id: "1".to_string(),
            body: "hi".to_string(),
        };
        let id = enqueue_comment_create(&conn, &CommentCreateVariables { input }).unwrap();
        let seq = pending(&conn)[0].seq;

        let comment = Comment {
            id: "c-real".into(),
            body: "hi".to_string(),
            created_at: "2026-01-03T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-03T00:00:00Z".parse().unwrap(),
            user: Some(types::User {
                id: "u-ada".into(),
                name: "Ada".to_string(),
            }),
            issue_id: Some("1".to_string()),
        };
        ack_comment_create(&conn, seq, &id, &comment).unwrap();

        let (found_id, synced_at): (String, Option<String>) = conn
            .query_row(
                "SELECT id, synced_at FROM issue_comments WHERE id = 'c-real'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(found_id, "c-real");
        assert!(synced_at.is_some());
        assert!(pending(&conn).is_empty());
    }

    #[test]
    fn op_is_sendable_issue_update_gates_on_the_rows_own_synced_at() {
        let conn = db_with_issue("1");
        enqueue_issue_update(
            &conn,
            update_vars(
                "1",
                IssueUpdateInput {
                    priority: Some(1),
                    ..Default::default()
                },
            ),
        )
        .unwrap();

        // `db_with_issue` upserts via the sync-fill path, so "1" is already
        // synced and its update is immediately sendable.
        let op = &pending(&conn)[0];
        assert!(op_is_sendable(&conn, op).unwrap());
    }

    #[test]
    fn op_is_sendable_issue_create_gates_on_an_unsynced_local_parent() {
        let db = crate::db::Memory::new().unwrap();
        let conn = db.connect().unwrap();
        crate::db::teams::upsert_team_state(
            &conn,
            "ENG",
            &types::WorkflowState {
                id: "s-todo".into(),
                name: "Todo".to_string(),
                position: 1.0,
            },
        )
        .unwrap();
        let parent_input = IssueCreateInput {
            title: "Parent".to_string(),
            team_id: "ENG".to_string(),
            description: None,
            state_id: Some("s-todo".to_string()),
            priority: None,
            assignee_id: None,
        };
        let parent_id = enqueue_issue_create(
            &conn,
            &IssueCreateVariables {
                input: parent_input,
            },
        )
        .unwrap();

        let child_input = IssueCreateInput {
            title: "Child".to_string(),
            team_id: "ENG".to_string(),
            description: None,
            state_id: Some("s-todo".to_string()),
            priority: None,
            assignee_id: None,
        };
        let child_id =
            enqueue_issue_create(&conn, &IssueCreateVariables { input: child_input }).unwrap();
        // `IssueCreateInput` carries no `parent_id`; a local parent linkage
        // (e.g. sync later discovering the relationship) is modeled directly
        // for this gate's purposes.
        conn.execute(
            "UPDATE issues SET parent_id = ?1 WHERE id = ?2",
            params![parent_id, child_id],
        )
        .unwrap();

        let ops = pending(&conn);
        let child_create_op = ops.iter().find(|o| o.id == child_id).unwrap();
        assert!(
            !op_is_sendable(&conn, child_create_op).unwrap(),
            "a create whose parent has not synced must not be sendable"
        );

        let parent_create_op = ops.iter().find(|o| o.id == parent_id).unwrap();
        let seq = parent_create_op.seq;
        let server_parent = base_issue("real-parent");
        ack_issue_create(&conn, seq, &parent_id, &server_parent).unwrap();

        let child_create_op = pending(&conn)
            .into_iter()
            .find(|o| o.id == child_id)
            .unwrap();
        assert!(
            op_is_sendable(&conn, &child_create_op).unwrap(),
            "the child create is sendable once its parent create has synced"
        );
    }
}
