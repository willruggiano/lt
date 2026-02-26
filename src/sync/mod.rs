pub mod delta;
pub mod full;
mod probe;

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

pub fn run(cmd: SyncCommands) -> Result<()> {
    match cmd {
        SyncCommands::Delta => delta::run(),
        SyncCommands::Full => full::run(),
        SyncCommands::Probe { token } => probe::run(token),
    }
}
