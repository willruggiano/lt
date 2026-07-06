use lt_types::issues::{AssigneeFilter, IssueFilter};
use lt_types::query::SortField;

use crate::db::sql::{self, BindParams, Frag, SortCol};

/// Lower an [`IssueFilter`] to the registered `WHERE`-clause fragments and
/// bind parameters that apply, plus the FTS5 match term (`filter.term`) when
/// set.
///
/// Returns a tuple of:
///   - the selected fragments, in the order they must be `AND`-joined
///   - a Vec of boxed `ToSql` values matching the placeholders in those fragments
///   - the FTS5 match term, if `filter.term` is set
///
/// The caller composes the final statement via [`crate::db::sql::select_issues`].
pub(crate) fn build_sql_filter(filter: &IssueFilter) -> (Vec<Frag>, BindParams, Option<String>) {
    let mut conditions: Vec<Frag> = Vec::new();
    let mut params: BindParams = Vec::new();

    if let Some(team) = &filter.team {
        conditions.push(sql::FRAG_TEAM_LOWER_OR_ID);
        let pattern = format!("%{}%", team.to_lowercase());
        params.push(Box::new(pattern.clone()));
        params.push(Box::new(pattern));
    }

    match &filter.assignee {
        Some(AssigneeFilter::IsNull) => {
            conditions.push(sql::FRAG_NO_ASSIGNEE);
        }
        Some(AssigneeFilter::Exact(name)) => {
            conditions.push(sql::FRAG_ASSIGNEE_EQ);
            params.push(Box::new(name.clone()));
        }
        Some(AssigneeFilter::Contains(name)) => {
            conditions.push(sql::FRAG_ASSIGNEE_LOWER_LIKE);
            params.push(Box::new(format!("%{}%", name.to_lowercase())));
        }
        None => {}
    }

    if let Some(state) = &filter.state {
        conditions.push(sql::FRAG_STATE_LOWER_LIKE);
        params.push(Box::new(format!("%{}%", state.to_lowercase())));
    }

    if let Some(priority) = filter.priority {
        conditions.push(sql::FRAG_PRIORITY_EQ);
        params.push(Box::new(priority.label().to_string()));
    }

    if let Some(title) = &filter.title {
        conditions.push(sql::FRAG_TITLE_LIKE);
        params.push(Box::new(format!("%{title}%")));
    }

    if let Some(ts) = &filter.created_after {
        conditions.push(sql::FRAG_CREATED_AFTER);
        params.push(Box::new(ts.clone()));
    }

    if let Some(ts) = &filter.created_before {
        conditions.push(sql::FRAG_CREATED_BEFORE);
        params.push(Box::new(ts.clone()));
    }

    if let Some(ts) = &filter.updated_after {
        conditions.push(sql::FRAG_UPDATED_AFTER);
        params.push(Box::new(ts.clone()));
    }

    if let Some(ts) = &filter.updated_before {
        conditions.push(sql::FRAG_UPDATED_BEFORE);
        params.push(Box::new(ts.clone()));
    }

    if let Some(label) = &filter.label {
        conditions.push(sql::FRAG_LABEL_EXISTS);
        params.push(Box::new(format!("%{}%", label.to_lowercase())));
    }

    if let Some(project) = &filter.project {
        conditions.push(sql::FRAG_PROJECT_LOWER_LIKE);
        params.push(Box::new(format!("%{}%", project.to_lowercase())));
    }

    if let Some(cycle) = &filter.cycle {
        conditions.push(sql::FRAG_CYCLE_LOWER_LIKE);
        params.push(Box::new(format!("%{}%", cycle.to_lowercase())));
    }

    if let Some(creator) = &filter.creator {
        conditions.push(sql::FRAG_CREATOR_LOWER_LIKE);
        params.push(Box::new(format!("%{}%", creator.to_lowercase())));
    }

    (conditions, params, filter.term.clone())
}

