mod auth;
mod config;
mod sync;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "lt",
    about = "Linear TUI for terminal power users",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage Linear authentication
    Auth {
        #[command(subcommand)]
        command: auth::AuthCommands,
    },
    /// Sync API diagnostics
    Sync {
        #[command(subcommand)]
        command: sync::SyncCommands,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Auth { command } => auth::run(command)?,
        Commands::Sync { command } => sync::run(command)?,
    }
    Ok(())
}
