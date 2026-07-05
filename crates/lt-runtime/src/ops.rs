//! The generic operation drivers: the local read and the upstream refresh
//! shared by every [`Read`]/[`Upsert`] operation
//! (docs/design/operation-seam-adr.md, "Decision 1").

use anyhow::Result;
use lt_storage::db::{Connection, EntityKey, Read, Upsert};
use lt_types::comments::{CommentConnection, CommentsQuery, CommentsVariables};
use lt_types::issues::IssuesQuery;
use lt_types::members::TeamMembersQuery;
use lt_types::pagination::PageInfo;
use lt_types::states::TeamStatesQuery;
use lt_types::teams::TeamsQuery;
use lt_upstream::client::{GraphqlTransport, execute};

/// One-shot local read: `Op::read` over `conn`. The search overlay's
/// debounced preview and the CLI's cached reads share this path.
pub fn load<Op: Read>(conn: &Connection, vars: &Op::Variables) -> Result<Op::Output> {
    Op::read(conn, vars)
}

/// Upstream refresh: fetch `Op` through `transport`, then upsert its output
/// into the cache, returning the entity keys the upsert touched
/// (docs/design/operation-seam-adr.md, "Decision 5").
pub fn refresh<Op>(
    conn: &Connection,
    transport: &dyn GraphqlTransport,
    vars: Op::Variables,
) -> Result<Vec<EntityKey>>
where
    Op: Upsert,
    Op::Variables: Clone,
{
    let out = execute::<Op>(transport, vars.clone())?;
    Op::upsert(conn, &vars, &out)
}

/// Refresh an issue's comment thread to exhaustion: a composed operation's
/// refresh may paginate nested connections across multiple wire requests
/// while staying one operation type
/// (docs/design/operation-seam-adr.md, "Decision 3"). Pages via
/// [`lt_upstream::comments::fetch_all`], then upserts the merged thread once
/// through [`CommentsQuery`]'s replace-set semantics -- upserting per page
/// would have each page's delete wipe the previous page's inserts.
pub fn refresh_comments(
    conn: &Connection,
    transport: &dyn GraphqlTransport,
    issue_id: &str,
) -> Result<Vec<EntityKey>> {
    let nodes = lt_upstream::comments::fetch_all(transport, issue_id)?;
    let vars = CommentsVariables {
        id: issue_id.to_string(),
        after: None,
    };
    let out = CommentConnection {
        nodes,
        page_info: PageInfo {
            has_next_page: false,
            end_cursor: None,
        },
    };
    CommentsQuery::upsert(conn, &vars, &out)
}

/// How a live subscription's background freshness refresh
/// (docs/design/operation-seam-adr.md, "Decision 6") brings its operation up
/// to date from upstream. Distinct from [`Upsert`], which only knows how to
/// write an already-fetched result into the cache: fetching needs
/// `lt-upstream`, which `lt-storage` does not depend on, so this lives here
/// rather than on the storage-side trait. Every operation [`crate::Runtime`]
/// can `subscribe` to implements it: the generic single-page [`refresh`]
/// driver for most operations, [`refresh_comments`]'s fetch-to-exhaustion for
/// `CommentsQuery` (its thread's fetch-all semantics, ADR "Decision 3").
pub trait Refresh: Upsert {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<Vec<EntityKey>>;
}

impl Refresh for IssuesQuery {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<Vec<EntityKey>> {
        refresh::<IssuesQuery>(conn, transport, vars)
    }
}

impl Refresh for TeamsQuery {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<Vec<EntityKey>> {
        refresh::<TeamsQuery>(conn, transport, vars)
    }
}

impl Refresh for TeamStatesQuery {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<Vec<EntityKey>> {
        refresh::<TeamStatesQuery>(conn, transport, vars)
    }
}

impl Refresh for TeamMembersQuery {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<Vec<EntityKey>> {
        refresh::<TeamMembersQuery>(conn, transport, vars)
    }
}

impl Refresh for CommentsQuery {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<Vec<EntityKey>> {
        refresh_comments(conn, transport, &vars.id)
    }
}

#[cfg(test)]
mod tests {
    use lt_storage::db;
    use lt_types::members::{TeamMembersQuery, TeamVariables as MembersTeamVariables};
    use lt_types::states::{TeamStatesQuery, TeamVariables as StatesTeamVariables};
    use lt_types::teams::TeamsQuery;
    use lt_upstream::client::FakeTransport;
    use serde_json::json;

    use super::*;

    fn conn() -> rusqlite::Connection {
        db::Database::memory().unwrap().connect().unwrap()
    }

    #[test]
    fn refresh_teams_upserts_the_fetched_set() {
        let conn = conn();
        let transport = FakeTransport::new(vec![json!({
            "teams": { "nodes": [
                { "id": "t1", "name": "Eng" },
                { "id": "t2", "name": "Design" }
            ] }
        })]);
        let touched = refresh::<TeamsQuery>(&conn, &transport, ()).unwrap();
        assert_eq!(touched, vec![EntityKey::Teams]);
        let teams = db::query_teams(&conn).unwrap();
        assert_eq!(
            teams.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["Design", "Eng"]
        );
    }

    #[test]
    fn refresh_teams_missing_data_returns_error() {
        let conn = conn();
        let transport = FakeTransport::new(vec![json!({})]);
        assert!(refresh::<TeamsQuery>(&conn, &transport, ()).is_err());
    }

