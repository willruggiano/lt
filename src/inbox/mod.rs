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
    let notifications = fetch_notifications_from_config(args.limit)?;

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