/// The registered `ORDER BY` column for a sort field, matching the read
/// model's join aliases (`i` issues, `s` state, `t` team, `ua` assignee).
/// Exhaustive over [`SortField`], so a new sort field fails compilation here
/// until mapped.
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
    use lt_types::scalars::Priority;

    use super::*;

    #[test]
    fn no_filters_returns_empty_clause() {
        let (conditions, params, term) = build_sql_filter(&IssueFilter::default());
        assert!(conditions.is_empty());
        assert_eq!(params.len(), 0);
        assert!(term.is_none());
    }

    #[test]
    fn team_filter() {
        let filter = IssueFilter {
            team: Some("backend".to_string()),
            ..Default::default()
        };
        let (conditions, params, _) = build_sql_filter(&filter);
        assert_eq!(conditions, vec![sql::FRAG_TEAM_LOWER_OR_ID]);
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn no_assignee_filter() {
        let filter = IssueFilter {
            assignee: Some(AssigneeFilter::IsNull),
            ..Default::default()
        };
        let (conditions, params, _) = build_sql_filter(&filter);
        assert_eq!(conditions, vec![sql::FRAG_NO_ASSIGNEE]);
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn assignee_contains_filter() {
        let filter = IssueFilter {
            assignee: Some(AssigneeFilter::Contains("alice".to_string())),
            ..Default::default()
        };
        let (conditions, params, _) = build_sql_filter(&filter);
        assert_eq!(conditions, vec![sql::FRAG_ASSIGNEE_LOWER_LIKE]);
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn assignee_exact_filter() {
        let filter = IssueFilter {
            assignee: Some(AssigneeFilter::Exact("Alice".to_string())),
            ..Default::default()
        };
        let (conditions, params, _) = build_sql_filter(&filter);
        assert_eq!(conditions, vec![sql::FRAG_ASSIGNEE_EQ]);
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn state_filter() {
        let filter = IssueFilter {
            state: Some("in progress".to_string()),
            ..Default::default()
        };
        let (conditions, params, _) = build_sql_filter(&filter);
        assert_eq!(conditions, vec![sql::FRAG_STATE_LOWER_LIKE]);
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn priority_filter() {
        let filter = IssueFilter {
            priority: Some(Priority(2)),
            ..Default::default()
        };
        let (conditions, params, _) = build_sql_filter(&filter);
        assert_eq!(conditions, vec![sql::FRAG_PRIORITY_EQ]);
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn title_filter() {
        let filter = IssueFilter {
            title: Some("crash".to_string()),
            ..Default::default()
        };
        let (conditions, params, _) = build_sql_filter(&filter);
        assert_eq!(conditions, vec![sql::FRAG_TITLE_LIKE]);
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn date_filter_created_after() {
        let filter = IssueFilter {
            created_after: Some("2025-01-01T00:00:00Z".to_string()),
            ..Default::default()
        };
        let (conditions, params, _) = build_sql_filter(&filter);
        assert_eq!(conditions, vec![sql::FRAG_CREATED_AFTER]);
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn label_project_cycle_creator_filters() {
        let bases = [
            (
                IssueFilter {
                    label: Some("backend".to_string()),
                    ..Default::default()
                },
                sql::FRAG_LABEL_EXISTS,
            ),
            (
                IssueFilter {
                    project: Some("platform".to_string()),
                    ..Default::default()
                },
                sql::FRAG_PROJECT_LOWER_LIKE,
            ),
            (
                IssueFilter {
                    cycle: Some("cycle 7".to_string()),
                    ..Default::default()
                },
                sql::FRAG_CYCLE_LOWER_LIKE,
            ),
            (
                IssueFilter {
                    creator: Some("carol".to_string()),
                    ..Default::default()
                },
                sql::FRAG_CREATOR_LOWER_LIKE,
            ),
        ];
        for (filter, frag) in bases {
            let (conditions, params, _) = build_sql_filter(&filter);
            assert_eq!(conditions, vec![frag]);
            assert_eq!(params.len(), 1);
        }
    }

    #[test]
    fn term_carries_through_for_fts() {
        let filter = IssueFilter {
            term: Some("oauth* crash*".to_string()),
            ..Default::default()
        };
        let (conditions, params, term) = build_sql_filter(&filter);
        assert!(conditions.is_empty());
        assert!(params.is_empty());
        assert_eq!(term.as_deref(), Some("oauth* crash*"));
    }

    #[test]
    fn multiple_filters_joined_with_and() {
        let filter = IssueFilter {
            state: Some("todo".to_string()),
            title: Some("bug".to_string()),
            ..Default::default()
        };
        let (conditions, params, _) = build_sql_filter(&filter);
        assert_eq!(
            conditions,
            vec![sql::FRAG_STATE_LOWER_LIKE, sql::FRAG_TITLE_LIKE]
        );
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn order_default() {
        assert_eq!(sort_column(&SortField::Updated), sql::SORT_UPDATED_AT);
    }

    #[test]
    fn order_title() {
        assert_eq!(sort_column(&SortField::Title), sql::SORT_TITLE);
    }

    #[test]
    fn order_priority() {
        assert_eq!(sort_column(&SortField::Priority), sql::SORT_PRIORITY_LABEL);
    }

    #[test]
    fn order_assignee() {
        assert_eq!(sort_column(&SortField::Assignee), sql::SORT_ASSIGNEE_NAME);
    }
}
