use std::io::Write;

use anyhow::Result;
use lt_runtime::notifications::Notification;
use lt_runtime::text;
use lt_types::scalars::DateTime;

/// Format a wire timestamp as a relative age string like '5m ago', '2h ago', '3d ago'.
/// `now_secs` is the reference "now" (Unix seconds); the binary passes the wall
/// clock, tests a fixed value.
fn relative_age(dt: &DateTime, now_secs: u64) -> String {
    let ts = u64::try_from(dt.0.timestamp()).unwrap_or(0);
    let diff = now_secs.saturating_sub(ts);
    if diff < 60 {
        format!("{diff}s ago")
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

/// Current Unix timestamp in seconds using `std::time`.
pub fn now_unix_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The widest value `it` yields, no narrower than `min`.
fn col_width(it: impl Iterator<Item = usize>, min: usize) -> usize {
    it.fold(min, usize::max)
}

pub fn print_table(
    out: &mut dyn Write,
    notifications: &[Notification],
    now_secs: u64,
) -> Result<()> {
    // Column widths
    let type_w = col_width(notifications.iter().map(|n| n.type_().len()), 4);

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
        "{:<type_w$}  {:<issue_w$}  {:<title_w$}  {:<actor_w$}  AGE",
        "TYPE",
        "ISSUE",
        "TITLE",
        "ACTOR",
        type_w = type_w,
        issue_w = issue_w,
        title_w = title_w,
        actor_w = actor_w,
    )?;

    let sep_len = type_w + 2 + issue_w + 2 + title_w + 2 + actor_w + 2 + 6;
    writeln!(out, "{}", "-".repeat(sep_len))?;

    for n in notifications {
        let type_str = n.type_();
        let issue_id = n.issue().map_or("-", |i| i.identifier.as_str());
        let raw_title = n.issue().map_or("-", |i| i.title.as_str());
        // Truncate title if needed
        let title = text::truncate(raw_title, title_w);
        let actor = n.actor().map_or("-", |a| a.name.as_str());
        let age = relative_age(n.created_at(), now_secs);

        writeln!(
            out,
            "{type_str:<type_w$}  {issue_id:<issue_w$}  {title:<title_w$}  {actor:<actor_w$}  {age}",
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_relative_age_formatting() {
        // Fixed "now" so the age is deterministic.
        // 2020-01-01T00:00:00Z is 0, 2020-01-02T00:00:00Z is one day later.
        let now = u64::try_from(
            "2020-01-02T01:00:00Z"
                .parse::<DateTime>()
                .unwrap()
                .0
                .timestamp(),
        )
        .unwrap();
        assert_eq!(
            relative_age(&"2020-01-01T00:00:00Z".parse().unwrap(), now),
            "1d ago"
        );
        assert_eq!(
            relative_age(&"2020-01-02T00:00:00Z".parse().unwrap(), now),
            "1h ago"
        );
        assert_eq!(
            relative_age(&"2020-01-02T00:59:30Z".parse().unwrap(), now),
            "30s ago"
        );
    }

    #[test]
    fn print_table_snapshot() {
        use lt_types::notifications::{BaseNotification, IssueNotification};
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
            type_: &'static str,
            issue: lt_types::types::Issue,
            actor: User,
            created_at: &'static str,
        }

        fn issue_notification(f: IssueNotificationFixture) -> Notification {
            Notification::IssueNotification(Box::new(IssueNotification {
                id: format!("n-{}", f.type_).into(),
                type_: f.type_.into(),
                read_at: None,
                created_at: f.created_at.parse().unwrap(),
                updated_at: f.created_at.parse().unwrap(),
                actor: Some(f.actor),
                issue: f.issue,
            }))
        }

        fn base_notification(type_: &str, created_at: &str) -> Notification {
            Notification::Other(BaseNotification {
                id: format!("n-{type_}").into(),
                type_: type_.into(),
                read_at: None,
                created_at: created_at.parse().unwrap(),
                updated_at: created_at.parse().unwrap(),
                actor: None,
            })
        }

        // Fixed "now" so the AGE column is deterministic.
        let now = u64::try_from(
            "2026-01-10T00:00:00Z"
                .parse::<DateTime>()
                .unwrap()
                .0
                .timestamp(),
        )
        .unwrap();
        let notifications = vec![
            issue_notification(IssueNotificationFixture {
                type_: "issueAssignedToYou",
                issue: sample_issue("1", "ENG-1", "Wire up the deterministic dataset generator"),
                actor: actor("Ada Lovelace"),
                created_at: "2026-01-09T23:00:00Z",
            }),
            issue_notification(IssueNotificationFixture {
                type_: "issueCommentMention",
                issue: sample_issue("2", "ENG-2", "Render markdown in the detail pane"),
                actor: actor("Grace Hopper"),
                created_at: "2026-01-08T00:00:00Z",
            }),
            base_notification("issueStatusChanged", "2026-01-01T00:00:00Z"),
        ];

        let mut buf = Vec::new();
        print_table(&mut buf, &notifications, now).unwrap();
        insta::assert_snapshot!(String::from_utf8(buf).unwrap());
    }
}
