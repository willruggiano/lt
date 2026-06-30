//! Fetch the authenticated user's identity (viewer) from the Linear API.

use anyhow::Result;
use cynic::QueryBuilder;

use super::client::{GraphqlTransport, query_as};
use super::schema;

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query")]
struct ViewerQuery {
    viewer: ViewerUser,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "User")]
struct ViewerUser {
    id: cynic::Id,
    name: String,
    organization: ViewerOrganization,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Organization")]
struct ViewerOrganization {
    name: String,
}

/// The authenticated user's identity.
pub struct Viewer {
    pub id: String,
    pub name: String,
    /// Linear organization (workspace) name.
    pub org_name: String,
}

pub fn fetch_viewer(transport: &dyn GraphqlTransport) -> Result<Viewer> {
    let operation = ViewerQuery::build(());
    let variables = serde_json::to_value(operation.variables)?;

    let data: ViewerQuery = query_as(transport, &operation.query, variables)?;
    Ok(Viewer {
        id: data.viewer.id.into_inner(),
        name: data.viewer.name,
        org_name: data.viewer.organization.name,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::linear::client::FakeTransport;

    #[test]
    fn fetch_viewer_maps_nested_fields() {
        let transport = FakeTransport::new(vec![json!({
            "viewer": { "id": "u1", "name": "Ada", "organization": { "name": "Acme" } }
        })]);
        let viewer = fetch_viewer(&transport).unwrap();
        assert_eq!(viewer.id, "u1");
        assert_eq!(viewer.name, "Ada");
        assert_eq!(viewer.org_name, "Acme");
    }
}
