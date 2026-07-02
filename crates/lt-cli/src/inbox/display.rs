use std::io::Write;

use anyhow::Result;
use chrono::Utc;
use lt_runtime::notifications::Notification;
use lt_runtime::text;
use lt_types::notifications::NotificationCategory;

/// The current wall-clock time.
pub fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

/// The widest value `it` yields, no narrower than `min`.
fn col_width(it: impl Iterator<Item = usize>, min: usize) -> usize {
    it.fold(min, usize::max)
}

/// A short, humane label for a notification's `category`, exhaustive over
/// Linear's `NotificationCategory` enum. `Other` is the decode fallback for a
/// category added to the schema after this build; render its raw wire value.
fn category_label(category: &NotificationCategory) -> &str {
    match category {
        NotificationCategory::AppsAndIntegrations => "Integration",
        NotificationCategory::Assignments => "Assigned",
        NotificationCategory::Billing => "Billing",
        NotificationCategory::CommentsAndReplies => "Comment",
        NotificationCategory::Customers => "Customer",
        NotificationCategory::DocumentChanges => "Document",
        NotificationCategory::Feed => "Feed",
        NotificationCategory::Mentions => "Mention",
        NotificationCategory::PostsAndUpdates => "Post",
        NotificationCategory::Reactions => "Reaction",
        NotificationCategory::Reminders => "Reminder",
        NotificationCategory::Reviews => "Review",
        NotificationCategory::StatusChanges => "Status",
        NotificationCategory::Subscriptions => "Subscribed",
        NotificationCategory::System => "System",
        NotificationCategory::Triage => "Triage",
        NotificationCategory::Other(raw) => raw.as_str(),
    }
}

pub fn print_table(
    out: &mut dyn Write,
    notifications: &[Notification],
    now: chrono::DateTime<Utc>,
) -> Result<()> {
    // Column widths
    let category_w = col_width(
        notifications
            .iter()
            .map(|n| category_label(n.category()).len()),
        8,
    );

    let issue_w = col_width(
        notifications
            .iter()
            .map(|n| n.issue().map_or(0, |i| i.identifier.len())),
        5,
    );

    let title_w = col_width(
        notifications
            .iter()
            .map(|n| n.issue().map_or(0, |i| i.title.len())),
        5,
    )
    .min(60);

    let actor_w = col_width(
        notifications
            .iter()
            .map(|n| n.actor().map_or(1, |a| a.name.len())),
        5,
    );

    // Header
    writeln!(
        out,
        "{:<category_w$}  {:<issue_w$}  {:<title_w$}  {:<actor_w$}  AGE",
        "CATEGORY",
        "ISSUE",
        "TITLE",
        "ACTOR",
        category_w = category_w,
        issue_w = issue_w,
        title_w = title_w,
        actor_w = actor_w,
    )?;

    let sep_len = category_w + 2 + issue_w + 2 + title_w + 2 + actor_w + 2 + 6;
    writeln!(out, "{}", "-".repeat(sep_len))?;

    for n in notifications {
        let category = category_label(n.category());
        let issue_id = n.issue().map_or("-", |i| i.identifier.as_str());
        let raw_title = n.issue().map_or("-", |i| i.title.as_str());
        // Truncate title if needed
        let title = text::truncate(raw_title, title_w);
        let actor = n.actor().map_or("-", |a| a.name.as_str());
        let age = n.created_at().relative_age(now);

        writeln!(
            out,
            "{category:<category_w$}  {issue_id:<issue_w$}  {title:<title_w$}  {actor:<actor_w$}  {age}",
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_table_snapshot() {
        use lt_types::notifications::{BaseNotification, IssueNotification};
        use lt_types::scalars::DateTime;
        use lt_types::types::User;

        use crate::issues::display::tests::sample_issue;

        fn actor(name: &str) -> User {
            User {
                id: "a".into(),
                name: name.into(),
            }
        }

        /// Carries the fields distinguishing one issue-notification fixture,
        /// keeping `issue_notification` under the argument-count limit.
        struct IssueNotificationFixture {
            id: &'static str,
            category: NotificationCategory,
            issue: lt_types::types::Issue,
            actor: User,
            created_at: &'static str,
        }

        fn issue_notification(f: IssueNotificationFixture) -> Notification {
            Notification::IssueNotification(Box::new(IssueNotification {
                id: f.id.into(),
                category: f.category,
                read_at: None,
                created_at: f.created_at.parse().unwrap(),
                updated_at: f.created_at.parse().unwrap(),
                actor: Some(f.actor),
                issue: f.issue,
            }))
        }

        fn base_notification(
            id: &str,
            category: NotificationCategory,
            created_at: &str,
        ) -> Notification {
            Notification::Other(BaseNotification {
                id: id.into(),
                category,
                read_at: None,
                created_at: created_at.parse().unwrap(),
                updated_at: created_at.parse().unwrap(),
                actor: None,
            })
        }

        // Fixed "now" so the AGE column is deterministic.
        let now = "2026-01-10T00:00:00Z".parse::<DateTime>().unwrap().0;
        let notifications = vec![
            issue_notification(IssueNotificationFixture {
                id: "n-assigned",
                category: NotificationCategory::Assignments,
                issue: sample_issue("1", "ENG-1", "Wire up the deterministic dataset generator"),
                actor: actor("Ada Lovelace"),
                created_at: "2026-01-09T23:00:00Z",
            }),
            issue_notification(IssueNotificationFixture {
                id: "n-mention",
                category: NotificationCategory::Mentions,
                issue: sample_issue("2", "ENG-2", "Render markdown in the detail pane"),
                actor: actor("Grace Hopper"),
                created_at: "2026-01-08T00:00:00Z",
            }),
            base_notification(
                "n-status",
                NotificationCategory::StatusChanges,
                "2026-01-01T00:00:00Z",
            ),
        ];

        let mut buf = Vec::new();
        print_table(&mut buf, &notifications, now).unwrap();
        insta::assert_snapshot!(String::from_utf8(buf).unwrap());
    }
}
