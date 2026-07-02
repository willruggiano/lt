//! The team list query (the new-issue modal's team picker), modelled as a
//! cynic `QueryFragment`. The fetch lives in `lt-upstream`.

use cynic::QueryBuilder;

use crate::graphql::GraphqlOperation;
use crate::schema;
use crate::types::Team;

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query")]
pub struct TeamsQuery {
    pub teams: TeamConnection,
}

impl GraphqlOperation for TeamsQuery {
    type Variables = ();
    type Output = Vec<Team>;
    const NAME: &'static str = "teams";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }

    fn extract(self) -> anyhow::Result<Self::Output> {
        Ok(self.teams.nodes)
    }
}

#[derive(cynic::QueryFragment)]
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
    fn extract_returns_team_nodes() {
        let data = serde_json::json!({
            "teams": { "nodes": [{ "id": "t1", "name": "Eng" }, { "id": "t2", "name": "Design" }] }
        });
        let teams = serde_json::from_value::<TeamsQuery>(data)
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            teams.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["Eng", "Design"]
        );
    }
}
