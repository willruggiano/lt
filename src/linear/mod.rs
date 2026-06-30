pub mod client;
pub mod mutations;
pub mod notifications;
pub mod types;
pub mod viewer;

/// cynic schema module, expanded from the committed Linear schema snapshot
/// registered in `build.rs`. Provides the marker types the `QueryFragment`
/// derives check selection sets against.
#[cynic::schema("linear")]
pub mod schema {}
