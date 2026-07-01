//! Team list reads (the new-issue modal's team picker).

use anyhow::Result;
use lt_types::types::Team;
use serde::Deserialize;
use serde_json::json;

use crate::client::{GraphqlTransport, query_as};

const TEAMS_QUERY: &str = r"
query Teams {
  teams {
    nodes {
      id
      name
    }
  }
}
";

#[derive(Deserialize)]
struct TeamConnection {
    nodes: Vec<Team>,
}

#[derive(Deserialize)]
struct TeamsData {
    teams: TeamConnection,
}

/// List the teams the viewer can file issues against.
pub fn fetch(transport: &dyn GraphqlTransport) -> Result<Vec<Team>> {
    let data: TeamsData = query_as(transport, TEAMS_QUERY, json!({}))?;
    Ok(data.teams.nodes)
}

#[cfg(test)]
mod tests {
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
