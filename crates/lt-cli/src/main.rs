mod auth;
mod inbox;
mod issues;
mod logging;
mod output;
mod search;
#[cfg(feature = "sim")]
mod sim;
mod sync;

use std::sync::{Arc, mpsc};

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
    /// Generate a deterministic fake dataset into the local DB (no Linear account needed)
    #[cfg(feature = "sim")]
    Sim {
        #[command(flatten)]
        args: sim::SimArgs,
    },
}

/// Build the `lt-runtime`-backed `Runtime` against the profile's local
/// database and the production HTTP transport, with the given event
/// callback. The sole place `lt-cli` names `Database`/`HttpTransportSource`.
fn build_runtime(on_event: lt_runtime::sync::service::OnEvent) -> lt_runtime::Runtime {
    lt_runtime::Runtime::new(
        lt_runtime::db::Database::File,
        Box::new(lt_runtime::HttpTransportSource),
        on_event,
    )
}

/// Launch the TUI with the `lt-runtime`-backed `Runtime` injected.
///
/// `lt-cli` owns both ends of the `AppEvent` channel: the sender feeds both
/// the TUI's input thread and the runtime's `OnEvent` callback, so a
/// same-thread write and a background sync/login outcome land on the same
/// queue; the receiver drives `lt_tui::run`'s loop. The runtime's blocking
/// `run` loop is spawned on a detached, process-lifetime background thread
/// before the TUI starts.
fn run_tui(
    filter: &lt_types::issues::IssueFilter,
    sort: &lt_runtime::query::SortField,
    direction: lt_runtime::query::SortDirection,
    limit: u32,
) -> Result<()> {
    let launch = lt_tui::ListLaunch {
        filter: lt_runtime::search_query::args_to_ast(filter, sort, direction),
        limit,
    };

    let (tx, rx) = mpsc::channel();
    let on_event_tx = tx.clone();
    let on_event: lt_runtime::sync::service::OnEvent = Box::new(move |ev| {
        if on_event_tx.send(lt_tui::AppEvent::Runtime(ev)).is_err() {
            tracing::debug!("runtime event: TUI is gone");
        }
    });
    let runtime = Arc::new(build_runtime(on_event));
    let sync_runtime = Arc::clone(&runtime);
    std::thread::spawn(move || sync_runtime.run());
    lt_tui::run(launch, runtime, tx, rx)
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Select the profile before anything touches auth, logs, or the DB.
    let profile = cli
        .profile
        .clone()
        .or_else(|| std::env::var("LT_PROFILE").ok().filter(|s| !s.is_empty()));
    lt_config::set_profile(profile)?;

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
        None => run_tui(
            &lt_types::issues::IssueFilter::default(),
            &lt_runtime::query::SortField::Updated,
            lt_runtime::query::SortDirection::Descending,
            50,
        )?,
        Some(Commands::Auth { command }) => auth::run(&mut out, &command)?,
        Some(Commands::Inbox { args }) => inbox::run(&mut out, &args)?,
        Some(Commands::Issues { args, subcommand }) => {
            let runtime = build_runtime(Box::new(|_| {}));
            issues::run(&mut out, &args, subcommand, &runtime)?;
        }
        Some(Commands::Tui { args }) => {
            run_tui(
                &args.literal_filter()?,
                &args.sort,
                args.sort_direction(),
                args.limit,
            )?;
        }
        Some(Commands::Sync { command }) => {
            let runtime = build_runtime(Box::new(|_| {}));
            let cmd = command.unwrap_or(sync::SyncCommands::Delta);
            sync::run(&mut out, cmd, &runtime)?;
        }
        Some(Commands::Search { args }) => {
            let runtime = build_runtime(Box::new(|_| {}));
            search::run(&mut out, &args, &runtime)?;
        }
        #[cfg(feature = "sim")]
        Some(Commands::Sim { args }) => {
            let runtime = build_runtime(Box::new(|_| {}));
            sim::run(&mut out, &args, &runtime)?;
        }
    }
    Ok(())
}
