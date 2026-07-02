//! Team-member list reads (the new-issue modal's assignee picker).

use anyhow::Result;
use lt_types::members::{TeamMembersQuery, query};
use lt_types::types::User;

use crate::client::GraphqlTransport;
use crate::graphql::fetch_team_scoped;

/// List a team's members.
pub fn fetch(transport: &dyn GraphqlTransport, team_id: &str) -> Result<Vec<User>> {
    fetch_team_scoped::<TeamMembersQuery>(transport, &query(), team_id)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::client::FakeTransport;

    #[test]
    fn fetch_extracts_nodes() {
        let transport = FakeTransport::new(vec![json!({
            "team": { "members": { "nodes": [
                { "id": "u1", "name": "Ada" },
                { "id": "u2", "name": "Grace" }
            ] } }
        })]);
        let members = fetch(&transport, "t1").unwrap();
        assert_eq!(
            members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["Ada", "Grace"]
        );
        assert_eq!(transport.variables(0)["teamId"], json!("t1"));
    }
}
