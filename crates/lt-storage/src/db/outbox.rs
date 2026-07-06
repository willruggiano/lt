//! The local write path: the pending-overlay merge source and the mutation
//! outbox the sync drainer replays.
//!
//! A TUI edit never touches the network. It writes its intent into
//! `pending_overlay` (so the read model renders it immediately) and a command
//! into `outbox`, both in one transaction. The sync drainer is the
//! single writer that replays the outbox against the API and reconciles the
//! base on success. [`Mutation`] binds each mutation operation to that local
//! effect and its ack, alongside a query operation's fetched-response write
//! (docs/design/unified-execute-adr.md, "Decision 2").

use anyhow::{Context, Result};
use chrono::Utc;
use lt_types::comments::{CommentCreateMutation, CommentCreateVariables};
use lt_types::graphql::GraphqlOperation;
use lt_types::inputs::{Field, IssueCreateInput, IssueUpdateInput};
use lt_types::issues::{
    IssueCreateMutation, IssueCreateVariables, IssueUpdateMutation, IssueUpdateVariables,
};
use lt_types::scalars::Priority;
use lt_types::types;
use rusqlite::{Connection, params};
use serde_json::json;

use crate::db::ops::{AckContext, Enqueued, EntityKey, Mutation};
use crate::db::sql;

/// The optimistic identifier every locally-created issue carries until the
/// drainer's ack replaces it with the server's real one.
pub const OPTIMISTIC_ISSUE_IDENTIFIER: &str = "NEW";

/// Which field of an issue an update overlay targets. The `&str` form is the
/// `pending_overlay.field` value, shared by the writer and the read-model merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayField {
    State,
    Priority,
    Assignee,
}

impl OverlayField {
    pub fn as_str(self) -> &'static str {
        match self {
            OverlayField::State => "state",
            OverlayField::Priority => "priority",
            OverlayField::Assignee => "assignee",
        }
    }
}

/// One pending command read from the outbox, in `seq` order.
pub struct PendingOp {
    pub seq: i64,
    pub op_type: String,
    pub entity_id: String,
    /// The mutation variables payload as stored JSON.
    pub variables: String,
}

// ---------------------------------------------------------------------------
// Overlay + outbox primitives
// ---------------------------------------------------------------------------

/// Upsert one `(entity_id, field)` overlay row. `value` is the referenced id,
/// the priority label (`Priority::label`), or `None` to mean "clear" (e.g.
/// unassign).
fn set_overlay(
    tx: &Connection,
    entity_id: &str,
    field: OverlayField,
    value: Option<&str>,
) -> Result<()> {
    sql::execute(
        tx,
        sql::SET_OVERLAY,
        params![entity_id, field.as_str(), value],
        "write pending overlay",
    )
}

/// Replace the pending command of `op_type` for `entity_id` with one carrying
/// `variables`. Coalescing: repeated edits to one entity collapse into a single
/// pending row rather than a queue of partial commands. A create's `entity_id`
/// is a fresh id it mints itself, so this is a no-op delete followed by an
/// insert -- every create stays independent.
fn replace_pending(
    tx: &Connection,
    op_type: &str,
    entity_id: &str,
    variables: &serde_json::Value,
) -> Result<()> {
    sql::execute(
        tx,
        sql::DELETE_SUPERSEDED_PENDING,
        params![op_type, entity_id],
        "clear superseded outbox command",
    )?;
    insert_pending(tx, op_type, entity_id, variables)
}

/// Insert a new pending command.
fn insert_pending(
    tx: &Connection,
    op_type: &str,
    entity_id: &str,
    variables: &serde_json::Value,
) -> Result<()> {
    sql::execute(
        tx,
        sql::INSERT_PENDING,
        params![
            op_type,
            entity_id,
            variables.to_string(),
            Utc::now().to_rfc3339()
        ],
        "enqueue outbox command",
    )
}

/// Read every `(field, value)` overlay row for an issue.
fn overlay_rows(conn: &Connection, issue_id: &str) -> Result<Vec<(String, Option<String>)>> {
    let mut stmt =
        sql::prepare(conn, sql::OVERLAY_ROWS).context("failed to prepare overlay query")?;
    let rows = stmt
        .query_map(params![issue_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .context("failed to query overlay rows")?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.context("failed to read overlay row")?);
    }
    Ok(out)
}

