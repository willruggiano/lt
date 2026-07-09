//! The team list query (the new-issue modal's team picker), modelled as a
//! cynic `QueryFragment`. The fetch lives in `lt-upstream`.

use cynic::QueryBuilder;
use linear_schema::linear as schema;

use crate::graphql::GraphqlOperation;
use crate::types::Team;

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query")]
pub struct TeamsQuery {
    pub teams: TeamConnection,
}

impl GraphqlOperation for TeamsQuery {
    type Variables = ();
    type Output = TeamConnection;
    const NAME: &'static str = "teams";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }
}

impl TryFrom<TeamsQuery> for TeamConnection {
    type Error = anyhow::Error;

    fn try_from(op: TeamsQuery) -> anyhow::Result<Self> {
        Ok(op.teams)
    }
}

#[derive(Default, cynic::QueryFragment)]
pub struct TeamConnection {
    pub nodes: Vec<Team>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_selects_team_nodes() {
        let built = TeamsQuery::operation(()).query;
        assert!(built.contains("teams"));
        assert!(built.contains("nodes"));
    }

    #[test]
    fn recomposes_into_the_team_connection() {
        let data = serde_json::json!({
            "teams": { "nodes": [{ "id": "t1", "name": "Eng" }, { "id": "t2", "name": "Design" }] }
        });
        let teams: TeamConnection = serde_json::from_value::<TeamsQuery>(data)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            teams
                .nodes
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["Eng", "Design"]
        );
    }
}
