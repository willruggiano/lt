mod auth;
mod logging;

use std::io::Write;
use std::sync::{Arc, mpsc};

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "lt", about, version)]
struct Cli {
    /// Set the active Linear workspace
    #[arg(short, long, global = true, env = "LT_PROFILE")]
    workspace: Option<String>,

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
    /// Sync issues from Linear (incremental by default)
    Sync {
        /// Perform a full sync instead of an incremental sync
        #[arg(long)]
        full: bool,
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

    // Keep the guard alive for the duration of main() so the background
    // logging thread is not torn down prematurely.
    let _guard = logging::init(cli.command.is_some())?;

    lt_config::set_workspace(cli.workspace)?;

    match cli.command {
        None => run_tui(
            &lt_types::issues::IssueFilter::default(),
            &lt_runtime::query::SortField::Updated,
            lt_runtime::query::SortDirection::Descending,
            50,
        )?,
        Some(Commands::Auth { command }) => auth::run(&mut std::io::stdout(), &command)?,
        Some(Commands::Sync { full }) => {
            let runtime = build_runtime(Box::new(|_| {}));
            runtime.sync(full)?;
            writeln!(std::io::stdout(), "Sync complete.")?;
        }
    }

    Ok(())
}
