//! The team-member list query (the new-issue modal's assignee picker),
//! modelled as a cynic `QueryFragment`. The fetch lives in `lt-upstream`.

use cynic::QueryBuilder;

use crate::schema;
use crate::types::User;

#[derive(cynic::QueryVariables)]
pub struct TeamVariables {
    #[cynic(rename = "teamId")]
    pub team_id: String,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query", variables = "TeamVariables")]
pub struct TeamMembersQuery {
    #[arguments(id: $team_id)]
    pub team: TeamWithMembers,
}

/// The built team-members query string.
#[must_use]
pub fn query() -> String {
    TeamMembersQuery::build(TeamVariables {
        team_id: String::new(),
    })
    .query
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Team")]
pub struct TeamWithMembers {
    pub members: UserConnection,
}

#[derive(cynic::QueryFragment)]
pub struct UserConnection {
    pub nodes: Vec<User>,
}

impl crate::graphql::TeamScopedQuery for TeamMembersQuery {
    type Node = User;
    fn into_nodes(self) -> Vec<User> {
        self.team.members.nodes
    }
}

#[cfg(test)]
mod tests {
    use super::query;

    #[test]
    fn query_declares_team_id_variable() {
        let built = query();
        assert!(built.contains("$teamId: String!"));
        assert!(built.contains("members"));
    }
}
