//! The seam between the TUI read model and the sync layer's API edge.
//!
//! Defined in `lt-runtime` -- the crate both `lt-tui` and `lt-cli` share -- so
//! the TUI can drive sync/login and live modal reads through a trait object
//! without a compile-time dependency on `lt-upstream` or `cynic`. The concrete
//! adapter ([`crate::LinearSyncService`]) is the only code that touches the
//! API edge.

use anyhow::Result;
use lt_types::inputs::{CommentCreateInput, IssueCreateInput};
use lt_types::viewer;

/// Everything the runtime reports, delivered through the [`OnEvent`] callback
/// the service is constructed with.
pub enum RuntimeEvent {
    /// The named slice of local state changed; re-read it if displayed.
    State(StateEvent),
    /// Sync-cycle progress and outcome -- scheduling and error text, not an
    /// invalidation.
    Sync(SyncEvent),
    /// Login outcome: identity or error text.
    Login(LoginEvent),
}

/// A payload-free invalidation. Variants carry only the scope id a consumer
/// needs to decide relevance and which query to re-run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateEvent {
    /// The issues read model changed (a write, or a sync upsert).
    Issues,
    /// One issue's comment thread changed.
    Comments { issue_id: String },
    /// The team list changed.
    Teams,
    /// One team's workflow states and memberships changed.
    Team { team_id: String },
}

/// Sync-cycle progress and outcome, reported by the loop.
pub enum SyncEvent {
    /// A sync cycle began. The TUI can no longer infer "in flight" from its
    /// own spawn, so the producer announces it.
    Started,
    /// Sync succeeded; carries a freshly-fetched identity when the loop
    /// decided one was needed (see `LinearSyncService::run`).
    Done(Option<viewer::User>),
    Error(String),
    NotAuthenticated,
}

/// Login-cycle outcome, reported by the login worker.
pub enum LoginEvent {
    /// Login succeeded. `viewer` is not optional: either you log in as a
    /// user or you don't -- a post-login identity-fetch failure is `Error`.
    Success {
        viewer: viewer::User,
    },
    Error(String),
}

/// Invoked once per event, from the service's threads.
pub type OnEvent = Box<dyn Fn(RuntimeEvent) + Send + Sync + 'static>;

/// A freshness interest. `StateEvent` minus `Issues`: the issue list's
/// freshness is the loop's own baseline cadence, not an interest a view
/// declares.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Comments { issue_id: String },
    Teams,
    Team { team_id: String },
}

/// One issue-field edit, mirroring the outbox commands
/// (`lt-storage/src/db/outbox.rs`).
pub enum IssueEdit {
    State {
        id: String,
        name: String,
    },
    Priority(u8),
    /// `(id, name)`; `None` clears the assignee.
    Assignee(Option<(String, String)>),
}

/// The sync/API operations the TUI drives, abstracted away from
/// `lt-upstream`.
///
/// The concrete implementation lives in `lt-runtime` and is the only code
/// that touches `HttpTransport`/cynic; the TUI holds it behind this trait so
/// an API call from the render/event path does not compile.
pub trait SyncService: Send + Sync {
    /// The sync loop: blocks for the life of the process. `lt-cli` spawns it
    /// on a detached background thread before the TUI starts. Owns all
    /// scheduling: the startup sync, the 30s delta cadence, prompt and
    /// periodic refreshes of watched scopes, and full syncs on request.
    fn run(&self);

    /// Declare/retract interest in a scope's freshness.
    fn watch(&self, scope: Scope);
    fn unwatch(&self, scope: Scope);

    /// User-initiated commands -- deliberate acts, distinct from data-driven
    /// scheduling: `request_sync` nudges the loop into an immediate full
    /// sync (the `r` key); `login` runs the OAuth flow (the `L` key).
    fn request_sync(&self);
    fn login(&self);

    /// Startup header identity, before the loop's first `Sync(Done)`.
    fn fetch_viewer(&self) -> Option<viewer::User>;

    /// Writes: transactional local enqueue, then the matching `State` event
    /// emitted through the callback.
    fn create_comment(&self, input: &CommentCreateInput) -> Result<()>;
    fn edit_issue(&self, issue_id: &str, edit: IssueEdit) -> Result<()>;
    /// Returns the optimistic identifier so the caller can seek to it.
    fn create_issue(&self, input: &IssueCreateInput) -> Result<String>;
}
