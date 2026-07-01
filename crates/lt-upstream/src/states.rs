//! Workflow-state list reads (the new-issue modal's state picker).

use anyhow::Result;
use lt_types::sync_dto::WorkflowState;

use crate::client::GraphqlTransport;
use crate::graphql::fetch_team_items;

const WORKFLOW_STATES_QUERY: &str = r"
query WorkflowStates($teamId: String!) {
  team(id: $teamId) {
    items: states {
      nodes {
        id
        name
        type
      }
    }
  }
}
";

/// List a team's workflow states.
pub fn fetch(transport: &dyn GraphqlTransport, team_id: &str) -> Result<Vec<WorkflowState>> {
    fetch_team_items(transport, WORKFLOW_STATES_QUERY, team_id)
}
