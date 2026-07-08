//! `lt-upstream`: the Linear API edge. Every network call to Linear originates
//! here; the rest of the workspace reaches it through `lt-cli`'s adapter.

pub mod auth;
pub mod comments;
pub mod issues;
pub mod query;
pub mod transport;

// Re-exported so downstream crates (`lt-storage`, `lt-runtime`, ...) can
// construct/read the `id` fields on the fragment types in `query::types`
// without taking a direct `cynic` dependency -- cynic itself stays confined
// to `lt-upstream`.
pub use cynic::Id;
