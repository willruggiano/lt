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

// ---------------------------------------------------------------------------
// Team-scoped fetch with position (lt-runtime::teams::sync_team_data)
// ---------------------------------------------------------------------------

/// A workflow state carrying `position`, used only by [`TeamStatesQuery`] --
/// the local cache's state/assignee pickers need Linear's stored ordering.
/// The shared [`WorkflowState`] fragment used by the issue fragment stays
/// `{ id, name }`.
#[derive(cynic::QueryFragment, Clone, PartialEq)]
#[cynic(graphql_type = "WorkflowState")]
pub struct WorkflowStateWithPosition {
    pub id: cynic::Id,
    pub name: String,
    pub position: f64,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Team")]
pub struct TeamWithPositionedStates {
    pub states: WorkflowStateWithPositionConnection,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "WorkflowStateConnection")]
pub struct WorkflowStateWithPositionConnection {
    pub nodes: Vec<WorkflowStateWithPosition>,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query", variables = "TeamVariables")]
pub struct TeamStatesQuery {
    #[arguments(id: $team_id)]
    pub team: TeamWithPositionedStates,
}

impl GraphqlOperation for TeamStatesQuery {
    type Variables = TeamVariables;
    type Output = Vec<WorkflowStateWithPosition>;
    const NAME: &'static str = "teamStates";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }

    fn extract(self) -> anyhow::Result<Self::Output> {
        Ok(self.team.states.nodes)
    }
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

    #[test]
    fn team_states_query_declares_team_id_variable_and_position() {
        let built = TeamStatesQuery::operation(TeamVariables {
            team_id: String::new(),
        })
        .query;
        assert!(built.contains("$teamId: String!"));
        assert!(built.contains("position"));
    }

    #[test]
    fn team_states_query_extract_returns_state_nodes_with_position() {
        let data = serde_json::json!({
            "team": { "states": { "nodes": [
                { "id": "s1", "name": "Todo", "position": 1.0 },
                { "id": "s2", "name": "Done", "position": 2.5 }
            ] } }
        });
        let states = serde_json::from_value::<TeamStatesQuery>(data)
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            states
                .iter()
                .map(|s| (s.name.as_str(), s.position))
                .collect::<Vec<_>>(),
            [("Todo", 1.0), ("Done", 2.5)]
        );
    }
}
