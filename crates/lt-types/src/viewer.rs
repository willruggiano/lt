//! The viewer (authenticated-user) query, modelled as cynic `QueryFragment`s.
//! These are the shared "currency" types; the fetch lives in `lt-sync`.

use crate::schema;

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query")]
pub struct ViewerQuery {
    pub viewer: ViewerUser,
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
