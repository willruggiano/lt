//! The team-member list query (the new-issue modal's assignee picker),
//! modelled as a cynic `QueryFragment`. The fetch lives in `lt-upstream`.

use cynic::QueryBuilder;
use linear_schema::linear as schema;

use crate::graphql::GraphqlOperation;
use crate::types::User;

#[derive(cynic::QueryVariables, Clone)]
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

impl GraphqlOperation for TeamMembersQuery {
    type Variables = TeamVariables;
    type Output = UserConnection;
    const NAME: &'static str = "teamMembers";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }
}

impl TryFrom<TeamMembersQuery> for UserConnection {
    type Error = anyhow::Error;

    fn try_from(op: TeamMembersQuery) -> anyhow::Result<Self> {
        Ok(op.team.members)
    }
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Team")]
pub struct TeamWithMembers {
    pub members: UserConnection,
}

#[derive(Default, cynic::QueryFragment)]
pub struct UserConnection {
    pub nodes: Vec<User>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_declares_team_id_variable() {
        let built = TeamMembersQuery::operation(TeamVariables {
            team_id: String::new(),
        })
        .query;
        assert!(built.contains("$teamId: String!"));
        assert!(built.contains("members"));
    }

    #[test]
    fn recomposes_into_the_member_connection() {
        let data = serde_json::json!({
            "team": { "members": { "nodes": [
                { "id": "u1", "name": "Ada" },
                { "id": "u2", "name": "Grace" }
            ] } }
        });
        let members: UserConnection = serde_json::from_value::<TeamMembersQuery>(data)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            members
                .nodes
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            ["Ada", "Grace"]
        );
    }
}
