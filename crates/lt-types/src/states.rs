//! The workflow-state list query (the new-issue modal's state picker),
//! modelled as a cynic `QueryFragment`. The fetch lives in `lt-upstream`.

use cynic::QueryBuilder;

use crate::graphql::GraphqlOperation;
use crate::pagination::PageInfo;
use crate::types::WorkflowState;
use crate::{schema, wire};

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
    type Output = WorkflowStateConnection;
    const NAME: &'static str = "workflowStates";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }
}

impl TryFrom<WorkflowStatesQuery> for WorkflowStateConnection {
    type Error = anyhow::Error;

    fn try_from(op: WorkflowStatesQuery) -> anyhow::Result<Self> {
        Ok(op.team.states.into())
    }
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Team")]
pub struct TeamWithStates {
    pub states: wire::WorkflowStateConnection,
}

#[derive(Default)]
pub struct WorkflowStateConnection {
    pub nodes: Vec<WorkflowState>,
}

impl From<wire::WorkflowStateConnection> for WorkflowStateConnection {
    fn from(w: wire::WorkflowStateConnection) -> Self {
        Self {
            nodes: w.nodes.into_iter().map(Into::into).collect(),
        }
    }
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
    type Output = WorkflowStateConnection;
    const NAME: &'static str = "teamStates";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }
}

impl TryFrom<TeamStatesQuery> for WorkflowStateConnection {
    type Error = anyhow::Error;

    fn try_from(op: TeamStatesQuery) -> anyhow::Result<Self> {
        Ok(op.team.states.into())
    }
}

// ---------------------------------------------------------------------------
// Org-wide fetch (the sync cycle: every workflow state a synced issue could
// reference must be locally known before any issue page lands, since sync
// owns workflow states -- issue upserts no longer write them)
// ---------------------------------------------------------------------------

#[derive(cynic::QueryVariables, Clone)]
pub struct AllWorkflowStatesVariables {
    pub first: i32,
    pub after: Option<String>,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query", variables = "AllWorkflowStatesVariables")]
pub struct AllWorkflowStatesQuery {
    #[arguments(first: $first, after: $after)]
    pub workflow_states: wire::WorkflowStateWithTeamConnection,
}

impl GraphqlOperation for AllWorkflowStatesQuery {
    type Variables = AllWorkflowStatesVariables;
    type Output = WorkflowStateWithTeamConnection;
    const NAME: &'static str = "allWorkflowStates";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }
}

impl TryFrom<AllWorkflowStatesQuery> for WorkflowStateWithTeamConnection {
    type Error = anyhow::Error;

    fn try_from(op: AllWorkflowStatesQuery) -> anyhow::Result<Self> {
        Ok(op.workflow_states.into())
    }
}

#[derive(Default)]
pub struct WorkflowStateWithTeamConnection {
    pub nodes: Vec<WorkflowStateWithTeam>,
    pub page_info: PageInfo,
}

impl From<wire::WorkflowStateWithTeamConnection> for WorkflowStateWithTeamConnection {
    fn from(w: wire::WorkflowStateWithTeamConnection) -> Self {
        Self {
            nodes: w.nodes.into_iter().map(Into::into).collect(),
            page_info: w.page_info,
        }
    }
}

/// A workflow state carrying its own team's id, so the org-wide fetch above
/// can upsert each state team-scoped without a second, per-team round trip.
pub struct WorkflowStateWithTeam {
    pub id: cynic::Id,
    pub name: String,
    pub position: f64,
    pub team: TeamRef,
}

impl From<wire::WorkflowStateWithTeam> for WorkflowStateWithTeam {
    fn from(w: wire::WorkflowStateWithTeam) -> Self {
        Self {
            id: w.id,
            name: w.name,
            position: w.position,
            team: w.team.into(),
        }
    }
}

pub struct TeamRef {
    pub id: cynic::Id,
}

impl From<wire::TeamRef> for TeamRef {
    fn from(w: wire::TeamRef) -> Self {
        Self { id: w.id }
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
    fn recomposes_into_the_state_connection() {
        let data = serde_json::json!({
            "team": { "states": { "nodes": [
                { "id": "s1", "name": "Todo", "position": 1.0 },
                { "id": "s2", "name": "Done", "position": 2.0 }
            ] } }
        });
        let states: WorkflowStateConnection = serde_json::from_value::<WorkflowStatesQuery>(data)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            states
                .nodes
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
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
    fn team_states_query_recomposes_state_nodes_with_position() {
        let data = serde_json::json!({
            "team": { "states": { "nodes": [
                { "id": "s1", "name": "Todo", "position": 1.0 },
                { "id": "s2", "name": "Done", "position": 2.5 }
            ] } }
        });
        let states: WorkflowStateConnection = serde_json::from_value::<TeamStatesQuery>(data)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            states
                .nodes
                .iter()
                .map(|s| (s.name.as_str(), s.position))
                .collect::<Vec<_>>(),
            [("Todo", 1.0), ("Done", 2.5)]
        );
    }

    #[test]
    fn all_workflow_states_query_declares_first_and_after_variables() {
        let built = AllWorkflowStatesQuery::operation(AllWorkflowStatesVariables {
            first: 250,
            after: None,
        })
        .query;
        assert!(built.contains("$first: Int"));
        assert!(built.contains("$after: String"));
        assert!(built.contains("workflowStates"));
        assert!(built.contains("team"));
    }

    #[test]
    fn all_workflow_states_query_recomposes_nodes_and_page_info() {
        let data = serde_json::json!({
            "workflowStates": {
                "nodes": [
                    { "id": "s1", "name": "Todo", "position": 1.0, "team": { "id": "t1" } },
                    { "id": "s2", "name": "Done", "position": 2.0, "team": { "id": "t2" } }
                ],
                "pageInfo": { "hasNextPage": true, "endCursor": "cur" }
            }
        });
        let page: WorkflowStateWithTeamConnection =
            serde_json::from_value::<AllWorkflowStatesQuery>(data)
                .unwrap()
                .try_into()
                .unwrap();
        assert_eq!(
            page.nodes
                .iter()
                .map(|s| (s.name.as_str(), s.team.id.inner()))
                .collect::<Vec<_>>(),
            [("Todo", "t1"), ("Done", "t2")]
        );
        assert!(page.page_info.has_next_page);
        assert_eq!(page.page_info.end_cursor.as_deref(), Some("cur"));
    }
}
