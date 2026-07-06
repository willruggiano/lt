//! `lt-runtime`: the composition layer between the local store (`lt-storage`)
//! and the Linear API edge (`lt-upstream`). It owns the sync engine, the
//! concrete [`Runtime`] (subscriptions, entity-keyed propagation, writes,
//! sync/login scheduling), the generic [`load`]/[`refresh`] operation
//! drivers, and the CLI command orchestration, and re-exports the store
//! read/write facade so `lt-tui`/`lt-cli` depend on this crate alone rather
//! than reaching across the seam.

pub mod ops;
pub mod subscription;
pub mod sync;

mod runtime;
pub use ops::{load, refresh};
#[cfg(feature = "sim")]
pub use runtime::SimSeed;
pub use runtime::{HttpTransportSource, Runtime, SearchOutcome, TransportSource};
pub use subscription::{Subscription, SubscriptionKey};

// Command orchestration for the CLI.
pub mod auth;
pub mod issues;
pub mod notifications;

// Store read/write facade re-exported so downstream crates name one seam.
#[cfg(feature = "sim")]
pub use lt_storage::sim;
pub use lt_storage::{db, search_query, text};
pub use lt_types::clock::Clock;
pub use lt_types::query;

/// Test-only seam for constructing/seeding an in-memory database without
/// naming `lt_runtime::db` (docs/design/operation-seam-adr.md, "Decision 4":
/// the TUI holds no `Database`/`Connection`). `lt-tui`'s tests route their
/// fixture setup through this module instead of the `db` re-export above.
#[cfg(any(test, feature = "test-util"))]
pub mod test_util {
    pub use lt_storage::db::outbox::sample_base_issue;
    pub use lt_storage::db::{Connection, Database, set_viewer, upsert_issues, upsert_team_state};
}
