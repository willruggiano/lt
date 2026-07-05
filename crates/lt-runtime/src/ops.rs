//! The generic operation drivers: the local read and the upstream refresh
//! shared by every [`Read`]/[`Upsert`] operation
//! (docs/design/operation-seam-adr.md, "Decision 1").

use anyhow::Result;
use lt_storage::db::{Connection, EntityKey, Read, Upsert};
use lt_types::detail::{IssueDetailQuery, IssueDetailVariables};
use lt_types::issues::IssuesQuery;
use lt_types::members::TeamMembersQuery;
use lt_types::new_issue::NewIssueQuery;
use lt_types::states::TeamStatesQuery;
use lt_types::teams::TeamsQuery;
use lt_types::viewer::ViewerQuery;
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

/// Refresh the detail pane's composed document: one wire request for the
/// issue plus its first page of comments/children, then
/// [`lt_upstream::comments::fetch_all`] pages the comment thread to
/// exhaustion -- multiple wire requests, one operation type
/// (docs/design/operation-seam-adr.md, "Decision 3"). Children stay a single
/// first-page fetch (capped at 250 by the document itself): never fetched
/// upstream at all before this operation existed, so this is an upgrade, not
/// a regression. Upserts once through [`IssueDetailQuery`]'s replace-set
/// comment semantics -- upserting per page would have each page's delete
/// wipe the previous page's inserts.
pub fn refresh_issue_detail(
    conn: &Connection,
    transport: &dyn GraphqlTransport,
    issue_id: &str,
) -> Result<Vec<EntityKey>> {
    let vars = IssueDetailVariables {
        id: issue_id.to_string(),
    };
    let out = execute::<IssueDetailQuery>(transport, vars.clone())?;
    let out = match out {
        Some(mut data) => {
            data.comments = lt_upstream::comments::fetch_all(transport, issue_id)?;
            Some(data)
        }
        // `Query.issue(id:)` is non-null on the wire; a missing payload here
        // would be a deserialization bug elsewhere, not a real outcome to
        // paginate comments for.
        None => None,
    };
    IssueDetailQuery::upsert(conn, &vars, &out)
}

/// How a live subscription's background freshness refresh
/// (docs/design/operation-seam-adr.md, "Decision 6") brings its operation up
/// to date from upstream. Distinct from [`Upsert`], which only knows how to
/// write an already-fetched result into the cache: fetching needs
/// `lt-upstream`, which `lt-storage` does not depend on, so this lives here
/// rather than on the storage-side trait. Every operation [`crate::Runtime`]
/// can `subscribe` to implements it: the generic single-page [`refresh`]
/// driver for most operations, [`refresh_issue_detail`]'s fetch-to-exhaustion
/// for `IssueDetailQuery`'s comment thread (ADR "Decision 3").
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

impl Refresh for IssueDetailQuery {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<Vec<EntityKey>> {
        refresh_issue_detail(conn, transport, &vars.id)
    }
}

impl Refresh for NewIssueQuery {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<Vec<EntityKey>> {
        refresh::<NewIssueQuery>(conn, transport, vars)
    }
}

impl Refresh for ViewerQuery {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<Vec<EntityKey>> {
        refresh::<ViewerQuery>(conn, transport, vars)
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

    /// A composed `IssueDetailQuery` wire response envelope: the shared issue
    /// fixture plus its comments/children connections.
    fn issue_detail_response(
        id: &str,
        comments: &[serde_json::Value],
        children: &[serde_json::Value],
    ) -> serde_json::Value {
        let mut issue = lt_types::issues::sample_issue_node(id);
        issue["comments"] = json!({
            "nodes": comments,
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        });
        issue["children"] = json!({
            "nodes": children,
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        });
        json!({ "issue": issue })
    }

    #[test]
    fn refresh_issue_detail_writes_the_issue_children_and_comments() {
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

        let transport = FakeTransport::new(vec![
            issue_detail_response(
                "i1",
                &[comment_node("stale-page", "2025-06-01T00:00:00Z")],
                &[lt_types::issues::sample_issue_node("child-1")],
            ),
            // `fetch_all`'s own (single-page) pagination.
            json!({ "issue": { "comments": {
                "nodes": [comment_node("c1", "2026-01-01T00:00:00Z")],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }}}),
        ]);

        let touched = refresh_issue_detail(&conn, &transport, "i1").unwrap();
        assert!(touched.contains(&EntityKey::Issue));
        assert!(touched.contains(&EntityKey::Comment {
            issue_id: "i1".to_string()
        }));

        assert!(db::query_issue_by_id(&conn, "i1").unwrap().is_some());
        assert!(db::query_issue_by_id(&conn, "child-1").unwrap().is_some());
        let rows = db::query_comments(&conn, "i1").unwrap();
        assert_eq!(
            rows.iter().map(|c| c.id.inner()).collect::<Vec<_>>(),
            ["c1"]
        );
    }

