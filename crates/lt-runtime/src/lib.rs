//! `lt-runtime`: the composition layer between the local store (`lt-storage`)
//! and the Linear API edge (`lt-upstream`). It owns the sync engine, the
//! [`SyncService`](sync_port::SyncService) port and its [`LinearSyncService`]
//! adapter, comment persistence, and the CLI command orchestration, and
//! re-exports the store read/write facade so `lt-tui`/`lt-cli` depend on this
//! crate alone rather than reaching across the seam.

pub mod comments;
pub mod sync;
pub mod sync_port;

mod adapter;
pub use adapter::LinearSyncService;

// Command orchestration for the CLI.
pub mod auth;
pub mod issues;
pub mod notifications;

// Store read/write facade re-exported so downstream crates name one seam.
#[cfg(feature = "sim")]
pub use lt_storage::sim;
pub use lt_storage::{db, search_query, text};
pub use lt_types::query;
