//! Render/pick DTOs the sync layer decodes API responses into.
//!
//! Kept separate from the cynic currency types in [`crate::types`] (which
//! already defines a `Team`) so the new-issue modal's picker types carry no
//! GraphQL-schema dependency and both crates that consume them (`lt-tui`,
//! `lt-cli`) can name them through the runtime seam.

/// The viewer's identity, surfaced in the TUI header and the "Me" assignee item.
pub struct ViewerIdentity {
    pub id: String,
    pub name: String,
    pub org_name: String,
}

/// A team the new-issue modal can target. The sync layer decodes API responses
/// directly into this shared type, so the adapter needs no mapping layer.
#[derive(serde::Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
}

/// A workflow state for the state picker. `type_` (the state category, e.g.
/// "unstarted") is used by the CLI's new-issue default; the TUI ignores it.
#[derive(serde::Deserialize)]
pub struct WorkflowState {
    pub id: String,
    pub name: String,
    #[serde(rename = "type", default)]
    pub type_: String,
}

/// A team member for the assignee picker.
#[derive(serde::Deserialize)]
pub struct Member {
    pub id: String,
    pub name: String,
}
