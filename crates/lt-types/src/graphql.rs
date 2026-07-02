//! Cross-operation query traits: shared shape rather than shared code, so
//! `lt-upstream`'s domain modules can decode a `team(id) { <conn> { nodes } }`
//! query through one generic function regardless of which connection field
//! the query selects.

use serde::de::DeserializeOwned;

/// A query whose response is `team(id) { <some connection> { nodes } }`.
/// `into_nodes` extracts the one field that differs between operations
/// (`states`, `members`, ...).
pub trait TeamScopedQuery: DeserializeOwned {
    type Node;
    fn into_nodes(self) -> Vec<Self::Node>;
}
