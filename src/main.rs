mod auth;
mod config;
mod db;
mod inbox;
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
    /// List Linear issues or manage issues
    Issues {
        #[command(flatten)]
        args: issues::IssueArgs,
        #[command(subcommand)]
        subcommand: Option<issues::IssueSubcommand>,
    },
    /// Interactive TUI for browsing issues
    Tui {
        #[command(flatten)]
        args: issues::IssueArgs,
    },
    /// Show Linear notification inbox
    Inbox {
        #[command(flatten)]
        args: inbox::InboxArgs,
    },
    /// Sync issues from Linear (incremental by default; use 'full' subcommand for a full sync)
    Sync {
        #[command(subcommand)]
        command: Option<sync::SyncCommands>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        None => tui::run(issues::IssueArgs::default())?,
        Some(Commands::Auth { command }) => auth::run(command)?,
        Some(Commands::Inbox { args }) => inbox::run(args)?,
        Some(Commands::Issues { args, subcommand }) => issues::run(args, subcommand)?,
        Some(Commands::Tui { args }) => tui::run(args)?,
        Some(Commands::Sync { command }) => {
            let cmd = command.unwrap_or(sync::SyncCommands::Delta);
            sync::run(cmd)?;
        }
    }
    Ok(())
}
