//! The workflow-state list query (the new-issue modal's state picker),
//! modelled as a cynic `QueryFragment`. The fetch lives in `lt-upstream`.

use cynic::QueryBuilder;

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

/// The built workflow-states query string.
#[must_use]
pub fn query() -> String {
    WorkflowStatesQuery::build(TeamVariables {
        team_id: String::new(),
    })
    .query
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

impl crate::graphql::TeamScopedQuery for WorkflowStatesQuery {
    type Node = WorkflowState;
    fn into_nodes(self) -> Vec<WorkflowState> {
        self.team.states.nodes
    }
}

#[cfg(test)]
mod tests {
    use super::query;

    #[test]
    fn query_declares_team_id_variable() {
        let built = query();
        assert!(built.contains("$teamId: String!"));
        assert!(built.contains("states"));
    }
}
