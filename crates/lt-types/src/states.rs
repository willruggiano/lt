//! The workflow-state list query (the new-issue modal's state picker),
//! modelled as a cynic `QueryFragment`. The fetch lives in `lt-upstream`.

use cynic::QueryBuilder;

use crate::graphql::GraphqlOperation;
use crate::schema;
use crate::types::WorkflowState;

#[derive(cynic::QueryVariables, Clone)]
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
// Team-scoped fetch (lt-runtime::teams::sync_team_data)
// ---------------------------------------------------------------------------

/// Team-scoped states, reusing [`TeamWithStates`]/[`WorkflowStateConnection`]:
/// the shared [`WorkflowState`] fragment already carries `position`, so this
/// is otherwise identical to [`WorkflowStatesQuery`] -- distinct because the
/// local cache's state/assignee pickers (this query, synced) and the
/// interactive new-issue session (that one, unsynced) are separate call
/// sites.
#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query", variables = "TeamVariables")]
pub struct TeamStatesQuery {
    #[arguments(id: $team_id)]
    pub team: TeamWithStates,
}

impl GraphqlOperation for TeamStatesQuery {
    type Variables = TeamVariables;
    type Output = Vec<WorkflowState>;
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
                { "id": "s1", "name": "Todo", "position": 1.0 },
                { "id": "s2", "name": "Done", "position": 2.0 }
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
            [("Todo", Some(1.0)), ("Done", Some(2.5))]
        );
    }
}
