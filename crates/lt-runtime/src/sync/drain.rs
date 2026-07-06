//! The outbox drainer: the single base-writer that replays queued local
//! mutations against the API and reconciles the base on success.
//!
//! It runs on the sync thread, before the fetch, so all base writes (drain acks
//! and fetch upserts) are serialized through one owner. A command that fails is
//! left pending with its error recorded and retried on the next sync; one bad
//! command does not block the others behind it.

use anyhow::{Result, bail};
use lt_storage::db::outbox::{self, PendingOp};
use lt_storage::db::{AckContext, EntityKey, Mutation};
use lt_types::comments::CommentCreateMutation;
use lt_types::graphql::GraphqlOperation;
use lt_types::issues::{IssueCreateMutation, IssueUpdateMutation};
use lt_upstream::client::{GraphqlTransport, execute};
use rusqlite::Connection;
use serde::de::DeserializeOwned;

/// Replay every pending outbox command, recording (not propagating) per-command
/// failures so a single bad command never aborts the surrounding sync. Returns
/// the union of entity keys every successfully-replayed command touched
/// (docs/design/operation-seam-adr.md, "Decision 5"), for the sync cycle's own
/// propagation.
pub fn drain(conn: &Connection, transport: &dyn GraphqlTransport) -> Result<Vec<EntityKey>> {
    let mut touched = Vec::new();
    for op in outbox::pending_operations(conn)? {
        match replay(conn, transport, &op) {
            Ok(keys) => touched.extend(keys),
            Err(e) => outbox::record_error(conn, op.seq, &e.to_string())?,
        }
    }
    Ok(touched)
}

fn replay(
    conn: &Connection,
    transport: &dyn GraphqlTransport,
    op: &PendingOp,
) -> Result<Vec<EntityKey>> {
    match op.op_type.as_str() {
        IssueUpdateMutation::NAME => replay_op::<IssueUpdateMutation>(conn, transport, op),
        IssueCreateMutation::NAME => replay_op::<IssueCreateMutation>(conn, transport, op),
        CommentCreateMutation::NAME => replay_op::<CommentCreateMutation>(conn, transport, op),
        other => bail!("unknown outbox op_type: {other}"),
    }
}

/// Replay one operation: decode its stored variables, execute the mutation on
/// the wire, then let the operation's own [`Mutation::ack`] reconcile the base
/// and retire the command.
fn replay_op<M>(
    conn: &Connection,
    transport: &dyn GraphqlTransport,
    op: &PendingOp,
) -> Result<Vec<EntityKey>>
where
    M: Mutation,
    M::Variables: DeserializeOwned + Clone,
    M::Output: TryFrom<M, Error = anyhow::Error>,
{
    let vars: M::Variables = serde_json::from_str(&op.variables)?;
    let out = execute::<M>(transport, vars.clone())?;
    M::ack(
        conn,
        AckContext {
            seq: op.seq,
            entity_id: &op.entity_id,
            vars: &vars,
        },
        out,
    )
}

#[cfg(test)]
mod tests {
    use lt_storage::db::outbox::sample_base_issue as base_issue;
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

    fn pending_count(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM outbox WHERE status = 'pending'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn drains_issue_update_and_reconciles_base() {
        let conn = db_with_issue("1");
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

        // No server issue in the response: falls back to the overlay
        // reconciliation.
        let transport = FakeTransport::new(vec![
            json!({ "issueUpdate": { "success": true, "issue": null } }),
        ]);
        let touched = drain(&conn, &transport).unwrap();

        let state: String = conn
            .query_row("SELECT state_id FROM issues WHERE id = '1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "s-done");
        assert_eq!(pending_count(&conn), 0);
        assert_eq!(touched, vec![EntityKey::Issue]);
        // The replayed command carried the coalesced variables.
        assert_eq!(transport.variables(0)["input"]["stateId"], json!("s-done"));
    }

    #[test]
    fn drains_issue_update_prefers_server_issue_when_present() {
        let conn = db_with_issue("1");
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

        // The server returns full truth, including a state the overlay never
        // recorded (e.g. a workflow automation moved it further).
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
    fn drains_issue_create_and_rewrites_temp_id() {
        let db = lt_storage::db::Database::memory().unwrap();
        let conn = db.connect().unwrap();
        // The optimistic create defaults to the team's first cached state
        // (sync owns workflow states; issue upserts never write them).
        lt_storage::db::upsert_team_state(
            &conn,
            "ENG",
            &lt_types::types::WorkflowState {
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
        IssueCreateMutation::enqueue(&conn, IssueCreateVariables { input }).unwrap();

        let mut server_issue = lt_upstream::issues::sample_issue_node("1");
        server_issue["id"] = json!("real-1");
        server_issue["identifier"] = json!("ENG-42");
        let transport = FakeTransport::new(vec![
            json!({ "issueCreate": { "success": true, "issue": server_issue } }),
        ]);
        let touched = drain(&conn, &transport).unwrap();
        assert!(touched.contains(&EntityKey::Issue));

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
    fn drains_comment_create_replacing_temp_row() {
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
        let touched = drain(&conn, &transport).unwrap();
        assert_eq!(
            touched,
            vec![EntityKey::Comment {
                issue_id: "1".to_string()
            }]
        );

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
        let touched = drain(&conn, &transport).unwrap();
        assert!(touched.is_empty());

        assert_eq!(pending_count(&conn), 1);
        let (attempts, last_error): (i64, Option<String>) = conn
            .query_row(
                "SELECT attempts, last_error FROM outbox WHERE entity_id = '1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(attempts, 1);
        assert!(last_error.is_some());

        // The overlay is intact, so the edit still renders.
        let overlays: i64 = conn
            .query_row("SELECT COUNT(*) FROM pending_overlay", [], |r| r.get(0))
            .unwrap();
        assert_eq!(overlays, 1);
    }

    #[test]
    fn unknown_op_type_is_recorded_as_an_error() {
        let conn = db_with_issue("1");
        conn.execute(
            "INSERT INTO outbox (op_type, entity_id, variables, status, attempts, created_at) \
             VALUES ('bogus', '1', '{}', 'pending', 0, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        let transport = FakeTransport::new(vec![]);
        drain(&conn, &transport).unwrap();

        let last_error: Option<String> = conn
            .query_row(
                "SELECT last_error FROM outbox WHERE entity_id = '1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(last_error.unwrap().contains("bogus"));
    }
}