    #[test]
    fn refresh_team_states_and_members_writes_positions_and_memberships() {
        let conn = conn();
        let transport = FakeTransport::new(vec![json!({ "team": { "states": { "nodes": [
            { "id": "s1", "name": "Todo", "position": 1.0 },
            { "id": "s2", "name": "Done", "position": 2.0 }
        ] } } })]);
        let touched = refresh::<TeamStatesQuery>(
            &conn,
            &transport,
            StatesTeamVariables {
                team_id: "t1".to_string(),
            },
        )
        .unwrap();
        assert_eq!(
            touched,
            vec![EntityKey::WorkflowStates {
                team_id: "t1".to_string()
            }]
        );

        let transport = FakeTransport::new(vec![json!({ "team": { "members": { "nodes": [
            { "id": "u1", "name": "Ada" },
            { "id": "u2", "name": "Grace" }
        ] } } })]);
        let touched = refresh::<TeamMembersQuery>(
            &conn,
            &transport,
            MembersTeamVariables {
                team_id: "t1".to_string(),
            },
        )
        .unwrap();
        assert_eq!(
            touched,
            vec![EntityKey::TeamMemberships {
                team_id: "t1".to_string()
            }]
        );

        let states = db::query_team_states(&conn, "t1").unwrap();
        assert_eq!(
            states.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["Todo", "Done"]
        );
        let members = db::query_team_members(&conn, "t1").unwrap();
        assert_eq!(
            members.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(),
            ["Ada", "Grace"]
        );
    }

    #[test]
    fn refresh_team_members_replaces_stale_memberships() {
        let conn = conn();
        let vars = MembersTeamVariables {
            team_id: "t1".to_string(),
        };
        let transport = FakeTransport::new(vec![json!({ "team": { "members": { "nodes": [
            { "id": "u1", "name": "Ada" }, { "id": "u2", "name": "Grace" }
        ] } } })]);
        refresh::<TeamMembersQuery>(&conn, &transport, vars.clone()).unwrap();

        let transport = FakeTransport::new(vec![json!({ "team": { "members": { "nodes": [
            { "id": "u1", "name": "Ada" }
        ] } } })]);
        refresh::<TeamMembersQuery>(&conn, &transport, vars).unwrap();

        let members = db::query_team_members(&conn, "t1").unwrap();
        assert_eq!(
            members.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(),
            ["Ada"]
        );
    }

    fn comment_node(id: &str, created_at: &str) -> serde_json::Value {
        json!({
            "id": id, "body": "b",
            "createdAt": created_at, "updatedAt": created_at,
            "user": { "id": "u1", "name": "Alice" },
            "issueId": "i1"
        })
    }

    #[test]
    fn refresh_comments_replaces_existing_with_the_fetched_set() {
        let conn = conn();
        db::upsert_comments(
            &conn,
            &[lt_types::comments::Comment {
                id: "old".into(),
                body: "stale".to_string(),
                created_at: "2025-01-01T00:00:00Z".parse().unwrap(),
                updated_at: "2025-01-01T00:00:00Z".parse().unwrap(),
                user: None,
                issue_id: Some("i1".to_string()),
            }],
        )
        .unwrap();

        let transport = FakeTransport::new(vec![json!({ "issue": { "comments": {
            "nodes": [
                comment_node("c1", "2026-01-01T00:00:00Z"),
                comment_node("c2", "2026-01-02T00:00:00Z")
            ],
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        }}})]);

        let touched = refresh_comments(&conn, &transport, "i1").unwrap();
        assert_eq!(
            touched,
            vec![EntityKey::Comment {
                issue_id: "i1".to_string()
            }]
        );

        let rows = db::query_comments(&conn, "i1").unwrap();
        assert_eq!(
            rows.iter().map(|c| c.id.inner()).collect::<Vec<_>>(),
            ["c1", "c2"]
        );
    }

    #[test]
    fn refresh_comments_paginates_to_exhaustion() {
        let conn = conn();
        let transport = FakeTransport::new(vec![
            json!({ "issue": { "comments": {
                "nodes": [comment_node("c1", "2026-01-01T00:00:00Z")],
                "pageInfo": { "hasNextPage": true, "endCursor": "cur" }
            }}}),
            json!({ "issue": { "comments": {
                "nodes": [comment_node("c2", "2026-01-02T00:00:00Z")],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }}}),
        ]);

        refresh_comments(&conn, &transport, "i1").unwrap();

        let rows = db::query_comments(&conn, "i1").unwrap();
        assert_eq!(
            rows.iter().map(|c| c.id.inner()).collect::<Vec<_>>(),
            ["c1", "c2"]
        );
        assert_eq!(transport.variables(1)["after"], json!("cur"));
    }

    #[test]
    fn refresh_comments_missing_issue_returns_error() {
        let conn = conn();
        let transport = FakeTransport::new(vec![json!({ "issue": null })]);
        assert!(refresh_comments(&conn, &transport, "i1").is_err());
    }

    #[test]
    fn refresh_trait_dispatches_comments_to_fetch_all() {
        let conn = conn();
        let transport = FakeTransport::new(vec![
            json!({ "issue": { "comments": {
                "nodes": [comment_node("c1", "2026-01-01T00:00:00Z")],
                "pageInfo": { "hasNextPage": true, "endCursor": "cur" }
            }}}),
            json!({ "issue": { "comments": {
                "nodes": [comment_node("c2", "2026-01-02T00:00:00Z")],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }}}),
        ]);
        let vars = CommentsVariables {
            id: "i1".to_string(),
            after: None,
        };

        CommentsQuery::refresh(&conn, &transport, vars).unwrap();

        let rows = db::query_comments(&conn, "i1").unwrap();
        assert_eq!(
            rows.iter().map(|c| c.id.inner()).collect::<Vec<_>>(),
            ["c1", "c2"]
        );
    }
}
