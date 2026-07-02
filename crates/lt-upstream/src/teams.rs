//! Team list reads (the new-issue modal's team picker).

use anyhow::Result;
use lt_types::teams::{TeamsQuery, query};
use lt_types::types::Team;
use serde_json::Value;

use crate::client::{GraphqlTransport, query_as};

/// List the teams the viewer can file issues against.
pub fn fetch(transport: &dyn GraphqlTransport) -> Result<Vec<Team>> {
    let data: TeamsQuery = query_as(transport, &query(), Value::Null)?;
    Ok(data.teams.nodes)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::client::FakeTransport;

    #[test]
    fn fetch_extracts_nodes() {
        let transport = FakeTransport::new(vec![json!({
            "teams": { "nodes": [{ "id": "t1", "name": "Eng" }, { "id": "t2", "name": "Design" }] }
        })]);
        let teams = fetch(&transport).unwrap();
        assert_eq!(
            teams.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["Eng", "Design"]
        );
    }
}
