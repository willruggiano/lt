use anyhow::Result;
use clap::Args;

mod display;

use crate::linear::notifications::fetch_notifications_from_config;

#[derive(Args, Clone)]
pub struct InboxArgs {
    /// Show read and unread notifications (default: unread only)
    #[arg(long)]
    pub all: bool,

    /// Maximum number of notifications to fetch
    #[arg(long, default_value = "20")]
    pub limit: usize,
}

pub fn run(args: InboxArgs) -> Result<()> {
    // When --all is set we need to paginate through all pages but still cap the
    // total at --limit.  Pass max_total so the pagination loop stops early.
    // When --all is not set we only need unread notifications; fetch with the
    // limit as a page-size hint and filter afterward.
    let max_total = Some(args.limit);
    let notifications = fetch_notifications_from_config(args.limit, max_total)?;

    let filtered: Vec<_> = if args.all {
        notifications
    } else {
        notifications
            .into_iter()
            .filter(|n| n.read_at.is_none())
            .collect()
    };

    if filtered.is_empty() {
        println!("Inbox zero.");
        return Ok(());
    }

    display::print_table(&filtered);
    Ok(())
}
