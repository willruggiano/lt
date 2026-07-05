//! `lt-runtime`: the composition layer between the local store (`lt-storage`)
//! and the Linear API edge (`lt-upstream`). It owns the sync engine, the
//! [`SyncService`](sync::service::SyncService) port and its [`LinearSyncService`]
//! adapter, the generic [`load`]/[`refresh`] operation drivers, and the CLI
//! command orchestration, and re-exports the store read/write facade so
//! `lt-tui`/`lt-cli` depend on this crate alone rather than reaching across
//! the seam.

pub mod ops;
pub mod sync;

mod adapter;
pub use adapter::LinearSyncService;
pub use ops::{load, refresh};

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
