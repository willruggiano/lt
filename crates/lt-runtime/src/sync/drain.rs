//! The op-log drainer: the single base-writer that replays queued local
//! mutations against the API and reconciles the base on success.
//!
//! It runs on the sync thread, before the fetch, so all base writes (drain acks
//! and fetch upserts) are serialized through one owner. An op that fails is
//! left pending with its error recorded and retried on the next sync; one bad
//! op does not block the others behind it.

use anyhow::{Result, bail};
use lt_storage::db::op_log::{self, PendingOp};
use lt_types::comments::CommentCreateMutation;
use lt_types::graphql::GraphqlOperation;
use lt_types::issues::{IssueCreateMutation, IssueUpdateMutation};
use lt_upstream::client::{GraphqlTransport, execute};
use rusqlite::Connection;

use crate::ops::{AckContext, Mutation};

/// Replay every sendable pending op, recording (not aborting on) per-op
/// failures. An op that is not yet sendable (a referenced id is still
/// un-synced) is left pending without an error. Returns whether at least one
/// op was replayed (the caller's changed-signal).
pub fn drain(conn: &Connection, transport: &dyn GraphqlTransport) -> Result<bool> {
    let mut changed = false;
    for op in op_log::pending_operations(conn)? {
        match replay(conn, transport, &op) {
            Ok(true) => changed = true,
            Ok(false) => {} // not sendable yet -- left pending, no error
            Err(e) => op_log::record_error(conn, op.seq, &e.to_string())?,
        }
    }
    Ok(changed)
}

/// `Ok(false)` = skipped because not yet sendable; `Ok(true)` = replayed+acked.
fn replay(conn: &Connection, transport: &dyn GraphqlTransport, op: &PendingOp) -> Result<bool> {
    if !op_log::op_is_sendable(conn, op)? {
        return Ok(false);
    }
    match op.operation.as_str() {
        IssueUpdateMutation::NAME => replay_op::<IssueUpdateMutation>(conn, transport, op)?,
        IssueCreateMutation::NAME => replay_op::<IssueCreateMutation>(conn, transport, op)?,
        CommentCreateMutation::NAME => replay_op::<CommentCreateMutation>(conn, transport, op)?,
        other => bail!("unknown op operation: {other}"),
    }
    Ok(true)
}

/// Rebuild the op's wire vars from the row it points at, execute the
/// mutation, then let the operation's own [`Mutation::ack`] reconcile the
/// base and retire it.
fn replay_op<M>(conn: &Connection, transport: &dyn GraphqlTransport, op: &PendingOp) -> Result<()>
where
    M: Mutation,
    M::Output: TryFrom<M, Error = anyhow::Error>,
{
    let vars = M::replay_vars(conn, &op.id)?;
    let out = execute::<M>(transport, vars)?;
    M::ack(
        conn,
        AckContext {
            seq: op.seq,
            id: &op.id,
        },
        out,
    )
}

#[cfg(test)]
mod tests {
    use lt_storage::db::op_log::sample_base_issue as base_issue;
    use lt_types::comments::{CommentCreateMutation, CommentCreateVariables};
    use lt_types::inputs::{CommentCreateInput, IssueCreateInput, IssueUpdateInput};
    use lt_types::issues::{
        IssueCreateMutation, IssueCreateVariables, IssueUpdateMutation, IssueUpdateVariables,
    };
    use lt_upstream::client::FakeTransport;
    use rusqlite::Connection;
    use serde_json::json;

    use super::*;

    fn db_with_issue(id: &str) -> Connection {
        let db = lt_storage::db::Database::memory().unwrap();
        let conn = db.connect().unwrap();
        lt_storage::db::upsert_issues(&conn, &[base_issue(id)]).unwrap();
        conn
    }

    /// Seed a workflow state on `base_issue`'s team ("ENG") so
    /// `enqueue_issue_update`/`ack_issue_update`'s `resolve_state_id` (R2's
    /// resolve-or-error contract) has it cached.
    fn seed_state(conn: &Connection, id: &str, name: &str, position: f64) {
        lt_storage::db::upsert_team_state(
            conn,
            "ENG",
            &lt_types::types::WorkflowState {
                id: id.into(),
                name: name.to_string(),
                position,
            },
        )
        .unwrap();
    }

    fn pending_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM op_log", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn drains_issue_update_and_reconciles_base() {
        let conn = db_with_issue("1");
        seed_state(&conn, "s-done", "Done", 2.0);
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

        // No server issue in the response: the ack only stamps synced_at --
        // the edit was already applied in place at enqueue.
        let transport = FakeTransport::new(vec![
            json!({ "issueUpdate": { "success": true, "issue": null } }),
        ]);
        assert!(drain(&conn, &transport).unwrap());

