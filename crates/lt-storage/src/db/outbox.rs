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
use rusqlite::{Connection, params};
use serde_json::json;

use lt_types::inputs::{CommentCreateInput, Field, IssueCreateInput, IssueUpdateInput};
use lt_types::types;

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
    tx.execute(
        "INSERT INTO pending_overlay (entity_id, field, value) VALUES (?1, ?2, ?3)
         ON CONFLICT(entity_id, field) DO UPDATE SET value = excluded.value",
        params![entity_id, field.as_str(), value],
    )
    .context("failed to write pending overlay")?;
    Ok(())
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
    tx.execute(
        "DELETE FROM outbox WHERE op_type = ?1 AND entity_id = ?2 AND status = 'pending'",
        params![op_type, entity_id],
    )
    .context("failed to clear superseded outbox command")?;
    insert_pending(tx, op_type, entity_id, variables)
}

/// Insert a new pending command. Used for creates, which never coalesce.
fn insert_pending(
    tx: &Connection,
    op_type: &str,
    entity_id: &str,
    variables: &serde_json::Value,
) -> Result<()> {
    tx.execute(
        "INSERT INTO outbox (op_type, entity_id, variables, status, attempts, created_at)
         VALUES (?1, ?2, ?3, 'pending', 0, ?4)",
        params![
            op_type,
            entity_id,
            variables.to_string(),
            Utc::now().to_rfc3339()
        ],
    )
    .context("failed to enqueue outbox command")?;
    Ok(())
}

/// Read every `(field, value)` overlay row for an issue.
fn overlay_rows(conn: &Connection, issue_id: &str) -> Result<Vec<(String, Option<String>)>> {
    let mut stmt = conn
        .prepare("SELECT field, value FROM pending_overlay WHERE entity_id = ?1")
        .context("failed to prepare overlay query")?;
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
    crate::db::issues::upsert_named_entity(&tx, "workflow_states", state_id, Some(state_name))?;
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
            crate::db::issues::upsert_named_entity(&tx, "users", id, Some(name))?;
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
        &optimistic.id,
        &json!({ "input": input }),
    )?;
    tx.commit().context("failed to commit issue create")?;
    Ok(())
}

/// Enqueue a comment create: insert an optimistic comment row under the client
/// `temp_id` and queue the `commentCreate` command. The issue and body come from
/// `input`.
pub fn enqueue_comment_create(
    conn: &Connection,
    temp_id: &str,
    author_name: Option<&str>,
    input: &CommentCreateInput,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let now = Utc::now().to_rfc3339();
    crate::db::upsert_comments(
        &tx,
        &[crate::db::Comment {
            id: temp_id.to_string(),
            issue_id: input.issue_id.clone(),
            body: input.body.clone(),
            author_name: author_name.map(str::to_string),
            created_at: now.clone(),
            updated_at: now,
            synced_at: String::new(),
        }],
    )?;
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
    let mut stmt = conn
        .prepare(
            "SELECT seq, op_type, entity_id, variables FROM outbox
             WHERE status = 'pending' ORDER BY seq",
        )
        .context("failed to prepare outbox query")?;
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
/// atomically. The overlay values become the new base truth, so the read model
/// never flickers back to the pre-edit value.
pub fn ack_issue_update(conn: &Connection, seq: i64, issue_id: &str) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for (field, value) in overlay_rows(&tx, issue_id)? {
        match field.as_str() {
            "state" => {
                tx.execute(
                    "UPDATE issues SET state_id = ?1 WHERE id = ?2",
                    params![value, issue_id],
                )?;
            }
            "assignee" => {
                tx.execute(
                    "UPDATE issues SET assignee_id = ?1 WHERE id = ?2",
                    params![value, issue_id],
                )?;
            }
            "priority" => {
                let label = value
                    .as_deref()
                    .and_then(|v| v.parse::<u8>().ok())
                    .map_or("No priority", types::priority_u8_to_label);
                tx.execute(
                    "UPDATE issues SET priority_label = ?1 WHERE id = ?2",
                    params![label, issue_id],
                )?;
            }
            _ => {}
        }
    }
    tx.execute(
        "DELETE FROM pending_overlay WHERE entity_id = ?1",
        params![issue_id],
    )?;
    delete_command(&tx, seq)?;
    tx.commit().context("failed to commit issue-update ack")?;
    Ok(())
}

