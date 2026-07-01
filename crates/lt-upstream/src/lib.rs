//! `lt-upstream`: the Linear API edge. Every network call to Linear originates
//! here; the rest of the workspace reaches it through `lt-cli`'s adapter.

pub mod auth;
pub mod client;
pub mod comments;
mod graphql;
pub mod issues;
pub mod members;
pub mod notifications;
pub mod states;
pub mod sync;
pub mod teams;
pub mod viewer;
