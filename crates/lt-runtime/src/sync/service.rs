//! The vocabulary the [`crate::Runtime`] reports to its consumer (the TUI).
//!
//! Invalidation is derived from `EntityKey`/`Read::reads`
//! (docs/design/operation-seam-adr.md, "Decision 5"), so the queue only ever
//! needs to say *which subscription* changed.

use lt_types::viewer;

use crate::subscription::SubscriptionKey;

/// Everything the runtime reports, delivered through the [`OnEvent`] callback
/// the runtime is constructed with.
pub enum RuntimeEvent {
    /// A live subscription's slot has a fresh result; the view holding it
    /// should `take` and re-apply its ui-state policy.
    Updated(SubscriptionKey),
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
    /// Sync succeeded, stamped with `last_synced_at` (the runtime's own
    /// `sync_meta` read), or `None` if that read finds no prior sync. The
    /// viewer identity is not carried here: a sync cycle persists it through
    /// the same `Upsert` seam as everything else it touches, so a live
    /// `ViewerQuery` subscription picks it up through propagation instead.
    Done(Option<chrono::DateTime<chrono::Utc>>),
    Error(String),
    NotAuthenticated,
}

/// Login-cycle outcome, reported by the login worker.
pub enum LoginEvent {
    /// Login succeeded. `viewer` is not optional: either you log in as a
    /// user or you don't -- a post-login identity-fetch failure is `Error`.
    Success {
        viewer: viewer::Viewer,
    },
    Error(String),
}

/// Invoked once per event, from the runtime's threads.
pub type OnEvent = Box<dyn Fn(RuntimeEvent) + Send + Sync + 'static>;
