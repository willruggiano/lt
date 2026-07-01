use std::io::Write;

use anyhow::Result;
use lt_storage::text;
use lt_sync::notifications::Notification;

/// Format an ISO-8601 timestamp as a relative age string like '5m ago', '2h ago', '3d ago'.
/// Falls back to the raw string if parsing fails. `now_secs` is the reference
/// "now" (Unix seconds); the binary passes the wall clock, tests a fixed value.
fn relative_age(iso: &str, now_secs: u64) -> String {
    // Parse "2024-01-15T10:30:00.000Z" or "2024-01-15T10:30:00Z"
    // We only need the numeric parts, so a manual approach is used.
    if let Some(ts) = parse_iso8601_secs(iso) {
        let diff = now_secs.saturating_sub(ts);
        if diff < 60 {
            return format!("{diff}s ago");
        } else if diff < 3600 {
            return format!("{}m ago", diff / 60);
        } else if diff < 86400 {
            return format!("{}h ago", diff / 3600);
        }
        return format!("{}d ago", diff / 86400);
    }
    iso.to_string()
}

/// Current Unix timestamp in seconds using `std::time`.
pub fn now_unix_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Minimal ISO-8601 parser: "YYYY-MM-DDTHH:MM:SS..." -> Unix seconds (UTC).
/// Ignores sub-seconds and timezone offsets other than Z.
fn parse_iso8601_secs(s: &str) -> Option<u64> {
    // Expected prefix: "YYYY-MM-DDTHH:MM:SS"
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: i64 = s[5..7].parse().ok()?;
    let day: i64 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let min: i64 = s[14..16].parse().ok()?;
    let sec: i64 = s[17..19].parse().ok()?;

    // Days from epoch (1970-01-01) to the given date using the civil-date algorithm.
    let days = days_from_civil(year, month, day)?;
    let total = days * 86400 + hour * 3600 + min * 60 + sec;
    u64::try_from(total).ok()
}

/// Returns number of days since 1970-01-01 for a given (y, m, d).
fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Adjust year/month so March = month 1
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * m + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days = era * 146_097 + doe - 719_468; // days since 1970-01-01
    Some(days)
}

pub fn print_table(
    out: &mut dyn Write,
    notifications: &[Notification],
    now_secs: u64,
) -> Result<()> {
    // Column widths
    let type_w = notifications
        .iter()
        .map(|n| n.type_.len())
        .max()
        .unwrap_or(4)
        .max(4);

    let issue_w = notifications
        .iter()
        .map(|n| n.issue.as_ref().map_or(0, |i| i.identifier.len()))
        .max()
        .unwrap_or(5)
        .max(5);

    let title_w = notifications
        .iter()
        .map(|n| n.issue.as_ref().map_or(0, |i| i.title.len()))
        .max()
        .unwrap_or(5)
        .clamp(5, 60);

    let actor_w = notifications
        .iter()
        .map(|n| n.actor.as_ref().map_or(1, |a| a.name.len()))
        .max()
        .unwrap_or(5)
        .max(5);

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
        let type_str = &n.type_;
        let issue_id = n.issue.as_ref().map_or("-", |i| i.identifier.as_str());
        let raw_title = n.issue.as_ref().map_or("-", |i| i.title.as_str());
        // Truncate title if needed
        let title = text::truncate(raw_title, title_w);
        let actor = n.actor.as_ref().map_or("-", |a| a.name.as_str());
        let age = relative_age(&n.created_at, now_secs);

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
    fn test_parse_iso8601_basic() {
        // 1970-01-01T00:00:00Z should give 0
        assert_eq!(parse_iso8601_secs("1970-01-01T00:00:00Z"), Some(0));
        // 1970-01-01T00:01:00Z should give 60
        assert_eq!(parse_iso8601_secs("1970-01-01T00:01:00Z"), Some(60));
        // 1970-01-01T01:00:00Z should give 3600
        assert_eq!(parse_iso8601_secs("1970-01-01T01:00:00Z"), Some(3600));
        // 1970-01-02T00:00:00Z should give 86400
        assert_eq!(parse_iso8601_secs("1970-01-02T00:00:00Z"), Some(86400));
    }

    #[test]
    fn test_relative_age_formatting() {
        // Fixed "now" so the age is deterministic.
        // 2020-01-01T00:00:00Z is 0, 2020-01-02T00:00:00Z is one day later.
        let now = parse_iso8601_secs("2020-01-02T01:00:00Z").unwrap();
        assert_eq!(relative_age("2020-01-01T00:00:00Z", now), "1d ago");
        assert_eq!(relative_age("2020-01-02T00:00:00Z", now), "1h ago");
        assert_eq!(relative_age("2020-01-02T00:59:30Z", now), "30s ago");
    }

    #[test]
    fn test_relative_age_invalid() {
        let result = relative_age("not-a-date", 0);
        assert_eq!(result, "not-a-date");
    }

    #[test]
    fn print_table_snapshot() {
        use lt_sync::notifications::{
            Notification, NotificationActor, NotificationIssue, NotificationIssueState,
            NotificationIssueTeam,
        };

        fn issue(identifier: &str, title: &str) -> NotificationIssue {
            NotificationIssue {
                identifier: identifier.into(),
                title: title.into(),
                state: NotificationIssueState {
                    name: "Todo".into(),
                },
                priority: None,
                team: NotificationIssueTeam {
                    name: "Engineering".into(),
                },
            }
        }
        fn actor(name: &str) -> NotificationActor {
            NotificationActor { name: name.into() }
        }
        fn notification(
            type_: &str,
            iss: Option<NotificationIssue>,
            act: Option<NotificationActor>,
            created_at: &str,
        ) -> Notification {
            Notification {
                id: format!("n-{type_}"),
                type_: type_.into(),
                read_at: None,
                created_at: created_at.into(),
                updated_at: created_at.into(),
                issue: iss,
                actor: act,
            }
        }

        // Fixed "now" so the AGE column is deterministic.
        let now = parse_iso8601_secs("2026-01-10T00:00:00Z").unwrap();
        let notifications = vec![
            notification(
                "issueAssignedToYou",
                Some(issue(
                    "ENG-1",
                    "Wire up the deterministic dataset generator",
                )),
                Some(actor("Ada Lovelace")),
                "2026-01-09T23:00:00Z",
            ),
            notification(
                "issueCommentMention",
                Some(issue("ENG-2", "Render markdown in the detail pane")),
                Some(actor("Grace Hopper")),
                "2026-01-08T00:00:00Z",
            ),
            notification("issueStatusChanged", None, None, "2026-01-01T00:00:00Z"),
        ];

        let mut buf = Vec::new();
        print_table(&mut buf, &notifications, now).unwrap();
        insta::assert_snapshot!(String::from_utf8(buf).unwrap());
    }
}
