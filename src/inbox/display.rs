use crate::linear::notifications::Notification;

/// Format an ISO-8601 timestamp as a relative age string like '5m ago', '2h ago', '3d ago'.
/// Falls back to the raw string if parsing fails.
fn relative_age(iso: &str) -> String {
    // Parse "2024-01-15T10:30:00.000Z" or "2024-01-15T10:30:00Z"
    // We only need the numeric parts, so a manual approach is used.
    let now_secs = now_unix_secs();
    if let Some(ts) = parse_iso8601_secs(iso) {
        let diff = now_secs.saturating_sub(ts);
        if diff < 60 {
            return format!("{}s ago", diff);
        } else if diff < 3600 {
            return format!("{}m ago", diff / 60);
        } else if diff < 86400 {
            return format!("{}h ago", diff / 3600);
        } else {
            return format!("{}d ago", diff / 86400);
        }
    }
    iso.to_string()
}

/// Current Unix timestamp in seconds using std::time.
fn now_unix_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
    if total < 0 {
        return None;
    }
    Some(total as u64)
}

/// Returns number of days since 1970-01-01 for a given (y, m, d).
fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    if m < 1 || m > 12 || d < 1 || d > 31 {
        return None;
    }
    // Adjust year/month so March = month 1
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * m + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days = era * 146097 + doe - 719468; // days since 1970-01-01
    Some(days)
}

pub fn print_table(notifications: &[Notification]) {
    // Column widths
    let type_w = notifications
        .iter()
        .map(|n| n.type_.len())
        .max()
        .unwrap_or(4)
        .max(4);

    let issue_w = notifications
        .iter()
        .map(|n| n.issue.as_ref().map(|i| i.identifier.len()).unwrap_or(0))
        .max()
        .unwrap_or(5)
        .max(5);

    let title_w = notifications
        .iter()
        .map(|n| n.issue.as_ref().map(|i| i.title.len()).unwrap_or(0))
        .max()
        .unwrap_or(5)
        .max(5)
        .min(60);

    let actor_w = notifications
        .iter()
        .map(|n| n.actor.as_ref().map(|a| a.name.len()).unwrap_or(1))
        .max()
        .unwrap_or(5)
        .max(5);

    // Header
    println!(
        "{:<type_w$}  {:<issue_w$}  {:<title_w$}  {:<actor_w$}  {}",
        "TYPE",
        "ISSUE",
        "TITLE",
        "ACTOR",
        "AGE",
        type_w = type_w,
        issue_w = issue_w,
        title_w = title_w,
        actor_w = actor_w,
    );

    let sep_len = type_w + 2 + issue_w + 2 + title_w + 2 + actor_w + 2 + 6;
    println!("{}", "-".repeat(sep_len));

    for n in notifications {
        let type_str = &n.type_;
        let issue_id = n
            .issue
            .as_ref()
            .map(|i| i.identifier.as_str())
            .unwrap_or("-");
        let raw_title = n.issue.as_ref().map(|i| i.title.as_str()).unwrap_or("-");
        // Truncate title if needed
        let title: String = if raw_title.len() > title_w {
            format!("{}...", &raw_title[..title_w.saturating_sub(3)])
        } else {
            raw_title.to_string()
        };
        let actor = n.actor.as_ref().map(|a| a.name.as_str()).unwrap_or("-");
        let age = relative_age(&n.created_at);

        println!(
            "{:<type_w$}  {:<issue_w$}  {:<title_w$}  {:<actor_w$}  {}",
            type_str,
            issue_id,
            title,
            actor,
            age,
            type_w = type_w,
            issue_w = issue_w,
            title_w = title_w,
            actor_w = actor_w,
        );
    }
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
        // Use a known timestamp far in the past so the age is stable.
        // We test that relative_age returns something ending in " ago".
        // 2020-01-01T00:00:00Z is definitely > 1 day ago.
        let result = relative_age("2020-01-01T00:00:00Z");
        assert!(
            result.ends_with("d ago") || result.ends_with("h ago"),
            "unexpected: {}",
            result
        );
    }

    #[test]
    fn test_relative_age_invalid() {
        let result = relative_age("not-a-date");
        assert_eq!(result, "not-a-date");
    }
}
