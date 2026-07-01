//! The seam between the TUI read model and the sync layer's API edge.
//!
//! Defined in the lowest crate both `lt-tui` and `lt-cli` share so the TUI can
//! drive sync/login and live modal reads through a trait object with no
//! compile-time dependency on `lt-sync` or `cynic`. `lt-cli` provides the
//! concrete adapter backed by `lt-sync`.

use std::sync::mpsc::Receiver;

use anyhow::Result;

use crate::query::IssueQuery;

/// The viewer's identity, surfaced in the TUI header and the "Me" assignee item.
pub struct ViewerIdentity {
    pub id: String,
    pub name: String,
    pub org_name: String,
}

/// A team the new-issue modal can target. `lt-sync` decodes API responses
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

/// Outcome of a background sync, delivered to the TUI event loop.
pub enum SyncEvent {
    /// Sync succeeded; carries a freshly-fetched identity when one was requested.
    Done(Option<ViewerIdentity>),
    Error(String),
    NotAuthenticated,
}

/// Outcome of a background login, delivered to the TUI event loop.
pub enum LoginEvent {
    Success {
        viewer_name: Option<String>,
        org_name: Option<String>,
    },
    Error(String),
}

/// The sync/API operations the TUI drives, abstracted away from `lt-sync`.
///
/// The concrete implementation lives in `lt-cli` and is the only code that
/// touches `HttpTransport`/cynic; the TUI holds it behind this trait so an API
/// call from the render/event path does not compile.
pub trait SyncService: Send + Sync {
    /// Spawn a background sync (full or delta); the receiver yields one
    /// [`SyncEvent`] when it completes.
    fn spawn_sync(
        &self,
        query: IssueQuery,
        full: bool,
        fetch_identity: bool,
    ) -> Receiver<SyncEvent>;

    /// Spawn the background OAuth login flow.
    fn spawn_login(&self) -> Receiver<LoginEvent>;

    /// Fetch the viewer identity (best-effort; `None` when unauthenticated).
    fn fetch_viewer(&self) -> Option<ViewerIdentity>;

    /// List the teams the viewer can file issues against.
    fn fetch_teams(&self) -> Result<Vec<Team>>;

    /// List a team's workflow states.
    fn fetch_workflow_states(&self, team_id: &str) -> Result<Vec<WorkflowState>>;

    /// List a team's members.
    fn fetch_team_members(&self, team_id: &str) -> Result<Vec<Member>>;

    /// Sync an issue's comments from the API into the local database.
    fn sync_comments(&self, issue_id: &str) -> Result<()>;
}
