//! The `lt sim` command: seed the local database from the deterministic
//! generator in `lt-storage`, via the injected `Runtime`.

use std::io::Write;

use anyhow::Result;
use clap::Args;
use lt_runtime::Runtime;

/// Knobs for `lt sim`.
#[derive(Args)]
pub struct SimArgs {
    /// RNG seed; the same seed always produces the same dataset.
    #[arg(long, default_value_t = 0)]
    pub seed: u64,
    /// Number of issues to generate.
    #[arg(long, default_value_t = 100)]
    pub size: usize,
}

/// Generate a dataset and write it into the active profile's local database.
///
/// Marks the cache fresh so the offline list/TUI serve the generated data
/// without attempting a network sync, and records the synced viewer identity
/// (a real assignee from the dataset) so the `--assignee=me` filter resolves.
pub fn run(out: &mut dyn Write, args: &SimArgs, runtime: &Runtime) -> Result<()> {
    let seed = runtime.seed_sim(args.seed, args.size)?;
    writeln!(
        out,
        "Seeded {} issues and {} comments (seed={}, size={}).",
        seed.issues, seed.comments, args.seed, args.size
    )?;
    Ok(())
}
