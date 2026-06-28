pub mod comments;
pub mod delta;
pub mod full;
mod probe;

use std::io::Write;

use anyhow::Result;
use clap::Subcommand;

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

pub fn run(out: &mut dyn Write, cmd: SyncCommands) -> Result<()> {
    match cmd {
        SyncCommands::Delta => {
            delta::run()?;
            writeln!(out, "Sync complete.")?;
            Ok(())
        }
        SyncCommands::Full => {
            full::run()?;
            writeln!(out, "Sync complete.")?;
            Ok(())
        }
        SyncCommands::Probe { token } => probe::run(out, token),
    }
}
