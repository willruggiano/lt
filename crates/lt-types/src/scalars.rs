//! Custom GraphQL scalars, modelled as newtypes so cynic can decode them
//! directly (see the ADR: `docs/design/linear-api-types-codegen.md`).

use crate::schema;

/// An ISO-8601 timestamp as returned by the Linear API, decoded straight into
/// `chrono` (rather than kept as a raw `String`) so every consumer gets a
/// real, comparable/formattable timestamp instead of re-parsing text.
#[derive(cynic::Scalar, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime(pub chrono::DateTime<chrono::Utc>);

/// An issue's `priority: Float!`, decoded straight into `u8` (Linear's
/// priority levels are always small non-negative integers: 0-4). `u8` cannot
/// implement cynic's `IsScalar` directly (orphan rule -- `f64`'s marker type
/// lives in the generated schema module), so this newtype closes the gap.
/// `#[serde(transparent)]` keeps the wire encoding identical to a bare `u8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Priority(pub u8);

impl cynic::schema::IsScalar<f64> for Priority {
    type SchemaType = f64;
}
