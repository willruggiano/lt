mod auth;
mod config;
mod db;
mod inbox;
mod issues;
mod linear;
mod logging;
mod output;
mod search;
mod sync;
mod text;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "lt", about = "Linear TUI for terminal power users", version)]
struct Cli {
    /// Profile to use: each profile has its own credentials and local
    /// database (one account/workspace per profile). Defaults to $`LT_PROFILE`
    /// or "default".
    #[arg(long, global = true)]
    profile: Option<String>,

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
    /// Search the local SQLite FTS5 index for issues
    Search {
        #[command(flatten)]
        args: search::SearchArgs,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Select the profile before anything touches auth, logs, or the DB.
    let profile = cli
        .profile
        .clone()
        .or_else(|| std::env::var("LT_PROFILE").ok().filter(|s| !s.is_empty()));
    config::set_profile(profile)?;

    // Determine whether we are entering TUI mode so we can choose the right
    // logging subscriber before any other code runs.
    let is_tui = matches!(cli.command, None | Some(Commands::Tui { .. }));

    // Keep the guard alive for the duration of main() so the background
    // logging thread is not torn down prematurely.
    let _log_guard = if is_tui {
        logging::init_tui()?
    } else {
        logging::init_cli()?
    };

    let mut out = output::Output::stdout();

    match cli.command {
        None => tui::run(issues::IssueArgs::default())?,
        Some(Commands::Auth { command }) => auth::run(&mut out, &command)?,
        Some(Commands::Inbox { args }) => inbox::run(&mut out, &args)?,
        Some(Commands::Issues { args, subcommand }) => issues::run(&mut out, args, subcommand)?,
        Some(Commands::Tui { args }) => tui::run(args)?,
        Some(Commands::Sync { command }) => {
            let cmd = command.unwrap_or(sync::SyncCommands::Delta);
            sync::run(&mut out, cmd)?;
        }
        Some(Commands::Search { args }) => search::run(&mut out, &args)?,
    }
    Ok(())
}
