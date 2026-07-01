//! Custom GraphQL scalars, modelled as newtypes so cynic can decode them
//! directly (see the ADR: `docs/design/linear-api-types-codegen.md`).

use crate::schema;

/// An ISO-8601 timestamp as returned by the Linear API.
#[derive(cynic::Scalar, Debug, Clone, PartialEq)]
pub struct DateTime(pub String);
