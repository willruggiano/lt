//! The viewer (authenticated-user) query, modelled as cynic `QueryFragment`s.
//! These are the shared "currency" types; the fetch lives in `lt-upstream`.

use cynic::QueryBuilder;

use crate::graphql::GraphqlOperation;
use crate::schema;

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query")]
pub struct ViewerQuery {
    pub viewer: User,
}

impl GraphqlOperation for ViewerQuery {
    type Variables = ();
    /// `Option`-shaped so the local (cache) read can honestly report "no
    /// viewer persisted yet" (pre-first-sync) without an empty-string
    /// sentinel; the wire side (`Query.viewer` is non-null) always extracts
    /// `Some`.
    type Output = Option<User>;
    const NAME: &'static str = "viewer";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }

    fn extract(self) -> anyhow::Result<Self::Output> {
        Ok(Some(self.viewer))
    }
}

/// The authenticated user's identity, as selected by the viewer query. A
/// distinct selection from [`crate::types::User`] (adds `organization`, drops
/// nothing an assignee/creator would need) is why this fragment exists
/// alongside it, disambiguated by module path.
#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "User")]
pub struct User {
    pub id: cynic::Id,
    pub name: String,
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

impl From<User> for crate::types::User {
    /// Narrows the viewer identity to the shared entity fragment (drops
    /// `organization`), for display contexts that need only `{id, name}`
    /// (e.g. a locally authored comment's author).
    fn from(user: User) -> Self {
        Self {
            id: user.id,
            name: user.name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_maps_nested_fields() {
        let data = serde_json::json!({
            "viewer": {
                "id": "u1",
                "name": "Ada",
                "organization": { "id": "o1", "name": "Acme", "urlKey": "acme" }
            }
        });
        let viewer = serde_json::from_value::<ViewerQuery>(data)
            .unwrap()
            .extract()
            .unwrap()
            .unwrap();
        assert_eq!(viewer.id.inner(), "u1");
        assert_eq!(viewer.name, "Ada");
        assert_eq!(viewer.organization.id.inner(), "o1");
        assert_eq!(viewer.organization.name, "Acme");
        assert_eq!(viewer.organization.url_key, "acme");
    }
}
