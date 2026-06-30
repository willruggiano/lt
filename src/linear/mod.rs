pub mod client;
pub mod inputs;
pub mod mutations;
pub mod notifications;
pub mod types;
pub mod viewer;

#[cynic::schema("linear")]
pub mod schema {}
