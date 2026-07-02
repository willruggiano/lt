use anyhow::{Result, anyhow};
use lt_types::query::{IssueQuery, SortField, parse_date};

use crate::db::sql::{self, BindParams, Frag, SortCol};

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

/// Select the registered `WHERE`-clause fragments and bind parameters that
/// apply to `IssueQuery`'s filter fields.
///
/// Returns a tuple of:
///   - the selected fragments, in the order they must be `AND`-joined
///   - a Vec of boxed `ToSql` values matching the placeholders in those fragments
///
/// The caller composes the final statement via
/// [`crate::db::sql::select_issues`] / [`crate::db::sql::select_issues_page`].
pub(crate) fn build_sql_filter(args: &IssueQuery) -> Result<(Vec<Frag>, BindParams)> {
    let mut conditions: Vec<Frag> = Vec::new();
    let mut params: BindParams = Vec::new();

    if let Some(team) = &args.team {
        conditions.push(sql::FRAG_TEAM_OR_ID);
        let pattern = format!("%{team}%");
        params.push(Box::new(pattern));
        params.push(Box::new(team.clone()));
    }

    if let Some(assignee) = &args.assignee {
        if assignee.eq_ignore_ascii_case("me") {
            // "me" is resolved at the call site by the caller who has auth context;
            // we emit a placeholder that the caller must fill with the viewer name.
            conditions.push(sql::FRAG_ASSIGNEE_EQ);
            params.push(Box::new(assignee.clone()));
        } else {
            conditions.push(sql::FRAG_ASSIGNEE_LIKE);
            let pattern = format!("%{assignee}%");
            params.push(Box::new(pattern));
        }
    } else if args.no_assignee {
        conditions.push(sql::FRAG_NO_ASSIGNEE);
    }

    if let Some(state) = &args.state {
        conditions.push(sql::FRAG_STATE_LIKE);
        let pattern = format!("%{state}%");
        params.push(Box::new(pattern));
    }

    if let Some(priority_str) = &args.priority {
        let label = parse_priority_label(priority_str)?;
        conditions.push(sql::FRAG_PRIORITY_EQ);
        params.push(Box::new(label));
    }

    if let Some(title) = &args.title {
        conditions.push(sql::FRAG_TITLE_LIKE);
        let pattern = format!("%{title}%");
        params.push(Box::new(pattern));
    }

    if let Some(date) = &args.created_after {
        let ts = parse_date(date, "created-after")?;
        conditions.push(sql::FRAG_CREATED_AFTER);
        params.push(Box::new(ts));
    }

    if let Some(date) = &args.created_before {
        let ts = parse_date(date, "created-before")?;
        conditions.push(sql::FRAG_CREATED_BEFORE);
        params.push(Box::new(ts));
    }

    if let Some(date) = &args.updated_after {
        let ts = parse_date(date, "updated-after")?;
        conditions.push(sql::FRAG_UPDATED_AFTER);
        params.push(Box::new(ts));
    }

    if let Some(date) = &args.updated_before {
        let ts = parse_date(date, "updated-before")?;
        conditions.push(sql::FRAG_UPDATED_BEFORE);
        params.push(Box::new(ts));
    }

    Ok((conditions, params))
}

/// The registered `ORDER BY` column for a sort field, matching the read
/// model's join aliases (`i` issues, `s` state, `t` team, `ua` assignee).
/// Exhaustive over [`SortField`], so a new sort field fails compilation here
/// until mapped -- shared by `search_query.rs`, which uses the same
/// generated `SortField` type.
pub(crate) fn sort_column(sort: &SortField) -> SortCol {
    match sort {
        SortField::Created => sql::SORT_CREATED_AT,
        SortField::Updated => sql::SORT_UPDATED_AT,
        SortField::Priority => sql::SORT_PRIORITY_LABEL,
        SortField::Title => sql::SORT_TITLE,
        SortField::Assignee => sql::SORT_ASSIGNEE_NAME,
        SortField::State => sql::SORT_STATE_NAME,
        SortField::Team => sql::SORT_TEAM_NAME,
    }
}

