//! The seam between the TUI read model and the sync layer's API edge.
//!
//! Defined in `lt-runtime` — the crate both `lt-tui` and `lt-cli` share — so the
//! TUI can drive sync/login and live modal reads through a trait object without
//! a compile-time dependency on `lt-upstream` or `cynic`. The concrete adapter
//! ([`crate::LinearSyncService`]) is the only code that touches the API edge.

use anyhow::Result;
use lt_types::viewer;

/// Outcome of a background sync, delivered to the TUI event loop.
pub enum SyncEvent {
    /// Sync succeeded; carries a freshly-fetched identity when one was requested.
    Done(Option<viewer::User>),
    Error(String),
    NotAuthenticated,
}

/// Outcome of a background login, delivered to the TUI event loop.
pub enum LoginEvent {
    /// Login succeeded; carries a freshly-fetched identity when the fetch
    /// itself succeeded.
    Success(Option<viewer::User>),
    Error(String),
}

/// Invoked exactly once with the outcome of a spawned background job.
pub type OnSync = Box<dyn FnOnce(SyncEvent) + Send + 'static>;
pub type OnLogin = Box<dyn FnOnce(LoginEvent) + Send + 'static>;

/// The sync/API operations the TUI drives, abstracted away from `lt-upstream`.
///
/// The concrete implementation lives in `lt-runtime` and is the only code that
/// touches `HttpTransport`/cynic; the TUI holds it behind this trait so an API
/// call from the render/event path does not compile.
pub trait SyncService: Send + Sync {
    /// Spawn a background sync (full or delta); `on_done` is invoked exactly
    /// once with the outcome, even if the sync body panics.
    fn spawn_sync(&self, full: bool, fetch_identity: bool, on_done: OnSync);

    /// Spawn the background OAuth login flow; same completion contract.
    fn spawn_login(&self, on_done: OnLogin);

    /// Fetch the viewer identity (best-effort; `None` when unauthenticated).
    fn fetch_viewer(&self) -> Option<viewer::User>;

    /// Sync an issue's comments from the API into the local database.
    fn sync_comments(&self, issue_id: &str) -> Result<()>;

    /// Sync the team list from the API into the local database.
    fn sync_teams(&self) -> Result<()>;

    /// Sync one team's workflow states and memberships from the API into the
    /// local database.
    fn sync_team_data(&self, team_id: &str) -> Result<()>;
}
