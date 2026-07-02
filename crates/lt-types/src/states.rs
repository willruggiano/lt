//! The workflow-state list query (the new-issue modal's state picker),
//! modelled as a cynic `QueryFragment`. The fetch lives in `lt-upstream`.

use cynic::QueryBuilder;

use crate::graphql::GraphqlOperation;
use crate::schema;
use crate::types::WorkflowState;

#[derive(cynic::QueryVariables)]
pub struct TeamVariables {
    #[cynic(rename = "teamId")]
    pub team_id: String,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query", variables = "TeamVariables")]
pub struct WorkflowStatesQuery {
    #[arguments(id: $team_id)]
    pub team: TeamWithStates,
}

impl GraphqlOperation for WorkflowStatesQuery {
    type Variables = TeamVariables;
    type Output = Vec<WorkflowState>;
    const NAME: &'static str = "workflowStates";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }

    fn extract(self) -> anyhow::Result<Self::Output> {
        Ok(self.team.states.nodes)
    }
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Team")]
pub struct TeamWithStates {
    pub states: WorkflowStateConnection,
}

#[derive(cynic::QueryFragment)]
pub struct WorkflowStateConnection {
    pub nodes: Vec<WorkflowState>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_declares_team_id_variable() {
        let built = WorkflowStatesQuery::operation(TeamVariables {
            team_id: String::new(),
        })
        .query;
        assert!(built.contains("$teamId: String!"));
        assert!(built.contains("states"));
    }

    #[test]
    fn extract_returns_state_nodes() {
        let data = serde_json::json!({
            "team": { "states": { "nodes": [
                { "id": "s1", "name": "Todo" },
                { "id": "s2", "name": "Done" }
            ] } }
        });
        let states = serde_json::from_value::<WorkflowStatesQuery>(data)
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            states.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["Todo", "Done"]
        );
    }
}
