pub mod detail;
pub mod display;
mod filter;
pub mod list;
pub mod new;
mod sort;

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};

#[derive(Clone, Debug, PartialEq, ValueEnum)]
pub enum SortField {
    Created,
    Updated,
    Priority,
    Title,
    Assignee,
    State,
    Team,
}

impl SortField {
    pub fn label(&self) -> &'static str {
        match self {
            SortField::Created => "created",
            SortField::Updated => "updated",
            SortField::Priority => "priority",
            SortField::Title => "title",
            SortField::Assignee => "assignee",
            SortField::State => "state",
            SortField::Team => "team",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            SortField::Updated => SortField::Created,
            SortField::Created => SortField::Priority,
            SortField::Priority => SortField::Title,
            SortField::Title => SortField::Assignee,
            SortField::Assignee => SortField::State,
            SortField::State => SortField::Team,
            SortField::Team => SortField::Updated,
        }
    }
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
    #[arg(long, default_value = "updated")]
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

impl Default for IssueArgs {
    fn default() -> Self {
        Self {
            team: None,
            assignee: None,
            no_assignee: false,
            state: None,
            priority: None,
            created_after: None,
            created_before: None,
            updated_after: None,
            updated_before: None,
            sort: SortField::Updated,
            desc: true,
            title: None,
            limit: 50,
            live: false,
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

pub fn run(args: IssueArgs, subcommand: Option<IssueSubcommand>) -> Result<()> {
    match subcommand {
        Some(IssueSubcommand::New {
            team,
            title,
            description,
            priority,
            state,
            assignee,
        }) => new::run(new::NewIssueArgs {
            team,
            title,
            description,
            priority,
            state,
            assignee,
        }),
        None => list::run(args),
    }
}