    #[test]
    fn refresh_issue_detail_paginates_comments_to_exhaustion() {
        let conn = conn();
        let transport = FakeTransport::new(vec![
            // The composed document's own (first-page) fetch.
            issue_detail_response("i1", &[comment_node("stale", "2025-01-01T00:00:00Z")], &[]),
            // `fetch_all`'s own pagination, from scratch.
            json!({ "issue": { "comments": {
                "nodes": [comment_node("c1", "2026-01-01T00:00:00Z")],
                "pageInfo": { "hasNextPage": true, "endCursor": "cur" }
            }}}),
            json!({ "issue": { "comments": {
                "nodes": [comment_node("c2", "2026-01-02T00:00:00Z")],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }}}),
        ]);

        refresh_issue_detail(&conn, &transport, "i1").unwrap();

        let rows = db::query_comments(&conn, "i1").unwrap();
        assert_eq!(
            rows.iter().map(|c| c.id.inner()).collect::<Vec<_>>(),
            ["c1", "c2"]
        );
        assert_eq!(transport.variables(2)["after"], json!("cur"));
    }

    #[test]
    fn refresh_issue_detail_wire_decode_error_propagates() {
        let conn = conn();
        let transport = FakeTransport::new(vec![json!({ "issue": null })]);
        assert!(refresh_issue_detail(&conn, &transport, "i1").is_err());
    }

    #[test]
    fn refresh_trait_dispatches_issue_detail_through_refresh_issue_detail() {
        let conn = conn();
        let transport = FakeTransport::new(vec![
            issue_detail_response("i1", &[], &[]),
            json!({ "issue": { "comments": {
                "nodes": [comment_node("c1", "2026-01-01T00:00:00Z")],
                "pageInfo": { "hasNextPage": false, "endCursor": null }
            }}}),
        ]);
        let vars = IssueDetailVariables {
            id: "i1".to_string(),
        };

        IssueDetailQuery::refresh(&conn, &transport, vars).unwrap();

        assert!(db::query_issue_by_id(&conn, "i1").unwrap().is_some());
        let rows = db::query_comments(&conn, "i1").unwrap();
        assert_eq!(
            rows.iter().map(|c| c.id.inner()).collect::<Vec<_>>(),
            ["c1"]
        );
    }

    #[test]
    fn refresh_new_issue_upserts_teams_and_team_scoped_data() {
        let conn = conn();
        let transport = FakeTransport::new(vec![json!({
            "teams": { "nodes": [{ "id": "t1", "name": "Eng" }] },
            "team": {
                "states": { "nodes": [{ "id": "s1", "name": "Todo", "position": 1.0 }] },
                "members": { "nodes": [{ "id": "u1", "name": "Ada" }] }
            }
        })]);

        let touched = refresh::<NewIssueQuery>(
            &conn,
            &transport,
            lt_types::new_issue::NewIssueVariables::new(Some("t1".to_string())),
        )
        .unwrap();

        assert!(touched.contains(&EntityKey::Teams));
        assert!(touched.contains(&EntityKey::WorkflowStates {
            team_id: "t1".to_string()
        }));
        assert_eq!(db::query_teams(&conn).unwrap()[0].name, "Eng");
        assert_eq!(db::query_team_members(&conn, "t1").unwrap()[0].name, "Ada");
    }

    #[test]
    fn refresh_viewer_persists_and_reports_viewer() {
        let conn = conn();
        let transport = FakeTransport::new(vec![json!({
            "viewer": { "id": "u1", "name": "Ada", "organization": { "name": "Acme", "urlKey": "acme" } }
        })]);

        let touched = refresh::<ViewerQuery>(&conn, &transport, ()).unwrap();

        assert_eq!(touched, vec![EntityKey::Viewer]);
        assert_eq!(db::synced_viewer(&conn).unwrap().unwrap().name, "Ada");
    }
}