/// Fold an issue's overlay rows into one coalesced `IssueUpdateInput`. The
/// priority overlay stores the label (`Priority::label`, the one source of
/// truth), so rebuilding the wire's numeric level inverts it via
/// `Priority::from_label`.
fn issue_update_input(conn: &Connection, issue_id: &str) -> Result<IssueUpdateInput> {
    let mut input = IssueUpdateInput::default();
    for (field, value) in overlay_rows(conn, issue_id)? {
        match field.as_str() {
            "state" => input.state_id = value,
            "priority" => {
                input.priority = value
                    .as_deref()
                    .map(|label| i32::from(Priority::from_label(label).0));
            }
            "assignee" => {
                input.assignee_id = match value {
                    Some(id) => Field::Value(id),
                    None => Field::Null,
                }
            }
            _ => {}
        }
    }
    Ok(input)
}

/// Rebuild and re-enqueue the coalesced `issueUpdate` command from the issue's
/// current overlay rows.
fn refresh_issue_update_command(tx: &Connection, issue_id: &str) -> Result<()> {
    let input = issue_update_input(tx, issue_id)?;
    let variables = json!({ "id": issue_id, "input": input });
    replace_pending(tx, IssueUpdateMutation::NAME, issue_id, &variables)
}

// ---------------------------------------------------------------------------
// IssueUpdateMutation
// ---------------------------------------------------------------------------

impl Mutation for IssueUpdateMutation {
    /// Overlay whichever of `vars.input`'s fields are set -- the id/name join
    /// each field resolves through is already cached, since every id offered
    /// by a picker came from that same picker's own `Mutation::apply` -- then rebuild
    /// the coalesced command from every overlay row the issue carries (not
    /// just this one), so repeated edits collapse into a single pending
    /// `issueUpdate`.
    fn enqueue(conn: &Connection, vars: IssueUpdateVariables) -> Result<Enqueued> {
        let tx = conn.unchecked_transaction()?;
        if let Some(state_id) = &vars.input.state_id {
            set_overlay(&tx, &vars.id, OverlayField::State, Some(state_id))?;
        }
        if let Some(priority) = vars.input.priority {
            let label = Priority(u8::try_from(priority).unwrap_or(0)).label();
            set_overlay(&tx, &vars.id, OverlayField::Priority, Some(label))?;
        }
        match &vars.input.assignee_id {
            Field::Value(id) => set_overlay(&tx, &vars.id, OverlayField::Assignee, Some(id))?,
            Field::Null => set_overlay(&tx, &vars.id, OverlayField::Assignee, None)?,
            Field::Absent => {}
        }
        refresh_issue_update_command(&tx, &vars.id)?;
        tx.commit().context("failed to commit issue update")?;
        Ok(Enqueued {
            entity_id: vars.id,
            touched: vec![EntityKey::Issue],
        })
    }

    /// Apply a confirmed issue update to the base and retire its overlay +
    /// command, atomically. When the server returned the updated issue
    /// (nullable in the schema even on success), its fields become the new
    /// base truth via a full upsert; otherwise falls back to applying the
    /// overlay's per-field intent, as before. Either way the read model never
    /// flickers back to the pre-edit value.
    fn ack(
        conn: &Connection,
        ctx: AckContext<'_, IssueUpdateVariables>,
        out: Option<types::Issue>,
    ) -> Result<Vec<EntityKey>> {
        let AckContext { seq, entity_id, .. } = ctx;
        let tx = conn.unchecked_transaction()?;
        if let Some(issue) = &out {
            let synced_at = Utc::now().to_rfc3339();
            crate::db::issues::upsert_issue_tx(&tx, issue, &synced_at)?;
        } else {
            for (field, value) in overlay_rows(&tx, entity_id)? {
                match field.as_str() {
                    "state" => {
                        sql::execute(
                            &tx,
                            sql::ACK_UPDATE_STATE,
                            params![value, entity_id],
                            "apply acked state",
                        )?;
                    }
                    "assignee" => {
                        sql::execute(
                            &tx,
                            sql::ACK_UPDATE_ASSIGNEE,
                            params![value, entity_id],
                            "apply acked assignee",
                        )?;
                    }
                    "priority" => {
                        let label = value.as_deref().unwrap_or("No priority");
                        sql::execute(
                            &tx,
                            sql::ACK_UPDATE_PRIORITY,
                            params![label, entity_id],
                            "apply acked priority",
                        )?;
                    }
                    _ => {}
                }
            }
        }
        sql::execute(
            &tx,
            sql::DELETE_PENDING_OVERLAY_FOR_ENTITY,
            params![entity_id],
            "clear acked overlay",
        )?;
        delete_command(&tx, seq)?;
        tx.commit().context("failed to commit issue-update ack")?;
        Ok(vec![EntityKey::Issue])
    }
}

