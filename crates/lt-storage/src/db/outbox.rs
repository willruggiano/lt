//! The local write path: the pending-overlay merge source and the mutation
//! outbox the sync drainer replays.
//!
//! A TUI edit never touches the network. It writes its intent into
//! `pending_overlay` (so the read model renders it immediately) and a command
//! into `outbox`, both in one transaction. The sync drainer is the
//! single writer that replays the outbox against the API and reconciles the
//! base on success.

use anyhow::{Context, Result};
use chrono::Utc;
use lt_types::inputs::{CommentCreateInput, Field, IssueCreateInput, IssueUpdateInput};
use lt_types::scalars::Priority;
use lt_types::types;
use rusqlite::{Connection, params};
use serde_json::json;

use crate::db::sql::{self, EntityTable};

pub const OP_ISSUE_UPDATE: &str = "IssueUpdate";
pub const OP_ISSUE_CREATE: &str = "IssueCreate";
pub const OP_COMMENT_CREATE: &str = "CommentCreate";

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
/// the priority number, or `None` to mean "clear" (e.g. unassign).
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
/// pending row rather than a queue of partial commands.
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

/// Insert a new pending command. Used for creates, which never coalesce.
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

/// Fold an issue's overlay rows into one coalesced `IssueUpdateInput`.
fn issue_update_input(conn: &Connection, issue_id: &str) -> Result<IssueUpdateInput> {
    let mut input = IssueUpdateInput::default();
    for (field, value) in overlay_rows(conn, issue_id)? {
        match field.as_str() {
            "state" => input.state_id = value,
            "priority" => input.priority = value.as_deref().and_then(|v| v.parse().ok()),
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
    replace_pending(tx, OP_ISSUE_UPDATE, issue_id, &variables)
}

// ---------------------------------------------------------------------------
// Enqueue (called by the TUI write path)
// ---------------------------------------------------------------------------

/// Enqueue a state change: record the chosen workflow state (so the read-model
/// join resolves its name), overlay the FK, and refresh the coalesced command.
pub fn enqueue_state_change(
    conn: &Connection,
    issue_id: &str,
    state_id: &str,
    state_name: &str,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    crate::db::issues::upsert_named_entity(
        &tx,
        EntityTable::WorkflowStates,
        state_id,
        Some(state_name),
    )?;
    set_overlay(&tx, issue_id, OverlayField::State, Some(state_id))?;
    refresh_issue_update_command(&tx, issue_id)?;
    tx.commit().context("failed to commit state change")?;
    Ok(())
}

/// Enqueue a priority change.
pub fn enqueue_priority_change(conn: &Connection, issue_id: &str, priority: u8) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    set_overlay(
        &tx,
        issue_id,
        OverlayField::Priority,
        Some(&priority.to_string()),
    )?;
    refresh_issue_update_command(&tx, issue_id)?;
    tx.commit().context("failed to commit priority change")?;
    Ok(())
}

/// Enqueue an assignee change. `assignee = None` clears (unassign).
pub fn enqueue_assignee_change(
    conn: &Connection,
    issue_id: &str,
    assignee: Option<(&str, &str)>,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let value = match assignee {
        Some((id, name)) => {
            crate::db::issues::upsert_named_entity(&tx, EntityTable::Users, id, Some(name))?;
            Some(id)
        }
        None => None,
    };
    set_overlay(&tx, issue_id, OverlayField::Assignee, value)?;
    refresh_issue_update_command(&tx, issue_id)?;
    tx.commit().context("failed to commit assignee change")?;
    Ok(())
}

/// Enqueue an issue create: insert an optimistic base row under a client temp
/// id and queue the `issueCreate` command.
pub fn enqueue_issue_create(
    conn: &Connection,
    optimistic: &types::Issue,
    input: &IssueCreateInput,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let synced_at = Utc::now().to_rfc3339();
    crate::db::issues::upsert_issue_tx(&tx, optimistic, &synced_at)?;
    insert_pending(
        &tx,
        OP_ISSUE_CREATE,
        optimistic.id.inner(),
        &json!({ "input": input }),
    )?;
    tx.commit().context("failed to commit issue create")?;
    Ok(())
}

/// Enqueue a comment create: insert an optimistic comment row under the client
/// `temp_id` and queue the `commentCreate` command. The issue and body come
/// from `input`; the author is the persisted viewer identity (`sync_meta`), if
/// one has been recorded yet.
pub fn enqueue_comment_create(
    conn: &Connection,
    temp_id: &str,
    input: &CommentCreateInput,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let now = lt_types::scalars::DateTime(Utc::now());
    let comment = lt_types::comments::Comment {
        id: temp_id.into(),
        body: input.body.clone(),
        created_at: now,
        updated_at: now,
        user: crate::db::viewer::viewer(&tx)?.map(types::User::from),
        issue_id: Some(input.issue_id.clone()),
    };
    crate::db::comments::upsert_comments(&tx, std::slice::from_ref(&comment))?;
    // entity_id is the temp comment id so the ack can find and replace the row.
    insert_pending(&tx, OP_COMMENT_CREATE, temp_id, &json!({ "input": input }))?;
    tx.commit().context("failed to commit comment create")?;
    Ok(())
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

/// Apply a confirmed issue update to the base and retire its overlay + command,
/// atomically. When the server returned the updated issue (nullable in the
/// schema even on success), its fields become the new base truth via a full
/// upsert; otherwise falls back to applying the overlay's per-field intent, as
/// before. Either way the read model never flickers back to the pre-edit value.
pub fn ack_issue_update(
    conn: &Connection,
    seq: i64,
    issue_id: &str,
    server_issue: Option<&types::Issue>,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    if let Some(issue) = server_issue {
        let synced_at = Utc::now().to_rfc3339();
        crate::db::issues::upsert_issue_tx(&tx, issue, &synced_at)?;
    } else {
        for (field, value) in overlay_rows(&tx, issue_id)? {
            match field.as_str() {
                "state" => {
                    sql::execute(
                        &tx,
                        sql::ACK_UPDATE_STATE,
                        params![value, issue_id],
                        "apply acked state",
                    )?;
                }
                "assignee" => {
                    sql::execute(
                        &tx,
                        sql::ACK_UPDATE_ASSIGNEE,
                        params![value, issue_id],
                        "apply acked assignee",
                    )?;
                }
                "priority" => {
                    let label = value
                        .as_deref()
                        .and_then(|v| v.parse::<u8>().ok())
                        .map_or("No priority", |p| Priority(p).label());
                    sql::execute(
                        &tx,
                        sql::ACK_UPDATE_PRIORITY,
                        params![label, issue_id],
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
        params![issue_id],
        "clear acked overlay",
    )?;
    delete_command(&tx, seq)?;
    tx.commit().context("failed to commit issue-update ack")?;
    Ok(())
}

/// Replace the optimistic temp issue with the server's full issue (server
/// truth, not a hand-stitched id/identifier rewrite) and retire the command.
pub fn ack_issue_create(
    conn: &Connection,
    seq: i64,
    temp_id: &str,
    issue: &types::Issue,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let synced_at = Utc::now().to_rfc3339();
    crate::db::issues::upsert_issue_tx(&tx, issue, &synced_at)?;
    sql::execute(
        &tx,
        sql::DELETE_ISSUE_LABELS_FOR_ISSUE,
        params![temp_id],
        "clear temp issue labels",
    )?;
    sql::execute(
        &tx,
        sql::DELETE_ISSUE_BY_ID,
        params![temp_id],
        "delete temp issue",
    )?;
    delete_command(&tx, seq)?;
    tx.commit().context("failed to commit issue-create ack")?;
    Ok(())
}

/// The server's acknowledgement of a queued `commentCreate`: the client temp
/// id to replace, the issue it belongs to, and the server's comment (used
/// as-is, from the `commentCreate` response).
pub struct CommentAck<'a> {
    pub temp_id: &'a str,
    pub issue_id: &'a str,
    pub comment: &'a lt_types::comments::Comment,
}

/// Replace the optimistic temp comment with the server copy and retire the
/// command, atomically -- so the comment never blinks out between ack and the
/// next per-issue comment sync.
pub fn ack_comment_create(conn: &Connection, seq: i64, ack: &CommentAck) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    sql::execute(
        &tx,
        sql::DELETE_ISSUE_COMMENT_BY_ID,
        params![ack.temp_id],
        "delete temp comment",
    )?;
    // Stamp the issue id from the ack rather than trusting the server's
    // comment payload for it: the mutation's `issueId` is nullable in the
    // schema, but the outbox command it replays always carries the issue it
    // was queued against.
    let comment = lt_types::comments::Comment {
        issue_id: Some(ack.issue_id.to_string()),
        ..ack.comment.clone()
    };
    crate::db::comments::upsert_comments(&tx, std::slice::from_ref(&comment))?;
    delete_command(&tx, seq)?;
    tx.commit().context("failed to commit comment-create ack")?;
    Ok(())
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
            position: None,
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

    #[test]
    fn per_field_edits_coalesce_into_one_command_preserving_null() {
        let conn = db_with_issue("1");
        enqueue_state_change(&conn, "1", "s-done", "Done").unwrap();
        enqueue_priority_change(&conn, "1", 1).unwrap();
        // Clearing the assignee must survive coalescing as an explicit null.
        enqueue_assignee_change(&conn, "1", None).unwrap();

        let ops = pending(&conn);
        assert_eq!(ops.len(), 1, "three edits collapse into one issueUpdate");
        assert_eq!(ops[0].op_type, OP_ISSUE_UPDATE);

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
        enqueue_state_change(&conn, "1", "s-done", "Done").unwrap();
        enqueue_assignee_change(&conn, "1", None).unwrap();

        let seq = pending(&conn)[0].seq;
        ack_issue_update(&conn, seq, "1", None).unwrap();

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
        enqueue_state_change(&conn, "1", "s-done", "Done").unwrap();

        let seq = pending(&conn)[0].seq;
        let mut server_issue = base_issue("1");
        server_issue.state = types::WorkflowState {
            id: "s-merged".into(),
            name: "Merged".to_string(),
            position: None,
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
    fn enqueue_create_inserts_temp_row_and_command() {
        let db = crate::db::Database::memory().unwrap();
        let conn = db.connect().unwrap();
        let mut issue = base_issue("temp");
        issue.id = "local:abc".into();
        issue.identifier = "NEW".to_string();
        let input = IssueCreateInput {
            title: "New".to_string(),
            team_id: "ENG".to_string(),
            description: None,
            state_id: Some("s-todo".to_string()),
            priority: None,
            assignee_id: None,
        };
        enqueue_issue_create(&conn, &issue, &input).unwrap();

        let ops = pending(&conn);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].op_type, OP_ISSUE_CREATE);
        assert_eq!(ops[0].entity_id, "local:abc");

        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM issues WHERE id = 'local:abc'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1);
    }

    #[test]
    fn ack_create_rewrites_temp_id_to_server_id() {
        let db = crate::db::Database::memory().unwrap();
        let conn = db.connect().unwrap();
        let mut issue = base_issue("temp");
        issue.id = "local:abc".into();
        let input = IssueCreateInput {
            title: "New".to_string(),
            team_id: "ENG".to_string(),
            description: None,
            state_id: None,
            priority: None,
            assignee_id: None,
        };
        enqueue_issue_create(&conn, &issue, &input).unwrap();
        let seq = pending(&conn)[0].seq;

        let mut server_issue = base_issue("real-1");
        server_issue.identifier = "ENG-42".to_string();
        ack_issue_create(&conn, seq, "local:abc", &server_issue).unwrap();

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
                "SELECT COUNT(*) FROM issues WHERE id = 'local:abc'",
                [],
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
            &lt_types::viewer::User {
                id: "u-ada".into(),
                name: "Ada".to_string(),
                organization: lt_types::viewer::Organization {
                    id: "org-1".into(),
                    name: "Acme".to_string(),
                    url_key: "acme".to_string(),
                },
            },
        )
        .unwrap();
        let input = CommentCreateInput {
            issue_id: "1".to_string(),
            body: "hi".to_string(),
        };
        enqueue_comment_create(&conn, "local:c", &input).unwrap();

        let rows = crate::db::query_comments(&conn, "1").unwrap();
        assert_eq!(rows[0].author(), "Ada");
    }

    #[test]
    fn ack_comment_replaces_temp_with_server_copy() {
        let conn = db_with_issue("1");
        let input = CommentCreateInput {
            issue_id: "1".to_string(),
            body: "hi".to_string(),
        };
        enqueue_comment_create(&conn, "local:c", &input).unwrap();
        let seq = pending(&conn)[0].seq;

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
        ack_comment_create(
            &conn,
            seq,
            &CommentAck {
                temp_id: "local:c",
                issue_id: "1",
                comment: &comment,
            },
        )
        .unwrap();

        let ids: Vec<String> = {
            let rows = crate::db::query_comments(&conn, "1").unwrap();
            rows.into_iter().map(|c| c.id.into_inner()).collect()
        };
        assert_eq!(ids, ["c-real"]);
        assert!(pending(&conn).is_empty());
    }
}
