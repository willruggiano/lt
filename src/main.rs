mod auth;
mod config;
mod issues;
mod linear;
mod sync;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "lt", about = "Linear TUI for terminal power users", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage Linear authentication
    Auth {
        #[command(subcommand)]
        command: auth::AuthCommands,
    },
    /// List Linear issues
    Issues {
        #[command(flatten)]
        args: issues::IssueArgs,
    },
    /// Interactive TUI for browsing issues
    Tui {
        #[command(flatten)]
        args: issues::IssueArgs,
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
        None => tui::run(issues::IssueArgs::default())?,
        Some(Commands::Auth { command }) => auth::run(command)?,
        Some(Commands::Issues { args }) => issues::run(args)?,
        Some(Commands::Tui { args }) => tui::run(args)?,
        Some(Commands::Sync { command }) => sync::run(command)?,
    }
    Ok(())
}
