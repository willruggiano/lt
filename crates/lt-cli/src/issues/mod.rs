pub mod display;
pub mod list;
pub mod new;

use std::io::Write;

use anyhow::Result;
use clap::{Args, Subcommand};

use lt_storage::query::{IssueQuery, SortField};

/// Clap value parser for `--sort`: maps a sort key to its [`SortField`], which
/// is intentionally clap-free (it lives in the data layer, `lt-storage`).
fn parse_sort_field(s: &str) -> Result<SortField, String> {
    SortField::from_key(s).ok_or_else(|| format!("invalid sort field: {s}"))
}

#[derive(Args, Clone)]
pub struct IssueArgs {
    /// Filter by team key or name
    #[arg(long)]
    pub team: Option<String>,

    /// Filter by assignee name, email, or "me"
    #[arg(long, conflicts_with = "no_assignee")]
    pub assignee: Option<String>,

    /// Show only unassigned issues
    #[arg(long, conflicts_with = "assignee")]
    pub no_assignee: bool,

    /// Filter by workflow state name
    #[arg(long)]
    pub state: Option<String>,

    /// Filter by priority label (none/urgent/high/normal/medium/low) or number (0-4)
    #[arg(long)]
    pub priority: Option<String>,

    /// Filter issues created on or after this date (YYYY-MM-DD)
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub created_after: Option<String>,

    /// Filter issues created before this date (YYYY-MM-DD)
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub created_before: Option<String>,

    /// Filter issues updated on or after this date (YYYY-MM-DD)
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub updated_after: Option<String>,

    /// Filter issues updated before this date (YYYY-MM-DD)
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub updated_before: Option<String>,

    /// Sort field
    #[arg(long, default_value = "updated", value_parser = parse_sort_field)]
    pub sort: SortField,

    /// Sort in descending order (default is ascending)
    #[arg(long)]
    pub desc: bool,

    /// Filter by title (case-insensitive substring)
    #[arg(long)]
    pub title: Option<String>,

    /// Maximum number of issues to return (capped at 250)
    #[arg(long, default_value = "50")]
    pub limit: u32,

    /// Bypass the local cache and fetch directly from the Linear API
    #[arg(long)]
    pub live: bool,
}

impl IssueArgs {
    /// Lower the clap args into the storage-layer [`IssueQuery`] (drops the
    /// CLI-only `--live` flag, which the caller handles separately).
    pub fn to_query(&self) -> IssueQuery {
        IssueQuery {
            team: self.team.clone(),
            assignee: self.assignee.clone(),
            no_assignee: self.no_assignee,
            state: self.state.clone(),
            priority: self.priority.clone(),
            created_after: self.created_after.clone(),
            created_before: self.created_before.clone(),
            updated_after: self.updated_after.clone(),
            updated_before: self.updated_before.clone(),
            sort: self.sort.clone(),
            desc: self.desc,
            title: self.title.clone(),
            limit: self.limit,
        }
    }
}

#[derive(Subcommand)]
pub enum IssueSubcommand {
    /// Create a new issue interactively (or via flags)
    New {
        /// Team name (skips team prompt)
        #[arg(long)]
        team: Option<String>,
        /// Issue title (skips title prompt)
        #[arg(long)]
        title: Option<String>,
        /// Issue description (skips description prompt)
        #[arg(long)]
        description: Option<String>,
        /// Priority: none/urgent/high/normal/low (skips priority prompt)
        #[arg(long)]
        priority: Option<String>,
        /// Workflow state name (skips state prompt)
        #[arg(long)]
        state: Option<String>,
        /// Assignee name, email, or 'me' (skips assignee prompt)
        #[arg(long)]
        assignee: Option<String>,
    },
}

pub fn run(
    out: &mut dyn Write,
    args: &IssueArgs,
    subcommand: Option<IssueSubcommand>,
) -> Result<()> {
    match subcommand {
        Some(IssueSubcommand::New {
            team,
            title,
            description,
            priority,
            state,
            assignee,
        }) => new::run(
            out,
            &new::NewIssueArgs {
                team,
                title,
                description,
                priority,
                state,
                assignee,
            },
        ),
        None => list::run(out, args),
    }
}
