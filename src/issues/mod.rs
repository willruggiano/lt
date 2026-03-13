mod display;
mod filter;
mod list;
mod sort;

use anyhow::Result;
use clap::{Args, ValueEnum};

#[derive(Clone, ValueEnum)]
pub enum SortField {
    Created,
    Updated,
    Priority,
    Title,
    Assignee,
    State,
    Team,
}

#[derive(Args)]
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
    #[arg(long, default_value = "updated")]
    pub sort: SortField,

    /// Sort in descending order (default is ascending)
    #[arg(long)]
    pub desc: bool,

    /// Maximum number of issues to return (capped at 250)
    #[arg(long, default_value = "50")]
    pub limit: u32,
}

pub fn run(args: IssueArgs) -> Result<()> {
    list::run(args)
}
