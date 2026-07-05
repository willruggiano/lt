//! The team-member list query (the new-issue modal's assignee picker),
//! modelled as a cynic `QueryFragment`. The fetch lives in `lt-upstream`.

use cynic::QueryBuilder;

use crate::graphql::GraphqlOperation;
use crate::schema;
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
    type Output = Vec<User>;
    const NAME: &'static str = "teamMembers";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }

    fn extract(self) -> anyhow::Result<Self::Output> {
        Ok(self.team.members.nodes)
    }
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
    fn extract_returns_member_nodes() {
        let data = serde_json::json!({
            "team": { "members": { "nodes": [
                { "id": "u1", "name": "Ada" },
                { "id": "u2", "name": "Grace" }
            ] } }
        });
        let members = serde_json::from_value::<TeamMembersQuery>(data)
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["Ada", "Grace"]
        );
    }
}
