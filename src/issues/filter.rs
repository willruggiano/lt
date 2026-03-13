use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use super::IssueArgs;

fn parse_date(s: &str, field: &str) -> Result<String> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3
        || parts[0].len() != 4
        || parts[1].len() != 2
        || parts[2].len() != 2
        || !parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(anyhow!(
            "--{}: date must be YYYY-MM-DD, got {:?}",
            field,
            s
        ));
    }
    Ok(format!("{}T00:00:00Z", s))
}

fn parse_priority(s: &str) -> Result<f64> {
    match s.to_lowercase().as_str() {
        "none" | "0" => Ok(0.0),
        "urgent" | "1" => Ok(1.0),
        "high" | "2" => Ok(2.0),
        "normal" | "medium" | "3" => Ok(3.0),
        "low" | "4" => Ok(4.0),
        _ => Err(anyhow!(
            "--priority: expected none/urgent/high/normal/medium/low or 0-4, got {:?}",
            s
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

    match filters.len() {
        0 => Ok(None),
        1 => Ok(Some(filters.remove(0))),
        _ => Ok(Some(json!({ "and": filters }))),
    }
}
