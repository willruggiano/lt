//! The vocabulary the [`crate::Runtime`] reports to its consumer (the TUI).
//!
//! `StateEvent` and `Scope` (docs/design/tui-app-event-queue-adr.md) are
//! superseded: invalidation is derived from `EntityKey`/`Read::reads`
//! (docs/design/operation-seam-adr.md, "Decision 5") rather than hand-placed,
//! so the queue only ever needs to say *which subscription* changed.

use lt_types::viewer;

use crate::subscription::SubId;

/// Everything the runtime reports, delivered through the [`OnEvent`] callback
/// the runtime is constructed with.
pub enum RuntimeEvent {
    /// A live subscription's slot has a fresh result; the view holding it
    /// should `take` and re-apply its ui-state policy.
    Updated(SubId),
    /// Sync-cycle progress and outcome -- scheduling and error text, not an
    /// invalidation.
    Sync(SyncEvent),
    /// Login outcome: identity or error text.
    Login(LoginEvent),
}

/// Sync-cycle progress and outcome, reported by the loop.
pub enum SyncEvent {
    /// A sync cycle began. The TUI can no longer infer "in flight" from its
    /// own spawn, so the producer announces it.
    Started,
    /// Sync succeeded; carries a freshly-fetched identity when the loop
    /// decided one was needed (see `Runtime::run`).
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

/// Invoked once per event, from the runtime's threads.
pub type OnEvent = Box<dyn Fn(RuntimeEvent) + Send + Sync + 'static>;

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
