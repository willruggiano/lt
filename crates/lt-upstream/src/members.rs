//! Team-member list reads (the new-issue modal's assignee picker).

use anyhow::Result;
use lt_storage::sync_port::Member;

use crate::client::GraphqlTransport;
use crate::graphql::fetch_team_items;

const TEAM_MEMBERS_QUERY: &str = r"
query TeamMembers($teamId: String!) {
  team(id: $teamId) {
    items: members {
      nodes {
        id
        name
      }
    }
  }
}
";

/// List a team's members.
pub fn fetch(transport: &dyn GraphqlTransport, team_id: &str) -> Result<Vec<Member>> {
    fetch_team_items(transport, TEAM_MEMBERS_QUERY, team_id)
}
