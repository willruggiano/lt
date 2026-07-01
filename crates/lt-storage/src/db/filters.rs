use anyhow::{Result, anyhow};
use rusqlite::types::ToSql;

use crate::query::parse_date;
use crate::query::{IssueQuery, SortField};

fn parse_priority_label(s: &str) -> Result<String> {
    let label = match s.to_lowercase().as_str() {
        "none" | "0" => "No priority",
        "urgent" | "1" => "Urgent",
        "high" | "2" => "High",
        "normal" | "medium" | "3" => "Medium",
        "low" | "4" => "Low",
        _ => {
            return Err(anyhow!(
                "--priority: expected none/urgent/high/normal/medium/low or 0-4, got {s:?}"
            ));
        }
    };
    Ok(label.to_string())
}

/// Build a SQL WHERE clause and bind parameters from `IssueQuery` filter fields.
///
/// Returns a tuple of:
///   - a WHERE clause string (empty string if no filters)
///   - a Vec of boxed `ToSql` values matching the placeholders in the clause
///
/// The caller is responsible for prepending "WHERE " if the clause is non-empty.
pub fn build_sql_filter(args: &IssueQuery) -> Result<(String, Vec<Box<dyn ToSql>>)> {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn ToSql>> = Vec::new();

    if let Some(team) = &args.team {
        clauses.push("(t.name LIKE ? OR i.team_id = ?)".to_string());
        let pattern = format!("%{team}%");
        params.push(Box::new(pattern));
        params.push(Box::new(team.clone()));
    }

    if let Some(assignee) = &args.assignee {
        if assignee.eq_ignore_ascii_case("me") {
            // "me" is resolved at the call site by the caller who has auth context;
            // we emit a placeholder that the caller must fill with the viewer name.
            clauses.push("ua.name = ?".to_string());
            params.push(Box::new(assignee.clone()));
        } else {
            clauses.push("ua.name LIKE ?".to_string());
            let pattern = format!("%{assignee}%");
            params.push(Box::new(pattern));
        }
    } else if args.no_assignee {
        clauses.push("i.assignee_id IS NULL".to_string());
    }

    if let Some(state) = &args.state {
        clauses.push("s.name LIKE ?".to_string());
        let pattern = format!("%{state}%");
        params.push(Box::new(pattern));
    }

    if let Some(priority_str) = &args.priority {
        let label = parse_priority_label(priority_str)?;
        clauses.push("i.priority_label = ?".to_string());
        params.push(Box::new(label));
    }

    if let Some(title) = &args.title {
        clauses.push("i.title LIKE ?".to_string());
        let pattern = format!("%{title}%");
        params.push(Box::new(pattern));
    }

    if let Some(date) = &args.created_after {
        let ts = parse_date(date, "created-after")?;
        clauses.push("i.created_at >= ?".to_string());
        params.push(Box::new(ts));
    }

    if let Some(date) = &args.created_before {
        let ts = parse_date(date, "created-before")?;
        clauses.push("i.created_at < ?".to_string());
        params.push(Box::new(ts));
    }

    if let Some(date) = &args.updated_after {
        let ts = parse_date(date, "updated-after")?;
        clauses.push("i.updated_at >= ?".to_string());
        params.push(Box::new(ts));
    }

    if let Some(date) = &args.updated_before {
        let ts = parse_date(date, "updated-before")?;
        clauses.push("i.updated_at < ?".to_string());
        params.push(Box::new(ts));
    }

    let sql = clauses.join(" AND ");
    Ok((sql, params))
}

/// The aliased ORDER BY column for a sort field, matching the read model's
/// join aliases (`i` issues, `s` state, `t` team, `ua` assignee).
pub fn sort_column(sort: &SortField) -> &'static str {
    match sort {
        SortField::Created => "i.created_at",
        SortField::Updated => "i.updated_at",
        SortField::Priority => "i.priority_label",
        SortField::Title => "i.title",
        SortField::Assignee => "ua.name",
        SortField::State => "s.name",
        SortField::Team => "t.name",
    }
}

/// Build a SQL ORDER BY clause from `IssueQuery` sort fields.
///
/// Returns a string like "`i.updated_at` DESC" or "`i.title` ASC".
/// The caller is responsible for prepending "ORDER BY ".
pub fn build_sql_order(args: &IssueQuery) -> String {
    let col = sort_column(&args.sort);
    let dir = if args.desc { "DESC" } else { "ASC" };
    format!("{col} {dir}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::IssueQuery;

    fn default_args() -> IssueQuery {
        IssueQuery::default()
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
        assert_eq!(sql, "(t.name LIKE ? OR i.team_id = ?)");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_no_assignee_filter() {
        let mut args = default_args();
        args.no_assignee = true;
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "i.assignee_id IS NULL");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn test_assignee_filter() {
        let mut args = default_args();
        args.assignee = Some("alice".to_string());
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "ua.name LIKE ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_state_filter() {
        let mut args = default_args();
        args.state = Some("in progress".to_string());
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "s.name LIKE ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_priority_filter_by_label() {
        let mut args = default_args();
        args.priority = Some("high".to_string());
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "i.priority_label = ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_priority_filter_by_number() {
        let mut args = default_args();
        args.priority = Some("2".to_string());
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "i.priority_label = ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_title_filter() {
        let mut args = default_args();
        args.title = Some("crash".to_string());
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "i.title LIKE ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_date_filter_created_after() {
        let mut args = default_args();
        args.created_after = Some("2025-01-01".to_string());
        let (sql, params) = build_sql_filter(&args).unwrap();
        assert_eq!(sql, "i.created_at >= ?");
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
        assert_eq!(sql, "s.name LIKE ? AND i.title LIKE ?");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_order_default() {
        let args = default_args();
        let order = build_sql_order(&args);
        // default: sort=Updated, desc=true
        assert_eq!(order, "i.updated_at DESC");
    }

    #[test]
    fn test_order_title_asc() {
        let mut args = default_args();
        args.sort = SortField::Title;
        args.desc = false;
        let order = build_sql_order(&args);
        assert_eq!(order, "i.title ASC");
    }

    #[test]
    fn test_order_priority_asc() {
        let mut args = default_args();
        args.sort = SortField::Priority;
        args.desc = false;
        let order = build_sql_order(&args);
        assert_eq!(order, "i.priority_label ASC");
    }

    #[test]
    fn test_order_assignee_desc() {
        let mut args = default_args();
        args.sort = SortField::Assignee;
        args.desc = true;
        let order = build_sql_order(&args);
        assert_eq!(order, "ua.name DESC");
    }
}
