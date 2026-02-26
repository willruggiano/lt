use anyhow::{Result, anyhow};
use rusqlite::types::ToSql;

use crate::issues::{IssueArgs, SortField};

fn parse_date(s: &str, field: &str) -> Result<String> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3
        || parts[0].len() != 4
        || parts[1].len() != 2
        || parts[2].len() != 2
        || !parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(anyhow!("--{}: date must be YYYY-MM-DD, got {:?}", field, s));
    }
    Ok(format!("{}T00:00:00Z", s))
}

fn parse_priority_label(s: &str) -> Result<String> {
    let label = match s.to_lowercase().as_str() {
        "none" | "0" => "No priority",
        "urgent" | "1" => "Urgent",
        "high" | "2" => "High",
        "normal" | "medium" | "3" => "Medium",
        "low" | "4" => "Low",
        _ => {
            return Err(anyhow!(
                "--priority: expected none/urgent/high/normal/medium/low or 0-4, got {:?}",
                s
            ))
        }
    };
    Ok(label.to_string())
}

/// Build a SQL WHERE clause and bind parameters from IssueArgs filter fields.
///
/// Returns a tuple of:
///   - a WHERE clause string (empty string if no filters)
///   - a Vec of boxed ToSql values matching the placeholders in the clause
///
/// The caller is responsible for prepending "WHERE " if the clause is non-empty.
pub fn build_sql_filter(args: &IssueArgs) -> Result<(String, Vec<Box<dyn ToSql>>)> {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn ToSql>> = Vec::new();

    if let Some(team) = &args.team {
        clauses.push("(team_name LIKE ? OR team_key = ?)".to_string());
        let pattern = format!("%{}%", team);
        params.push(Box::new(pattern));
        params.push(Box::new(team.clone()));
    }

    if let Some(assignee) = &args.assignee {
        if assignee.eq_ignore_ascii_case("me") {
            // "me" is resolved at the call site by the caller who has auth context;
            // we emit a placeholder that the caller must fill with the viewer name.
            clauses.push("assignee_name = ?".to_string());
            params.push(Box::new(assignee.clone()));
        } else {
            clauses.push("assignee_name LIKE ?".to_string());
            let pattern = format!("%{}%", assignee);
            params.push(Box::new(pattern));
        }
    } else if args.no_assignee {
        clauses.push("assignee_name IS NULL".to_string());
    }

    if let Some(state) = &args.state {
        clauses.push("state_name LIKE ?".to_string());
        let pattern = format!("%{}%", state);
        params.push(Box::new(pattern));
    }

    if let Some(priority_str) = &args.priority {
        let label = parse_priority_label(priority_str)?;
        clauses.push("priority_label = ?".to_string());
        params.push(Box::new(label));
    }

    if let Some(title) = &args.title {
        clauses.push("title LIKE ?".to_string());
        let pattern = format!("%{}%", title);
        params.push(Box::new(pattern));
    }

    if let Some(date) = &args.created_after {
        let ts = parse_date(date, "created-after")?;
        clauses.push("created_at >= ?".to_string());
        params.push(Box::new(ts));
    }

    if let Some(date) = &args.created_before {
        let ts = parse_date(date, "created-before")?;
        clauses.push("created_at < ?".to_string());
        params.push(Box::new(ts));
    }

    if let Some(date) = &args.updated_after {
        let ts = parse_date(date, "updated-after")?;
        clauses.push("updated_at >= ?".to_string());
        params.push(Box::new(ts));
    }

    if let Some(date) = &args.updated_before {
        let ts = parse_date(date, "updated-before")?;
        clauses.push("updated_at < ?".to_string());
        params.push(Box::new(ts));
    }

    let sql = clauses.join(" AND ");
    Ok((sql, params))
}

/// Build a SQL ORDER BY clause from IssueArgs sort fields.
///
/// Returns a string like "updated_at DESC" or "title ASC".
/// The caller is responsible for prepending "ORDER BY ".
pub fn build_sql_order(args: &IssueArgs) -> String {
    let col = match args.sort {
        SortField::Created => "created_at",
        SortField::Updated => "updated_at",
        SortField::Priority => "priority_label",
        SortField::Title => "title",
        SortField::Assignee => "assignee_name",
        SortField::State => "state_name",
        SortField::Team => "team_name",
    };
    let dir = if args.desc { "DESC" } else { "ASC" };
    format!("{} {}", col, dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issues::IssueArgs;

    fn default_args() -> IssueArgs {
        IssueArgs::default()
    }

    #[test]
    fn test_no_filters_returns_empty_clause() {
        let args = default_args();
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn test_team_filter() {
        let mut args = default_args();
        args.team = Some("backend".to_string());
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "(team_name LIKE ? OR team_key = ?)");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_no_assignee_filter() {
        let mut args = default_args();
        args.no_assignee = true;
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "assignee_name IS NULL");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn test_assignee_filter() {
        let mut args = default_args();
        args.assignee = Some("alice".to_string());
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "assignee_name LIKE ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_state_filter() {
        let mut args = default_args();
        args.state = Some("in progress".to_string());
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "state_name LIKE ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_priority_filter_by_label() {
        let mut args = default_args();
        args.priority = Some("high".to_string());
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "priority_label = ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_priority_filter_by_number() {
        let mut args = default_args();
        args.priority = Some("2".to_string());
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "priority_label = ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_title_filter() {
        let mut args = default_args();
        args.title = Some("crash".to_string());
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "title LIKE ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_date_filter_created_after() {
        let mut args = default_args();
        args.created_after = Some("2025-01-01".to_string());
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "created_at >= ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_invalid_date_returns_error() {
        let mut args = default_args();
        args.created_after = Some("not-a-date".to_string());
        let result = build_sql_filter(&args);
        assert!(result.is_err());
    }

    #[test]
    fn test_multiple_filters_joined_with_and() {
        let mut args = default_args();
        args.state = Some("todo".to_string());
        args.title = Some("bug".to_string());
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "state_name LIKE ? AND title LIKE ?");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_order_default() {
        let args = default_args();
        let order = build_sql_order(&args);
        // default: sort=Updated, desc=true
        assert_eq!(order, "updated_at DESC");
    }

    #[test]
    fn test_order_title_asc() {
        let mut args = default_args();
        args.sort = SortField::Title;
        args.desc = false;
        let order = build_sql_order(&args);
        assert_eq!(order, "title ASC");
    }

    #[test]
    fn test_order_priority_asc() {
        let mut args = default_args();
        args.sort = SortField::Priority;
        args.desc = false;
        let order = build_sql_order(&args);
        assert_eq!(order, "priority_label ASC");
    }

    #[test]
    fn test_order_assignee_desc() {
        let mut args = default_args();
        args.sort = SortField::Assignee;
        args.desc = true;
        let order = build_sql_order(&args);
        assert_eq!(order, "assignee_name DESC");
    }
}
