pub mod display;
pub mod list;
pub mod new;

use std::io::Write;

use anyhow::Result;
use clap::{Args, Subcommand};
use lt_runtime::query::SortField;
use lt_types::issues::IssueFilter;

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

    /// Sort in ascending order (default is descending)
    #[arg(long)]
    pub asc: bool,

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
    /// Lower every filter field except assignee: `--assignee=me` needs a DB
    /// connection to resolve against the synced viewer (`list::resolve_assignee`);
    /// `--no-assignee`/a plain name need no DB access and are set here.
    pub(crate) fn literal_filter(&self) -> Result<IssueFilter> {
        Ok(IssueFilter {
            team: self.team.clone(),
            assignee: if self.no_assignee {
                Some(lt_types::issues::AssigneeFilter::IsNull)
            } else {
                self.assignee
                    .clone()
                    .map(lt_types::issues::AssigneeFilter::Contains)
            },
            state: self.state.clone(),
            priority: self.priority.as_deref().map(str::parse).transpose()?,
            created_after: self
                .created_after
                .as_deref()
                .map(|d| lt_types::query::parse_date(d, "created-after"))
                .transpose()?,
            created_before: self
                .created_before
                .as_deref()
                .map(|d| lt_types::query::parse_date(d, "created-before"))
                .transpose()?,
            updated_after: self
                .updated_after
                .as_deref()
                .map(|d| lt_types::query::parse_date(d, "updated-after"))
                .transpose()?,
            updated_before: self
                .updated_before
                .as_deref()
                .map(|d| lt_types::query::parse_date(d, "updated-before"))
                .transpose()?,
            title: self.title.clone(),
            label: None,
            project: None,
            cycle: None,
            creator: None,
            term: None,
        })
    }

    /// `--asc`'s typed direction (default is descending).
    pub(crate) fn sort_direction(&self) -> lt_types::query::SortDirection {
        if self.asc {
            lt_types::query::SortDirection::Ascending
        } else {
            lt_types::query::SortDirection::Descending
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
