//! Hand-maintained specs for the search-bar grammar (filter stems and sort
//! keys), read by `lt-upstream/build.rs` and `lt-storage/build.rs`.
//!
//! Previously sourced from `build/search_filter_fields.toml` and validated at
//! build time against `IssueFilter`/`IssueSortInput` in the GraphQL schema
//! snapshot; that validation is gone, so keeping these lists in sync with the
//! schema is now a manual review step.

use crate::{FieldSpec, SortFieldSpec};

/// Build a `Vec<T>` from `(key, b, c)` string-literal triples, shared by
/// [`filter_fields`] and [`sort_fields`] so the two spec lists are just data.
fn specs<T>(entries: &[(&str, &str, &str)], make: impl Fn(String, String, String) -> T) -> Vec<T> {
    entries
        .iter()
        .map(|&(key, b, c)| make(key.to_string(), b.to_string(), c.to_string()))
        .collect()
}

/// The `IssueFilter` fields exposed as filter stems in the search bar.
pub fn filter_fields() -> Vec<FieldSpec> {
    specs(
        &[
            ("assignee", "assignee", "NullableUserFilter"),
            ("priority", "priority", "NullableNumberComparator"),
            ("state", "state", "WorkflowStateFilter"),
            ("team", "team", "TeamFilter"),
            ("label", "labels", "IssueLabelCollectionFilter"),
            ("project", "project", "NullableProjectFilter"),
            ("cycle", "cycle", "NullableCycleFilter"),
            ("creator", "creator", "NullableUserFilter"),
        ],
        |key, gql_field, gql_type| FieldSpec {
            key,
            gql_field,
            gql_type,
        },
    )
}

/// The `IssueSortInput` fields exposed as `sort:` values in the search bar.
pub fn sort_fields() -> Vec<SortFieldSpec> {
    specs(
        &[
            ("updated", "updatedAt", "i.updated_at"),
            ("created", "createdAt", "i.created_at"),
            ("priority", "priority", "i.priority_label"),
            ("title", "title", "i.title"),
            ("assignee", "assignee", "ua.name"),
            ("state", "workflowState", "s.name"),
            ("team", "team", "t.name"),
        ],
        |key, gql_field, sql_col| SortFieldSpec {
            key,
            gql_field,
            sql_col,
        },
    )
}
