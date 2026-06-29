use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use super::IssueArgs;

pub(crate) fn parse_date(s: &str, field: &str) -> Result<String> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3
        || parts[0].len() != 4
        || parts[1].len() != 2
        || parts[2].len() != 2
        || !parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(anyhow!("--{field}: date must be YYYY-MM-DD, got {s:?}"));
    }
    Ok(format!("{s}T00:00:00Z"))
}

fn parse_priority(s: &str) -> Result<f64> {
    match s.to_lowercase().as_str() {
        "none" | "0" => Ok(0.0),
        "urgent" | "1" => Ok(1.0),
        "high" | "2" => Ok(2.0),
        "normal" | "medium" | "3" => Ok(3.0),
        "low" | "4" => Ok(4.0),
        _ => Err(anyhow!(
            "--priority: expected none/urgent/high/normal/medium/low or 0-4, got {s:?}"
        )),
    }
}

pub fn build_filter(args: &IssueArgs) -> Result<Option<Value>> {
    let mut filters: Vec<Value> = Vec::new();

    if let Some(team) = &args.team {
        filters.push(json!({
            "team": {
                "or": [
                    { "key": { "eqIgnoreCase": team } },
                    { "name": { "containsIgnoreCase": team } }
                ]
            }
        }));
    }

    if let Some(assignee) = &args.assignee {
        if assignee.eq_ignore_ascii_case("me") {
            filters.push(json!({
                "assignee": { "isMe": { "eq": true } }
            }));
        } else {
            filters.push(json!({
                "assignee": {
                    "or": [
                        { "name": { "containsIgnoreCase": assignee } },
                        { "email": { "containsIgnoreCase": assignee } }
                    ]
                }
            }));
        }
    } else if args.no_assignee {
        filters.push(json!({
            "assignee": { "null": true }
        }));
    }

    if let Some(state) = &args.state {
        filters.push(json!({
            "state": { "name": { "containsIgnoreCase": state } }
        }));
    }

    if let Some(priority_str) = &args.priority {
        let priority_val = parse_priority(priority_str)?;
        filters.push(json!({
            "priority": { "eq": priority_val }
        }));
    }

    if let Some(date) = &args.created_after {
        let ts = parse_date(date, "created-after")?;
        filters.push(json!({ "createdAt": { "gte": ts } }));
    }

    if let Some(date) = &args.created_before {
        let ts = parse_date(date, "created-before")?;
        filters.push(json!({ "createdAt": { "lt": ts } }));
    }

    if let Some(date) = &args.updated_after {
        let ts = parse_date(date, "updated-after")?;
        filters.push(json!({ "updatedAt": { "gte": ts } }));
    }

    if let Some(date) = &args.updated_before {
        let ts = parse_date(date, "updated-before")?;
        filters.push(json!({ "updatedAt": { "lt": ts } }));
    }

    if let Some(title) = &args.title {
        filters.push(json!({ "title": { "containsIgnoreCase": title } }));
    }

    match filters.len() {
        0 => Ok(None),
        1 => Ok(Some(filters.remove(0))),
        _ => Ok(Some(json!({ "and": filters }))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issues::IssueArgs;

    #[test]
    fn parse_date_accepts_iso_date() {
        assert_eq!(
            parse_date("2026-06-29", "created-after").unwrap(),
            "2026-06-29T00:00:00Z"
        );
    }

    #[test]
    fn parse_date_rejects_malformed() {
        // Wrong number of parts.
        assert!(parse_date("2026-06", "f").is_err());
        // Wrong component widths.
        assert!(parse_date("26-6-9", "f").is_err());
        assert!(parse_date("2026-6-29", "f").is_err());
        // Non-digit components.
        assert!(parse_date("2026-0a-29", "f").is_err());
    }

    #[test]
    fn parse_priority_maps_labels_and_numbers() {
        // Compare through the JSON encoding to sidestep clippy::float_cmp; the
        // values are small integers so the textual form is exact.
        for (input, want) in [
            ("none", "0.0"),
            ("0", "0.0"),
            ("urgent", "1.0"),
            ("1", "1.0"),
            ("high", "2.0"),
            ("2", "2.0"),
            ("normal", "3.0"),
            ("medium", "3.0"),
            ("3", "3.0"),
            ("low", "4.0"),
            ("4", "4.0"),
            // Case-insensitive.
            ("URGENT", "1.0"),
        ] {
            let got = json!(parse_priority(input).unwrap()).to_string();
            assert_eq!(got, want, "for {input:?}");
        }
    }

    #[test]
    fn parse_priority_rejects_unknown() {
        assert!(parse_priority("bogus").is_err());
    }

    #[test]
    fn build_filter_no_args_is_none() {
        let filter = build_filter(&IssueArgs::default()).unwrap();
        assert!(filter.is_none());
    }

    #[test]
    fn build_filter_single_predicate_is_unwrapped() {
        let args = IssueArgs {
            title: Some("oauth".to_string()),
            ..Default::default()
        };
        let filter = build_filter(&args).unwrap().unwrap();
        assert_eq!(
            filter,
            json!({ "title": { "containsIgnoreCase": "oauth" } })
        );
    }

    #[test]
    fn build_filter_team_matches_key_or_name() {
        let args = IssueArgs {
            team: Some("ENG".to_string()),
            ..Default::default()
        };
        let filter = build_filter(&args).unwrap().unwrap();
        assert_eq!(
            filter,
            json!({
                "team": {
                    "or": [
                        { "key": { "eqIgnoreCase": "ENG" } },
                        { "name": { "containsIgnoreCase": "ENG" } }
                    ]
                }
            })
        );
    }

    #[test]
    fn build_filter_assignee_me_uses_is_me() {
        let args = IssueArgs {
            assignee: Some("ME".to_string()),
            ..Default::default()
        };
        let filter = build_filter(&args).unwrap().unwrap();
        assert_eq!(filter, json!({ "assignee": { "isMe": { "eq": true } } }));
    }

    #[test]
    fn build_filter_assignee_name_matches_name_or_email() {
        let args = IssueArgs {
            assignee: Some("alice".to_string()),
            ..Default::default()
        };
        let filter = build_filter(&args).unwrap().unwrap();
        assert_eq!(
            filter,
            json!({
                "assignee": {
                    "or": [
                        { "name": { "containsIgnoreCase": "alice" } },
                        { "email": { "containsIgnoreCase": "alice" } }
                    ]
                }
            })
        );
    }

    #[test]
    fn build_filter_no_assignee_is_null() {
        let args = IssueArgs {
            no_assignee: true,
            ..Default::default()
        };
        let filter = build_filter(&args).unwrap().unwrap();
        assert_eq!(filter, json!({ "assignee": { "null": true } }));
    }

    #[test]
    fn build_filter_state_and_priority() {
        let args = IssueArgs {
            state: Some("todo".to_string()),
            priority: Some("high".to_string()),
            ..Default::default()
        };
        let filter = build_filter(&args).unwrap().unwrap();
        assert_eq!(
            filter,
            json!({ "and": [
                { "state": { "name": { "containsIgnoreCase": "todo" } } },
                { "priority": { "eq": 2.0 } }
            ] })
        );
    }

    #[test]
    fn build_filter_all_date_bounds() {
        let args = IssueArgs {
            created_after: Some("2026-01-01".to_string()),
            created_before: Some("2026-02-01".to_string()),
            updated_after: Some("2026-03-01".to_string()),
            updated_before: Some("2026-04-01".to_string()),
            ..Default::default()
        };
        let filter = build_filter(&args).unwrap().unwrap();
        assert_eq!(
            filter,
            json!({ "and": [
                { "createdAt": { "gte": "2026-01-01T00:00:00Z" } },
                { "createdAt": { "lt": "2026-02-01T00:00:00Z" } },
                { "updatedAt": { "gte": "2026-03-01T00:00:00Z" } },
                { "updatedAt": { "lt": "2026-04-01T00:00:00Z" } }
            ] })
        );
    }

    #[test]
    fn build_filter_propagates_parse_errors() {
        let bad_priority = IssueArgs {
            priority: Some("bogus".to_string()),
            ..Default::default()
        };
        assert!(build_filter(&bad_priority).is_err());

        let bad_date = IssueArgs {
            created_after: Some("nope".to_string()),
            ..Default::default()
        };
        assert!(build_filter(&bad_date).is_err());
    }
}
