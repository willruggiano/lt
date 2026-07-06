//! The `lt sync` command surface. The sync engine lives in `lt-runtime`; this
//! is only the clap dispatch.

use std::io::Write;

use anyhow::Result;
use clap::Subcommand;
use lt_runtime::sync;

#[derive(Subcommand)]
pub enum SyncCommands {
    /// Incremental sync: fetch issues updated since last sync (default)
    Delta,
    /// Fetch all issues from Linear and store them in the local SQLite cache
    Full,
    /// Probe the sync API to test whether a token is accepted
    Probe {
        /// Token to test instead of the stored OAuth token (e.g. a personal API key)
        #[arg(long)]
        token: Option<String>,
    },
}

pub fn run(out: &mut dyn Write, cmd: SyncCommands, runtime: &lt_runtime::Runtime) -> Result<()> {
    match cmd {
        SyncCommands::Delta => {
            runtime.sync_delta()?;
            writeln!(out, "Sync complete.")?;
            Ok(())
        }
        SyncCommands::Full => {
            runtime.sync_full()?;
            writeln!(out, "Sync complete.")?;
            Ok(())
        }
        SyncCommands::Probe { token } => sync::probe::run(out, token),
    }
}
