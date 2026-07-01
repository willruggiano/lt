pub mod comments;
pub mod graphql;
pub mod inputs;
pub mod issues;
pub mod members;
pub mod notifications;
pub mod pagination;
pub mod query;
pub mod scalars;
pub mod states;
pub mod teams;
pub mod types;
pub mod viewer;

// Re-exported so downstream crates (`lt-storage`, `lt-runtime`, ...) can
// construct/read the `id` fields on the fragment types in `types` without
// taking a direct `cynic` dependency -- cynic itself stays confined to
// `lt-types`.
pub use cynic::Id;

#[cynic::schema("linear")]
pub mod schema {}