/// Rewrite the optimistic temp issue row with the server-assigned id and
/// identifier, then retire the command. `server` is the `(id, identifier)` pair
/// from the `issueCreate` response.
pub fn ack_issue_create(
    conn: &Connection,
    seq: i64,
    temp_id: &str,
    server: (&str, &str),
) -> Result<()> {
    let (real_id, identifier) = server;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE issue_labels SET issue_id = ?1 WHERE issue_id = ?2",
        params![real_id, temp_id],
    )?;
    tx.execute(
        "UPDATE issues SET id = ?1, identifier = ?2 WHERE id = ?3",
        params![real_id, identifier, temp_id],
    )?;
    delete_command(&tx, seq)?;
    tx.commit().context("failed to commit issue-create ack")?;
    Ok(())
}

/// Replace the optimistic temp comment with the server copy and retire the
/// command, atomically -- so the comment never blinks out between ack and the
/// next per-issue comment sync.
pub fn ack_comment_create(
    conn: &Connection,
    seq: i64,
    temp_id: &str,
    comment: &crate::db::Comment,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM issue_comments WHERE id = ?1", params![temp_id])?;
    crate::db::upsert_comments(&tx, std::slice::from_ref(comment))?;
    delete_command(&tx, seq)?;
    tx.commit().context("failed to commit comment-create ack")?;
    Ok(())
}

/// Record a failed drain attempt; the command stays pending for the next sync.
pub fn record_error(conn: &Connection, seq: i64, error: &str) -> Result<()> {
    crate::db::execute(
        conn,
        "UPDATE outbox SET attempts = attempts + 1, last_error = ?1 WHERE seq = ?2",
        params![error, seq],
        "record outbox error",
    )
}

fn delete_command(tx: &Connection, seq: i64) -> Result<()> {
    tx.execute("DELETE FROM outbox WHERE seq = ?1", params![seq])
        .context("failed to delete outbox command")?;
    Ok(())
}

/// A minimal base issue for the write-path tests, shared with the sync drainer
/// tests so the fixture is defined once.
#[cfg(any(test, feature = "test-util"))]
pub fn sample_base_issue(id: &str) -> types::Issue {
    types::Issue {
        id: id.to_string(),
        identifier: format!("ENG-{id}"),
        title: format!("issue {id}"),
        priority_label: "Normal".to_string(),
        priority: 3,
        state: types::State {
            id: "s-todo".to_string(),
            name: "Todo".to_string(),
        },
        assignee: None,
        team: types::Team {
            id: "ENG".to_string(),
            name: "Engineering".to_string(),
        },
        description: None,
        labels: types::LabelConnection { nodes: Vec::new() },
        project: None,
        cycle: None,
        creator: None,
        parent: None,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-02T00:00:00Z".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{sample_base_issue as base_issue, *};

    fn db_with_issue(id: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
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
    fn ack_issue_update_applies_overlay_to_base_and_clears() {
        let conn = db_with_issue("1");
        enqueue_state_change(&conn, "1", "s-done", "Done").unwrap();
        enqueue_assignee_change(&conn, "1", None).unwrap();

        let seq = pending(&conn)[0].seq;
        ack_issue_update(&conn, seq, "1").unwrap();

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
    fn enqueue_create_inserts_temp_row_and_command() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        let mut issue = base_issue("temp");
        issue.id = "local:abc".to_string();
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
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        let mut issue = base_issue("temp");
        issue.id = "local:abc".to_string();
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

        ack_issue_create(&conn, seq, "local:abc", ("real-1", "ENG-42")).unwrap();

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
    fn ack_comment_replaces_temp_with_server_copy() {
        let conn = db_with_issue("1");
        let input = CommentCreateInput {
            issue_id: "1".to_string(),
            body: "hi".to_string(),
        };
        enqueue_comment_create(&conn, "local:c", Some("Ada"), &input).unwrap();
        let seq = pending(&conn)[0].seq;

        ack_comment_create(
            &conn,
            seq,
            "local:c",
            &crate::db::Comment {
                id: "c-real".to_string(),
                issue_id: "1".to_string(),
                body: "hi".to_string(),
                author_name: Some("Ada".to_string()),
                created_at: "2026-01-03T00:00:00Z".to_string(),
                updated_at: "2026-01-03T00:00:00Z".to_string(),
                synced_at: String::new(),
            },
        )
        .unwrap();

        let ids: Vec<String> = {
            let rows = crate::db::query_comments(&conn, "1").unwrap();
            rows.into_iter().map(|c| c.id).collect()
        };
        assert_eq!(ids, ["c-real"]);
        assert!(pending(&conn).is_empty());
    }
}
