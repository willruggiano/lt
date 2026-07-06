//! `lt-upstream`: the Linear API edge. Every network call to Linear originates
//! here; the rest of the workspace reaches it through `lt-cli`'s adapter.

pub mod auth;
pub mod client;
pub mod comments;
pub mod issues;
