//! Workflow-state list reads (the new-issue modal's state picker).

use anyhow::Result;
use lt_types::states::{WorkflowStatesQuery, query};
use lt_types::types::WorkflowState;

use crate::client::GraphqlTransport;
use crate::graphql::fetch_team_scoped;

/// List a team's workflow states.
pub fn fetch(transport: &dyn GraphqlTransport, team_id: &str) -> Result<Vec<WorkflowState>> {
    fetch_team_scoped::<WorkflowStatesQuery>(transport, &query(), team_id)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::client::FakeTransport;

    #[test]
    fn fetch_extracts_nodes() {
        let transport = FakeTransport::new(vec![json!({
            "team": { "states": { "nodes": [
                { "id": "s1", "name": "Todo" },
                { "id": "s2", "name": "Done" }
            ] } }
        })]);
        let states = fetch(&transport, "t1").unwrap();
        assert_eq!(
            states.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["Todo", "Done"]
        );
        assert_eq!(transport.variables(0)["teamId"], json!("t1"));
    }
}
