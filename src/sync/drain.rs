//! The outbox drainer: the single base-writer that replays queued local
//! mutations against the API and reconciles the base on success.
//!
//! It runs on the sync thread, before the fetch, so all base writes (drain acks
//! and fetch upserts) are serialized through one owner. A command that fails is
//! left pending with its error recorded and retried on the next sync; one bad
//! command does not block the others behind it.

use anyhow::{Result, bail};
use rusqlite::Connection;

use crate::db::outbox::{self, PendingOp};
use crate::linear::client::GraphqlTransport;
use crate::linear::mutations;

/// Replay every pending outbox command, recording (not propagating) per-command
/// failures so a single bad command never aborts the surrounding sync.
pub fn drain(conn: &Connection, transport: &dyn GraphqlTransport) -> Result<()> {
    for op in outbox::pending_operations(conn)? {
        if let Err(e) = replay(conn, transport, &op) {
            outbox::record_error(conn, op.seq, &e.to_string())?;
        }
    }
    Ok(())
}

fn replay(conn: &Connection, transport: &dyn GraphqlTransport, op: &PendingOp) -> Result<()> {
    let variables = serde_json::from_str(&op.variables)?;
    match op.op_type.as_str() {
        outbox::OP_ISSUE_UPDATE => {
            mutations::post_issue_update(transport, variables)?;
            outbox::ack_issue_update(conn, op.seq, &op.entity_id)?;
        }
        outbox::OP_ISSUE_CREATE => {
            let created = mutations::post_issue_create(transport, variables)?;
            outbox::ack_issue_create(
                conn,
                op.seq,
                &op.entity_id,
                (&created.id, &created.identifier),
            )?;
        }
        outbox::OP_COMMENT_CREATE => {
            let issue_id = variables["input"]["issueId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let created = mutations::post_comment_create(transport, variables)?;
            let comment = crate::db::Comment {
                id: created.id,
                issue_id,
                body: created.body,
                author_name: created.user.map(|u| u.name),
                created_at: created.created_at,
                updated_at: created.updated_at,
                synced_at: String::new(),
            };
            outbox::ack_comment_create(conn, op.seq, &op.entity_id, &comment)?;
        }
        other => bail!("unknown outbox op_type: {other}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::db::outbox::{self, sample_base_issue as base_issue};
    use crate::linear::client::FakeTransport;

    fn db_with_issue(id: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        crate::db::upsert_issues(&conn, &[base_issue(id)]).unwrap();
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
        outbox::enqueue_state_change(&conn, "1", "s-done", "Done").unwrap();

        let transport = FakeTransport::new(vec![json!({ "issueUpdate": { "success": true } })]);
        drain(&conn, &transport).unwrap();

        let state: String = conn
            .query_row("SELECT state_id FROM issues WHERE id = '1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "s-done");
        assert_eq!(pending_count(&conn), 0);
        // The replayed command carried the coalesced variables.
        assert_eq!(transport.variables(0)["input"]["stateId"], json!("s-done"));
    }

    #[test]
    fn drains_issue_create_and_rewrites_temp_id() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        let mut issue = base_issue("temp");
        issue.id = "local:abc".to_string();
        issue.identifier = "NEW".to_string();
        let input = crate::linear::inputs::IssueCreateInput {
            title: "New".to_string(),
            team_id: "ENG".to_string(),
            description: None,
            state_id: None,
            priority: None,
            assignee_id: None,
        };
        outbox::enqueue_issue_create(&conn, &issue, &input).unwrap();

        let transport = FakeTransport::new(vec![json!({
            "issueCreate": { "success": true, "issue": {
                "id": "real-1", "identifier": "ENG-42", "title": "New"
            }}
        })]);
        drain(&conn, &transport).unwrap();

        let ident: String = conn
            .query_row(
                "SELECT identifier FROM issues WHERE id = 'real-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ident, "ENG-42");
        assert_eq!(pending_count(&conn), 0);
    }

    #[test]
    fn drains_comment_create_replacing_temp_row() {
        let conn = db_with_issue("1");
        let input = crate::linear::inputs::CommentCreateInput {
            issue_id: "1".to_string(),
            body: "hi".to_string(),
        };
        outbox::enqueue_comment_create(&conn, "local:c", Some("Ada"), &input).unwrap();

        let transport = FakeTransport::new(vec![json!({
            "commentCreate": { "success": true, "comment": {
                "id": "c-real", "body": "hi",
                "createdAt": "2026-01-03T00:00:00Z", "updatedAt": "2026-01-03T00:00:00Z",
                "user": { "name": "Ada" }
            }}
        })]);
        drain(&conn, &transport).unwrap();

        let ids: Vec<String> = crate::db::query_comments(&conn, "1")
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, ["c-real"]);
        assert_eq!(pending_count(&conn), 0);
    }

    #[test]
    fn offline_drain_leaves_command_pending_and_records_error() {
        let conn = db_with_issue("1");
        outbox::enqueue_state_change(&conn, "1", "s-done", "Done").unwrap();

        // No scripted responses: the transport errors, simulating offline.
        let transport = FakeTransport::new(vec![]);
        drain(&conn, &transport).unwrap();

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
}
