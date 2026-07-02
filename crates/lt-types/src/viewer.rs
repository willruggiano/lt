//! The viewer (authenticated-user) query, modelled as cynic `QueryFragment`s.
//! These are the shared "currency" types; the fetch lives in `lt-upstream`.

use cynic::QueryBuilder;

use crate::schema;

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query")]
pub struct ViewerQuery {
    pub viewer: User,
}

/// The built viewer query string. Kept here so cynic stays confined to
/// `lt-types`: the sync layer sends this string and deserializes the response
/// back into [`ViewerQuery`] via serde, without depending on cynic itself.
#[must_use]
pub fn query() -> String {
    ViewerQuery::build(()).query
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
    pub name: String,
    #[cynic(rename = "urlKey")]
    pub url_key: String,
}
