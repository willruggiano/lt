//! Persistence of team metadata: the team list and one team's workflow
//! states + memberships. Targeted sync mirroring `comments.rs` -- fetch from
//! the API edge, then upsert into local storage.

use anyhow::Result;
use lt_storage::db;
use lt_types::members::TeamMembersQuery;
use lt_types::states::{TeamStatesQuery, TeamVariables as StatesTeamVariables};
use lt_types::teams::TeamsQuery;
use lt_upstream::client::{GraphqlTransport, execute};
use rusqlite::Connection;

/// Fetch every team from the Linear API and upsert it into the local `teams`
/// table.
pub fn sync_teams(conn: &Connection, transport: &dyn GraphqlTransport) -> Result<()> {
    let teams = execute::<TeamsQuery>(transport, ())?;
    db::upsert_teams(conn, &teams)
}

/// Fetch one team's workflow states and members from the Linear API and
/// upsert both into local storage: states via the team-scoped upsert (id,
/// name, `team_id`, position); users first, then memberships replaced
/// wholesale so the membership join resolves names and a removed member is
/// dropped rather than left stale.
pub fn sync_team_data(
    conn: &Connection,
    transport: &dyn GraphqlTransport,
    team_id: &str,
) -> Result<()> {
    let states = execute::<TeamStatesQuery>(
        transport,
        StatesTeamVariables {
            team_id: team_id.to_string(),
        },
    )?;
    for state in &states {
        db::upsert_team_state(conn, team_id, state)?;
    }

    let members = execute::<TeamMembersQuery>(
        transport,
        lt_types::members::TeamVariables {
            team_id: team_id.to_string(),
        },
    )?;
    db::upsert_users(conn, &members)?;
    let member_ids: Vec<&str> = members.iter().map(|u| u.id.inner()).collect();
    db::replace_team_memberships(conn, team_id, &member_ids)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use lt_upstream::client::FakeTransport;
    use serde_json::json;

    use super::*;

    fn teams_page(teams: &[(&str, &str)]) -> serde_json::Value {
        let nodes: Vec<_> = teams
            .iter()
            .map(|(id, name)| json!({ "id": id, "name": name }))
            .collect();
        json!({ "teams": { "nodes": nodes } })
    }

    fn team_states_page(states: &[(&str, &str, f64)]) -> serde_json::Value {
        let nodes: Vec<_> = states
            .iter()
            .map(|(id, name, position)| json!({ "id": id, "name": name, "position": position }))
            .collect();
        json!({ "team": { "states": { "nodes": nodes } } })
    }

    fn team_members_page(members: &[(&str, &str)]) -> serde_json::Value {
        let nodes: Vec<_> = members
            .iter()
            .map(|(id, name)| json!({ "id": id, "name": name }))
            .collect();
        json!({ "team": { "members": { "nodes": nodes } } })
    }

    fn conn() -> rusqlite::Connection {
        let database = db::Database::memory().unwrap();
        database.connect().unwrap()
    }

    #[test]
    fn sync_teams_upserts_the_fetched_set() {
        let conn = conn();
        let transport = FakeTransport::new(vec![teams_page(&[("t1", "Eng"), ("t2", "Design")])]);
        sync_teams(&conn, &transport).unwrap();

        let teams = db::query_teams(&conn).unwrap();
        assert_eq!(
            teams.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["Design", "Eng"]
        );
    }

    #[test]
    fn sync_team_data_writes_states_and_memberships() {
        let conn = conn();
        let transport = FakeTransport::new(vec![
            team_states_page(&[("s1", "Todo", 1.0), ("s2", "Done", 2.0)]),
            team_members_page(&[("u1", "Ada"), ("u2", "Grace")]),
        ]);
        sync_team_data(&conn, &transport, "t1").unwrap();

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
    fn sync_team_data_replaces_stale_memberships() {
        let conn = conn();
        let transport = FakeTransport::new(vec![
            team_states_page(&[]),
            team_members_page(&[("u1", "Ada"), ("u2", "Grace")]),
        ]);
        sync_team_data(&conn, &transport, "t1").unwrap();

        let transport = FakeTransport::new(vec![
            team_states_page(&[]),
            team_members_page(&[("u1", "Ada")]),
        ]);
        sync_team_data(&conn, &transport, "t1").unwrap();

        let members = db::query_team_members(&conn, "t1").unwrap();
        assert_eq!(
            members.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(),
            ["Ada"]
        );
    }

    #[test]
    fn sync_teams_missing_data_returns_error() {
        let conn = conn();
        let transport = FakeTransport::new(vec![json!({})]);
        assert!(sync_teams(&conn, &transport).is_err());
    }
}
