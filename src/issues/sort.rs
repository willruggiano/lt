use serde_json::{Value, json};

use super::SortField;

pub fn build_sort(field: &SortField, desc: bool) -> Value {
    let order = if desc { "Descending" } else { "Ascending" };
    let field_key = match field {
        SortField::Created => "createdAt",
        SortField::Updated => "updatedAt",
        SortField::Priority => "priority",
        SortField::Title => "title",
        SortField::Assignee => "assignee",
        SortField::State => "workflowState",
        SortField::Team => "team",
    };
    json!([{ field_key: { "order": order } }])
}