// ---------------------------------------------------------------------------
// IssueCreateMutation
// ---------------------------------------------------------------------------

/// Build the optimistic issue fragment for a locally-created issue. Display
/// names are resolved from the same lookup tables the pickers read (team,
/// state, member). Sync owns workflow states (issue upserts never write
/// them), so an absent `state_id` cannot fabricate one: it defaults to the
/// team's first cached state (`query_team_states` order), erroring if the
/// team has no cached states (a never-synced cache). A `state_id` lookup miss
/// falls back to the id as its own display name -- the offered id came from a
/// stale/offline picker, not the cache, so there is no real position to carry.
fn optimistic_issue(conn: &Connection, input: &IssueCreateInput) -> Result<types::Issue> {
    let team_name = crate::db::teams::query_teams(conn)?
        .into_iter()
        .find(|t| t.id.inner() == input.team_id)
        .map_or_else(String::new, |t| t.name);

    let (state_id, state_name, state_position) = if let Some(id) = &input.state_id {
        let cached = crate::db::teams::query_team_states(conn, &input.team_id)?
            .into_iter()
            .find(|s| s.id.inner() == id);
        match cached {
            Some(s) => (id.clone(), s.name, s.position),
            None => (id.clone(), id.clone(), 0.0),
        }
    } else {
        let first = crate::db::teams::query_team_states(conn, &input.team_id)?
            .into_iter()
            .next()
            .with_context(|| {
                format!(
                    "no workflow states cached for team {} -- run `lt sync`",
                    input.team_id
                )
            })?;
        (first.id.inner().to_string(), first.name, first.position)
    };

    let assignee = match &input.assignee_id {
        Some(id) => Some(
            crate::db::teams::query_team_members(conn, &input.team_id)?
                .into_iter()
                .find(|u| u.id.inner() == id)
                .unwrap_or_else(|| types::User {
                    id: id.clone().into(),
                    name: id.clone(),
                }),
        ),
        None => None,
    };

    let priority = input
        .priority
        .and_then(|p| u8::try_from(p).ok())
        .unwrap_or(0);
    let now = lt_types::scalars::DateTime(Utc::now());
    Ok(types::Issue {
        id: temp_id().into(),
        identifier: OPTIMISTIC_ISSUE_IDENTIFIER.to_string(),
        title: input.title.clone(),
        priority: lt_types::scalars::Priority(priority),
        priority_label: lt_types::scalars::Priority(priority).label().to_string(),
        state: types::WorkflowState {
            id: state_id.into(),
            name: state_name,
            position: state_position,
        },
        assignee,
        team: types::Team {
            id: input.team_id.clone().into(),
            name: team_name,
        },
        description: input.description.clone(),
        labels: types::IssueLabelConnection { nodes: Vec::new() },
        project: None,
        cycle: None,
        creator: None,
        parent: None,
        created_at: now,
        updated_at: now,
    })
}

impl Mutation for IssueCreateMutation {
    /// Insert an optimistic base row under a client temp id and queue the
    /// `issueCreate` command. Creates never coalesce: each mints its own temp
    /// id, so the shared coalescing primitive is a no-op delete plus an
    /// insert.
    fn enqueue(conn: &Connection, vars: IssueCreateVariables) -> Result<Enqueued> {
        let tx = conn.unchecked_transaction()?;
        let optimistic = optimistic_issue(&tx, &vars.input)?;
        let synced_at = Utc::now().to_rfc3339();
        crate::db::issues::upsert_issue_tx(&tx, &optimistic, &synced_at)?;
        let variables =
            serde_json::to_value(&vars).context("failed to serialize issue-create variables")?;
        insert_pending(
            &tx,
            IssueCreateMutation::NAME,
            optimistic.id.inner(),
            &variables,
        )?;
        tx.commit().context("failed to commit issue create")?;
        Ok(Enqueued {
            entity_id: optimistic.id.into_inner(),
            touched: vec![EntityKey::Issue],
        })
    }