        let state: String = conn
            .query_row("SELECT state_id FROM issues WHERE id = '1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "s-done");
        assert_eq!(pending_count(&conn), 0);
        // `replay_vars` rebuilt the input from the in-place row.
        assert_eq!(transport.variables(0)["input"]["stateId"], json!("s-done"));
    }

    #[test]
    fn drains_issue_update_prefers_server_issue_when_present() {
        let conn = db_with_issue("1");
        seed_state(&conn, "s-done", "Done", 2.0);
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

        // The server returns full truth, including a state the local edit
        // never recorded (e.g. a workflow automation moved it further). Must
        // be cached too, or `ack_issue_update`'s upsert resolves it to NULL.
        seed_state(&conn, "s-merged", "Merged", 3.0);
        let mut server_issue = lt_upstream::issues::sample_issue_node("1");
        server_issue["state"] = json!({ "id": "s-merged", "name": "Merged", "position": 3.0 });
        let transport = FakeTransport::new(vec![
            json!({ "issueUpdate": { "success": true, "issue": server_issue } }),
        ]);
        drain(&conn, &transport).unwrap();

        let state: String = conn
            .query_row("SELECT state_id FROM issues WHERE id = '1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "s-merged");
        assert_eq!(pending_count(&conn), 0);
    }

    #[test]
    fn drains_issue_create_and_attaches_server_id() {
        let db = lt_storage::db::Database::memory().unwrap();
        let conn = db.connect().unwrap();
        // The optimistic create defaults to the team's first cached state
        // (sync owns workflow states; issue upserts never write them).
        seed_state(&conn, "s-todo", "Todo", 1.0);
        let input = IssueCreateInput {
            title: "New".to_string(),
            team_id: "ENG".to_string(),
            description: None,
            state_id: None,
            priority: None,
            assignee_id: None,
        };
        IssueCreateMutation::enqueue(&conn, IssueCreateVariables { input }).unwrap();

        let mut server_issue = lt_upstream::issues::sample_issue_node("1");
        server_issue["id"] = json!("real-1");
        server_issue["identifier"] = json!("ENG-42");
        let transport = FakeTransport::new(vec![
            json!({ "issueCreate": { "success": true, "issue": server_issue } }),
        ]);
        drain(&conn, &transport).unwrap();

        let ident: String = conn
            .query_row(
                "SELECT identifier FROM issues WHERE id = 'real-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ident, "ENG-42");
        // The temp row is gone, not just renamed in place.
        let temp: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM issues WHERE identifier = 'NEW'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(temp, 0);
        assert_eq!(pending_count(&conn), 0);
    }

    #[test]
    fn drains_comment_create_and_attaches_server_id() {
        let conn = db_with_issue("1");
        let input = CommentCreateInput {
            issue_id: "1".to_string(),
            body: "hi".to_string(),
        };
        CommentCreateMutation::enqueue(&conn, CommentCreateVariables { input }).unwrap();

        let transport = FakeTransport::new(vec![json!({
            "commentCreate": { "success": true, "comment": {
                "id": "c-real", "body": "hi",
                "createdAt": "2026-01-03T00:00:00Z", "updatedAt": "2026-01-03T00:00:00Z",
                "user": { "id": "u1", "name": "Ada" },
                "issueId": "1"
            }}
        })]);
        drain(&conn, &transport).unwrap();

        let ids: Vec<String> = lt_storage::db::query_comments(&conn, "1")
            .unwrap()
            .into_iter()
            .map(|c| c.id.into_inner())
            .collect();
        assert_eq!(ids, ["c-real"]);
        assert_eq!(pending_count(&conn), 0);
    }

    #[test]
    fn offline_drain_leaves_command_pending_and_records_error() {
        let conn = db_with_issue("1");
        seed_state(&conn, "s-done", "Done", 2.0);
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

        // No scripted responses: the transport errors, simulating offline.
        let transport = FakeTransport::new(vec![]);
        assert!(!drain(&conn, &transport).unwrap());

        assert_eq!(pending_count(&conn), 1);
        let (attempts, last_error): (i64, Option<String>) = conn
            .query_row(
                "SELECT attempts, last_error FROM op_log WHERE id = '1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(attempts, 1);
        assert!(last_error.is_some());

        // The optimistic edit applied in place at enqueue, so it still
        // renders even while the op is pending.
        let state: String = conn
            .query_row("SELECT state_id FROM issues WHERE id = '1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "s-done");
    }

    #[test]
    fn unknown_op_operation_is_recorded_as_an_error() {
        let conn = db_with_issue("1");
        conn.execute(
            "INSERT INTO op_log (operation, id) VALUES ('bogus', '1')",
            [],
        )
        .unwrap();

        let transport = FakeTransport::new(vec![]);
        assert!(!drain(&conn, &transport).unwrap());

        let last_error: Option<String> = conn
            .query_row("SELECT last_error FROM op_log WHERE id = '1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(last_error.unwrap().contains("bogus"));
    }
}
