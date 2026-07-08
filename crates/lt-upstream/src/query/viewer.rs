//! The viewer (authenticated-user) query, modelled as cynic `QueryFragment`s.
//! These are the shared "currency" types; the fetch lives in `lt-upstream`.

use cynic::QueryBuilder;
use linear_schema::linear as schema;

use super::graphql::GraphqlOperation;
use super::types;

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query")]
pub struct ViewerQuery {
    viewer: ViewerEnvelope,
}

impl GraphqlOperation for ViewerQuery {
    type Variables = ();
    type Output = Option<Viewer>;
    const NAME: &'static str = "viewer";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }
}

impl TryFrom<ViewerQuery> for Option<Viewer> {
    type Error = anyhow::Error;

    fn try_from(op: ViewerQuery) -> anyhow::Result<Self> {
        Ok(Some(Viewer {
            user: types::User {
                id: op.viewer.id,
                name: op.viewer.name,
            },
            organization: op.viewer.organization,
        }))
    }
}

/// The wire selection for `Query.viewer`: the shared [`types::User`] fields
/// plus `organization`. Private -- callers see only [`Viewer`], composed from
/// it by `impl TryFrom<ViewerQuery> for Option<Viewer>`.
#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "User")]
struct ViewerEnvelope {
    id: cynic::Id,
    name: String,
    organization: Organization,
}

/// The authenticated user's identity: the shared [`types::User`] fragment
/// plus the organization the viewer query alone selects.
#[derive(Debug, Clone)]
pub struct Viewer {
    pub user: types::User,
    pub organization: Organization,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Organization")]
pub struct Organization {
    pub id: cynic::Id,
    pub name: String,
    #[cynic(rename = "urlKey")]
    pub url_key: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recompose_maps_nested_fields() {
        let data = serde_json::json!({
            "viewer": {
                "id": "u1",
                "name": "Ada",
                "organization": { "id": "o1", "name": "Acme", "urlKey": "acme" }
            }
        });
        let viewer: Option<Viewer> = serde_json::from_value::<ViewerQuery>(data)
            .unwrap()
            .try_into()
            .unwrap();
        let viewer = viewer.unwrap();
        assert_eq!(viewer.user.id.inner(), "u1");
        assert_eq!(viewer.user.name, "Ada");
        assert_eq!(viewer.organization.id.inner(), "o1");
        assert_eq!(viewer.organization.name, "Acme");
        assert_eq!(viewer.organization.url_key, "acme");
    }
}