    /// Replace the optimistic temp issue with the server's full issue (server
    /// truth, not a hand-stitched id/identifier rewrite) and retire the
    /// command.
    fn ack(
        conn: &Connection,
        ctx: AckContext<'_, IssueCreateVariables>,
        issue: types::Issue,
    ) -> Result<Vec<EntityKey>> {
        let AckContext { seq, entity_id, .. } = ctx;
        let tx = conn.unchecked_transaction()?;
        let synced_at = Utc::now().to_rfc3339();
        crate::db::issues::upsert_issue_tx(&tx, &issue, &synced_at)?;
        sql::execute(
            &tx,
            sql::DELETE_ISSUE_LABELS_FOR_ISSUE,
            params![entity_id],
            "clear temp issue labels",
        )?;
        sql::execute(
            &tx,
            sql::DELETE_ISSUE_BY_ID,
            params![entity_id],
            "delete temp issue",
        )?;
        delete_command(&tx, seq)?;
        tx.commit().context("failed to commit issue-create ack")?;
        Ok(crate::db::issues::issue_upsert_touched(
            std::slice::from_ref(&issue),
        ))
    }
}

// ---------------------------------------------------------------------------
// CommentCreateMutation
// ---------------------------------------------------------------------------

impl Mutation for CommentCreateMutation {
    /// Insert an optimistic comment row under a freshly-minted `temp_id` and
    /// queue the `commentCreate` command. The issue and body come from
    /// `vars.input`; the author is the persisted viewer identity
    /// (`sync_meta`), if one has been recorded yet.
    fn enqueue(conn: &Connection, vars: CommentCreateVariables) -> Result<Enqueued> {
        let tx = conn.unchecked_transaction()?;
        let id = temp_id();
        let now = lt_types::scalars::DateTime(Utc::now());
        let comment = lt_types::comments::Comment {
            id: id.clone().into(),
            body: vars.input.body.clone(),
            created_at: now,
            updated_at: now,
            user: crate::db::viewer::viewer(&tx)?.map(|v| v.user),
            issue_id: Some(vars.input.issue_id.clone()),
        };
        crate::db::comments::upsert_comments(&tx, std::slice::from_ref(&comment))?;
        let issue_id = vars.input.issue_id.clone();
        let variables =
            serde_json::to_value(&vars).context("failed to serialize comment-create variables")?;
        insert_pending(&tx, CommentCreateMutation::NAME, &id, &variables)?;
        tx.commit().context("failed to commit comment create")?;
        Ok(Enqueued {
            entity_id: id,
            touched: vec![EntityKey::Comment { issue_id }],
        })
    }

    /// Replace the optimistic temp comment with the server copy and retire
    /// the command, atomically -- so the comment never blinks out between ack
    /// and the next per-issue comment sync. Stamps the issue id from `vars`
    /// rather than trusting the server's comment payload for it: the
    /// mutation's `issueId` is nullable in the schema, but the outbox command
    /// it replays always carries the issue it was queued against.
    fn ack(
        conn: &Connection,
        ctx: AckContext<'_, CommentCreateVariables>,
        comment: lt_types::comments::Comment,
    ) -> Result<Vec<EntityKey>> {
        let AckContext {
            seq,
            entity_id,
            vars,
        } = ctx;
        let tx = conn.unchecked_transaction()?;
        sql::execute(
            &tx,
            sql::DELETE_ISSUE_COMMENT_BY_ID,
            params![entity_id],
            "delete temp comment",
        )?;
        let issue_id = vars.input.issue_id.clone();
        let comment = lt_types::comments::Comment {
            issue_id: Some(issue_id.clone()),
            ..comment
        };
        crate::db::comments::upsert_comments(&tx, std::slice::from_ref(&comment))?;
        delete_command(&tx, seq)?;
        tx.commit().context("failed to commit comment-create ack")?;
        Ok(vec![EntityKey::Comment { issue_id }])
    }
}

// ---------------------------------------------------------------------------
// Drain support (called by the sync drainer)
// ---------------------------------------------------------------------------

/// Every pending command in `seq` order.
pub fn pending_operations(conn: &Connection) -> Result<Vec<PendingOp>> {
    let mut stmt =
        sql::prepare(conn, sql::PENDING_OPERATIONS).context("failed to prepare outbox query")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(PendingOp {
                seq: r.get(0)?,
                op_type: r.get(1)?,
                entity_id: r.get(2)?,
                variables: r.get(3)?,
            })
        })
        .context("failed to query outbox")?;
    let mut ops = Vec::new();
    for row in rows {
        ops.push(row.context("failed to read outbox row")?);
    }
    Ok(ops)
}

