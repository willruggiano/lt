use std::io::Write;

use anyhow::{Result, bail};
use clap::Args;
use lt_runtime::{Runtime, SearchOutcome};

use crate::issues::display::print_table;

#[derive(Args, Clone)]
pub struct SearchArgs {
    /// Search query (FTS5 syntax: prefix `auth*`, phrase `"oauth token"`, AND of terms)
    pub query: String,

    /// Maximum number of results to return
    #[arg(long, default_value = "20")]
    pub limit: usize,

    /// Bypass local index and use Linear API search (not yet implemented)
    #[arg(long)]
    pub live: bool,
}

pub fn run(out: &mut dyn Write, args: &SearchArgs, runtime: &Runtime) -> Result<()> {
    if args.live {
        bail!("--live search via Linear API is not yet implemented");
    }

    match runtime.search(&args.query, args.limit)? {
        SearchOutcome::NoIndex => bail!("Run 'lt sync' to build the local index first."),
        SearchOutcome::Results {
            issues,
            approximate,
        } => {
            let note = if approximate {
                "Note: FTS index is empty or stale. Run 'lt sync full' to rebuild it. \
                 Showing approximate results from title search."
                    .to_string()
            } else {
                String::new()
            };
            print_table(out, &issues, &note)?;
        }
    }
    Ok(())
}
