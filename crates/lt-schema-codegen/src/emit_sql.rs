//! Static per-entity CRUD statement emission over a fragment's storage
//! columns: `id` plus its scalar and foreign-key columns. Junction
//! (`*Connection`) fields carry no column and are excluded, as is the
//! composed filter/`WHERE` query (a runtime selection, not a static
//! statement).

use std::collections::BTreeSet;

use proc_macro2::TokenStream;

use crate::classify::{FieldRole, classify_fragment};
use crate::schema_model::Schema;
use crate::selection_model::Fragment;
use crate::{const_str_item, table_name};

/// `fragment`'s storage columns -- `id` plus every scalar/foreign-key column,
/// in fragment field order.
fn storage_columns(
    fragment: &Fragment,
    schema: &Schema,
    generated_types: &BTreeSet<&str>,
) -> Vec<String> {
    let roles = classify_fragment(fragment, schema, generated_types);
    let mut columns = vec!["id".to_string()];
    for role in &roles {
        match role {
            FieldRole::PrimaryKey | FieldRole::Junction { .. } => {}
            FieldRole::ScalarColumn { column, .. } | FieldRole::ForeignKey { column, .. } => {
                columns.push(column.clone());
            }
        }
    }
    columns
}

fn table_and_columns(
    fragment: &Fragment,
    schema: &Schema,
    generated_types: &BTreeSet<&str>,
) -> (String, Vec<String>) {
    (
        table_name(&fragment.graphql_type),
        storage_columns(fragment, schema, generated_types),
    )
}

/// Emit an upsert-on-`id` insert: `INSERT INTO <table> (<cols>) VALUES
/// (?1, ...) ON CONFLICT(id) DO UPDATE SET <col>=excluded.<col>, ...`,
/// reproducing `UPSERT_ISSUE`'s shape (`crates/lt-storage/src/db/sql.rs`).
pub fn emit_upsert(
    fragment: &Fragment,
    schema: &Schema,
    generated_types: &BTreeSet<&str>,
) -> TokenStream {
    let (table, columns) = table_and_columns(fragment, schema, generated_types);

    let placeholders: Vec<String> = (1..=columns.len()).map(|n| format!("?{n}")).collect();
    let set_clause: Vec<String> = columns
        .iter()
        .skip(1)
        .map(|c| format!("{c} = excluded.{c}"))
        .collect();

    let sql = format!(
        "INSERT INTO {table} ({}) VALUES ({}) ON CONFLICT(id) DO UPDATE SET {}",
        columns.join(", "),
        placeholders.join(", "),
        set_clause.join(", ")
    );

    const_str_item("UPSERT", &table, &sql)
}

/// Emit `UPDATE <table> SET <col>=?, ... WHERE id=?`, over every column but
/// `id`.
pub fn emit_update(
    fragment: &Fragment,
    schema: &Schema,
    generated_types: &BTreeSet<&str>,
) -> TokenStream {
    let (table, columns) = table_and_columns(fragment, schema, generated_types);
    let non_id = &columns[1..];

    let set_clause: Vec<String> = non_id
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{c} = ?{}", i + 1))
        .collect();
    let id_placeholder = non_id.len() + 1;

    let sql = format!(
        "UPDATE {table} SET {} WHERE id = ?{id_placeholder}",
        set_clause.join(", ")
    );

    const_str_item("UPDATE", &table, &sql)
}

/// Emit `DELETE FROM <table> WHERE id=?`.
pub fn emit_delete(fragment: &Fragment) -> TokenStream {
    let table = table_name(&fragment.graphql_type);
    let sql = format!("DELETE FROM {table} WHERE id = ?1");
    const_str_item("DELETE", &table, &sql)
}

/// Emit `SELECT <cols> FROM <table> WHERE id=?`, the simple by-id read (the
/// composed filter/`WHERE` query is out of scope).
pub fn emit_select(
    fragment: &Fragment,
    schema: &Schema,
    generated_types: &BTreeSet<&str>,
) -> TokenStream {
    let (table, columns) = table_and_columns(fragment, schema, generated_types);
    let sql = format!("SELECT {} FROM {table} WHERE id = ?1", columns.join(", "));
    const_str_item("SELECT", &table, &sql)
}

#[cfg(test)]
mod tests {
    use super::{emit_delete, emit_select, emit_update, emit_upsert};
    use crate::schema_model::Schema;
    use crate::selection_model::{Fragment, parse_fragments};
    use crate::test_fixtures::{ISSUE_FRAGMENT_SRC, ISSUE_SDL, issue_generated_types};

    fn issue_fragment() -> Fragment {
        parse_fragments(ISSUE_FRAGMENT_SRC)
            .expect("fragment source parses")
            .into_iter()
            .find(|f| f.rust_name == "Issue")
            .expect("Issue fragment present")
    }

    #[test]
    fn issues_upsert_snapshot() {
        let schema = Schema::parse(ISSUE_SDL).expect("SDL parses");
        let tokens = emit_upsert(&issue_fragment(), &schema, &issue_generated_types());
        insta::assert_snapshot!(crate::format_generated("", tokens));
    }

    #[test]
    fn issues_update_snapshot() {
        let schema = Schema::parse(ISSUE_SDL).expect("SDL parses");
        let tokens = emit_update(&issue_fragment(), &schema, &issue_generated_types());
        insta::assert_snapshot!(crate::format_generated("", tokens));
    }

    #[test]
    fn issues_delete_snapshot() {
        let tokens = emit_delete(&issue_fragment());
        insta::assert_snapshot!(crate::format_generated("", tokens));
    }

    #[test]
    fn issues_select_snapshot() {
        let schema = Schema::parse(ISSUE_SDL).expect("SDL parses");
        let tokens = emit_select(&issue_fragment(), &schema, &issue_generated_types());
        insta::assert_snapshot!(crate::format_generated("", tokens));
    }
}