/// Record a failed drain attempt; the command stays pending for the next sync.
pub fn record_error(conn: &Connection, seq: i64, error: &str) -> Result<()> {
    sql::execute(
        conn,
        sql::RECORD_ERROR,
        params![error, seq],
        "record outbox error",
    )
}

fn delete_command(tx: &Connection, seq: i64) -> Result<()> {
    sql::execute(
        tx,
        sql::DELETE_COMMAND,
        params![seq],
        "delete outbox command",
    )
}

/// A client-side temporary id for an optimistic create, distinguishable from a
/// server id by its `local:` prefix. Comment sync preserves `local:` rows so an
/// un-acked comment is not wiped before the drainer posts it.
pub fn temp_id() -> String {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use rand::Rng as _;
    let mut bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut bytes);
    format!("local:{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// A minimal base issue for the write-path tests, shared with the sync drainer
/// tests so the fixture is defined once.
#[cfg(any(test, feature = "test-util"))]
pub fn sample_base_issue(id: &str) -> types::Issue {
    types::Issue {
        id: id.into(),
        identifier: format!("ENG-{id}"),
        title: format!("issue {id}"),
        priority_label: "Normal".to_string(),
        priority: lt_types::scalars::Priority(3),
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

    fn db_with_issue(id: &str) -> Connection {
        let db = crate::db::Database::memory().unwrap();
        let conn = db.connect().unwrap();
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
    fn per_field_edits_coalesce_into_one_command_preserving_null() {
        let conn = db_with_issue("1");
        IssueUpdateMutation::enqueue(
            &conn,
            update_vars(
                "1",
                IssueUpdateInput {
                    state_id: Some("s-done".to_string()),
                    ..Default::default()
                },
            ),
        )
        .unwrap();
        IssueUpdateMutation::enqueue(
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
        IssueUpdateMutation::enqueue(
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
        assert_eq!(ops.len(), 1, "three edits collapse into one issueUpdate");
        assert_eq!(ops[0].op_type, IssueUpdateMutation::NAME);

        let vars: serde_json::Value = serde_json::from_str(&ops[0].variables).unwrap();
        assert_eq!(vars["id"], "1");
        assert_eq!(
            vars["input"],
            serde_json::json!({ "stateId": "s-done", "priority": 1, "assigneeId": null })
        );
    }

    #[test]
    fn ack_issue_update_applies_overlay_to_base_when_no_server_issue() {
        let conn = db_with_issue("1");
        IssueUpdateMutation::enqueue(
            &conn,
            update_vars(
                "1",
                IssueUpdateInput {
                    state_id: Some("s-done".to_string()),
                    ..Default::default()
                },
            ),
        )
        .unwrap();
        IssueUpdateMutation::enqueue(
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

        let op = &pending(&conn)[0];
        let seq = op.seq;
        let vars: IssueUpdateVariables = serde_json::from_str(&op.variables).unwrap();
        IssueUpdateMutation::ack(
            &conn,
            AckContext {
                seq,
                entity_id: "1",
                vars: &vars,
            },
            None,
        )
        .unwrap();

        // Base now carries the acked values; overlay and command are gone.
        let (state_id, assignee): (String, Option<String>) = conn
            .query_row(
                "SELECT state_id, assignee_id FROM issues WHERE id = '1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state_id, "s-done");
        assert_eq!(assignee, None);

        let overlays: i64 = conn
            .query_row("SELECT COUNT(*) FROM pending_overlay", [], |r| r.get(0))
            .unwrap();
        assert_eq!(overlays, 0);
        assert!(pending(&conn).is_empty());
    }

    #[test]
    fn ack_issue_update_prefers_server_issue_over_overlay() {
        let conn = db_with_issue("1");
        IssueUpdateMutation::enqueue(
            &conn,
            update_vars(
                "1",
                IssueUpdateInput {
                    state_id: Some("s-done".to_string()),
                    ..Default::default()
                },
            ),
        )
        .unwrap();

        let op = &pending(&conn)[0];
        let seq = op.seq;
        let vars: IssueUpdateVariables = serde_json::from_str(&op.variables).unwrap();
        let mut server_issue = base_issue("1");
        server_issue.state = types::WorkflowState {
            id: "s-merged".into(),
            name: "Merged".to_string(),
            position: 2.0,
        };
        IssueUpdateMutation::ack(
            &conn,
            AckContext {
                seq,
                entity_id: "1",
                vars: &vars,
            },
            Some(server_issue),
        )
        .unwrap();

        let state_id: String = conn
            .query_row("SELECT state_id FROM issues WHERE id = '1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state_id, "s-merged");
        assert!(pending(&conn).is_empty());
    }

    #[test]
    fn enqueue_create_inserts_temp_row_and_command() {
        let db = crate::db::Database::memory().unwrap();
        let conn = db.connect().unwrap();
        let input = IssueCreateInput {
            title: "New".to_string(),
            team_id: "ENG".to_string(),
            description: None,
            state_id: Some("s-todo".to_string()),
            priority: None,
            assignee_id: None,
        };
        let enqueued = IssueCreateMutation::enqueue(&conn, IssueCreateVariables { input }).unwrap();
        assert_eq!(enqueued.touched, vec![EntityKey::Issue]);
        assert!(enqueued.entity_id.starts_with("local:"));

        let ops = pending(&conn);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].op_type, IssueCreateMutation::NAME);
        assert!(ops[0].entity_id.starts_with("local:"));

        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM issues WHERE id = ?1",
                params![ops[0].entity_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1);
    }

    #[test]
    fn ack_create_rewrites_temp_id_to_server_id() {
        let db = crate::db::Database::memory().unwrap();
        let conn = db.connect().unwrap();
        // The optimistic create defaults to the team's first cached state
        // (sync owns workflow states; issue upserts never write them).
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
        let input = IssueCreateInput {
            title: "New".to_string(),
            team_id: "ENG".to_string(),
            description: None,
            state_id: None,
            priority: None,
            assignee_id: None,
        };
        IssueCreateMutation::enqueue(
            &conn,
            IssueCreateVariables {
                input: input.clone(),
            },
        )
        .unwrap();
        let op = &pending(&conn)[0];
        let seq = op.seq;
        let temp_id = op.entity_id.clone();

        let mut server_issue = base_issue("real-1");
        server_issue.identifier = "ENG-42".to_string();
        let touched = IssueCreateMutation::ack(
            &conn,
            AckContext {
                seq,
                entity_id: &temp_id,
                vars: &IssueCreateVariables { input },
            },
            server_issue,
        )
        .unwrap();
        assert!(touched.contains(&EntityKey::Issue));

        let ident: String = conn
            .query_row(
                "SELECT identifier FROM issues WHERE id = 'real-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ident, "ENG-42");
        let temp: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM issues WHERE id = ?1",
                params![temp_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(temp, 0);
        assert!(pending(&conn).is_empty());
    }

    #[test]
    fn enqueue_comment_tags_author_from_the_persisted_viewer() {
        let conn = db_with_issue("1");
        crate::db::viewer::set_viewer(
            &conn,
            &lt_types::viewer::Viewer {
                user: types::User {
                    id: "u-ada".into(),
                    name: "Ada".to_string(),
                },
                organization: lt_types::viewer::Organization {
                    id: "org-1".into(),
                    name: "Acme".to_string(),
                    url_key: "acme".to_string(),
                },
            },
        )
        .unwrap();
        let input = lt_types::inputs::CommentCreateInput {
            issue_id: "1".to_string(),
            body: "hi".to_string(),
        };
        CommentCreateMutation::enqueue(&conn, CommentCreateVariables { input }).unwrap();

        let rows = crate::db::query_comments(&conn, "1").unwrap();
        assert_eq!(rows[0].author(), "Ada");
    }

    #[test]
    fn ack_comment_replaces_temp_with_server_copy() {
        let conn = db_with_issue("1");
        let input = lt_types::inputs::CommentCreateInput {
            issue_id: "1".to_string(),
            body: "hi".to_string(),
        };
        CommentCreateMutation::enqueue(
            &conn,
            CommentCreateVariables {
                input: input.clone(),
            },
        )
        .unwrap();
        let op = &pending(&conn)[0];
        let seq = op.seq;
        let temp_id = op.entity_id.clone();

        let comment = lt_types::comments::Comment {
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
        let touched = CommentCreateMutation::ack(
            &conn,
            AckContext {
                seq,
                entity_id: &temp_id,
                vars: &CommentCreateVariables { input },
            },
            comment,
        )
        .unwrap();
        assert_eq!(
            touched,
            vec![EntityKey::Comment {
                issue_id: "1".to_string()
            }]
        );

        let ids: Vec<String> = {
            let rows = crate::db::query_comments(&conn, "1").unwrap();
            rows.into_iter().map(|c| c.id.into_inner()).collect()
        };
        assert_eq!(ids, ["c-real"]);
        assert!(pending(&conn).is_empty());
    }
}
