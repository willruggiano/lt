mod full;
mod probe;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum SyncCommands {
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
        SyncCommands::Full => full::run(),
        SyncCommands::Probe { token } => probe::run(token),
    }
}
