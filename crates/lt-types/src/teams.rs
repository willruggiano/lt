//! The team list query (the new-issue modal's team picker), modelled as a
//! cynic `QueryFragment`. The fetch lives in `lt-upstream`.

use cynic::QueryBuilder;

use crate::schema;
use crate::types::Team;

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query")]
pub struct TeamsQuery {
    pub teams: TeamConnection,
}

/// The built teams query string.
#[must_use]
pub fn query() -> String {
    TeamsQuery::build(()).query
}

#[derive(cynic::QueryFragment)]
pub struct TeamConnection {
    pub nodes: Vec<Team>,
}

#[cfg(test)]
mod tests {
    use super::query;

    #[test]
    fn query_selects_team_nodes() {
        let built = query();
        assert!(built.contains("teams"));
        assert!(built.contains("nodes"));
    }
}