#[cfg(test)]
mod tests {
    use lt_types::query::IssueQuery;

    use super::*;

    fn default_args() -> IssueQuery {
        IssueQuery::default()
    }

    #[test]
    fn test_no_filters_returns_empty_clause() {
        let args = default_args();
        let (conditions, params) = build_sql_filter(&args).unwrap();
        assert!(conditions.is_empty());
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn test_team_filter() {
        let mut args = default_args();
        args.team = Some("backend".to_string());
        let (conditions, params) = build_sql_filter(&args).unwrap();
        assert_eq!(conditions, vec![sql::FRAG_TEAM_OR_ID]);
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_no_assignee_filter() {
        let mut args = default_args();
        args.no_assignee = true;
        let (conditions, params) = build_sql_filter(&args).unwrap();
        assert_eq!(conditions, vec![sql::FRAG_NO_ASSIGNEE]);
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn test_assignee_filter() {
        let mut args = default_args();
        args.assignee = Some("alice".to_string());
        let (conditions, params) = build_sql_filter(&args).unwrap();
        assert_eq!(conditions, vec![sql::FRAG_ASSIGNEE_LIKE]);
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_state_filter() {
        let mut args = default_args();
        args.state = Some("in progress".to_string());
        let (conditions, params) = build_sql_filter(&args).unwrap();
        assert_eq!(conditions, vec![sql::FRAG_STATE_LIKE]);
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_priority_filter_by_label() {
        let mut args = default_args();
        args.priority = Some("high".to_string());
        let (conditions, params) = build_sql_filter(&args).unwrap();
        assert_eq!(conditions, vec![sql::FRAG_PRIORITY_EQ]);
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_priority_filter_by_number() {
        let mut args = default_args();
        args.priority = Some("2".to_string());
        let (conditions, params) = build_sql_filter(&args).unwrap();
        assert_eq!(conditions, vec![sql::FRAG_PRIORITY_EQ]);
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_title_filter() {
        let mut args = default_args();
        args.title = Some("crash".to_string());
        let (conditions, params) = build_sql_filter(&args).unwrap();
        assert_eq!(conditions, vec![sql::FRAG_TITLE_LIKE]);
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_date_filter_created_after() {
        let mut args = default_args();
        args.created_after = Some("2025-01-01".to_string());
        let (conditions, params) = build_sql_filter(&args).unwrap();
        assert_eq!(conditions, vec![sql::FRAG_CREATED_AFTER]);
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
        let (conditions, params) = build_sql_filter(&args).unwrap();
        assert_eq!(conditions, vec![sql::FRAG_STATE_LIKE, sql::FRAG_TITLE_LIKE]);
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_order_default() {
        let args = default_args();
        // default: sort=Updated, desc=true
        assert_eq!(sort_column(&args.sort), sql::SORT_UPDATED_AT);
        assert!(args.desc);
    }

    #[test]
    fn test_order_title_asc() {
        let mut args = default_args();
        args.sort = SortField::Title;
        args.desc = false;
        assert_eq!(sort_column(&args.sort), sql::SORT_TITLE);
        assert!(!args.desc);
    }

    #[test]
    fn test_order_priority_asc() {
        let mut args = default_args();
        args.sort = SortField::Priority;
        args.desc = false;
        assert_eq!(sort_column(&args.sort), sql::SORT_PRIORITY_LABEL);
        assert!(!args.desc);
    }

    #[test]
    fn test_order_assignee_desc() {
        let mut args = default_args();
        args.sort = SortField::Assignee;
        args.desc = true;
        assert_eq!(sort_column(&args.sort), sql::SORT_ASSIGNEE_NAME);
        assert!(args.desc);
    }
}
