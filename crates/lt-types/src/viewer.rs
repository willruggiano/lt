//! The viewer (authenticated-user) query, modelled as cynic `QueryFragment`s.
//! These are the shared "currency" types; the fetch lives in `lt-upstream`.

use cynic::QueryBuilder;

use crate::schema;

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query")]
pub struct ViewerQuery {
    pub viewer: ViewerUser,
}

/// The built viewer query string. Kept here so cynic stays confined to
/// `lt-types`: the sync layer sends this string and deserializes the response
/// back into [`ViewerQuery`] via serde, without depending on cynic itself.
#[must_use]
pub fn query() -> String {
    ViewerQuery::build(()).query
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "User")]
pub struct ViewerUser {
    pub id: cynic::Id,
    pub name: String,
    pub organization: ViewerOrganization,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Organization")]
pub struct ViewerOrganization {
    pub name: String,
}
