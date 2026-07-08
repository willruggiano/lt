//! `CREATE TABLE` DDL emission from a classified fragment.
//!
//! Storage-only scaffolding -- `synced_at`, the FTS5 shadow index and its
//! triggers, `op_log`, `sync_meta` -- is not fragment-derived; it is a
//! hand-written DDL fragment concatenated alongside the tables this module
//! generates.

use std::collections::BTreeSet;

use proc_macro2::TokenStream;
use quote::quote;

use crate::affinity::affinity;
use crate::classify::{FieldRole, classify_fragment, to_snake_case};
use crate::schema_model::Schema;
use crate::selection_model::Fragment;
use crate::{const_str_item, table_name};

/// Emit `fragment`'s entity table, plus one junction table per
/// `*Connection` field, as `pub const` SQL string declarations.
pub fn emit_create_table(
    fragment: &Fragment,
    schema: &Schema,
    generated_types: &BTreeSet<&str>,
) -> TokenStream {
    let roles = classify_fragment(fragment, schema, generated_types);
    let table = table_name(&fragment.graphql_type);

    let mut columns = vec!["id TEXT PRIMARY KEY".to_string()];
    let mut junction_items = Vec::new();

    for (field, role) in fragment.fields.iter().zip(roles.iter()) {
        match role {
            FieldRole::PrimaryKey => {}
            FieldRole::ScalarColumn {
                column, not_null, ..
            } => {
                let aff = affinity(&field.base_type);
                let suffix = if *not_null { " NOT NULL" } else { "" };
                columns.push(format!("{column} {aff}{suffix}"));
            }
            FieldRole::ForeignKey {
                column,
                target_type,
                not_null,
            } => {
                let target_table = table_name(target_type);
                let suffix = if *not_null { " NOT NULL" } else { "" };
                columns.push(format!(
                    "{column} TEXT{suffix} REFERENCES {target_table}(id) ON UPDATE CASCADE"
                ));
            }
            FieldRole::Junction {
                connection_type, ..
            } => {
                junction_items.push(emit_junction_table(
                    &fragment.graphql_type,
                    &table,
                    connection_type,
                    schema,
                ));
            }
        }
    }

    let sql = format!("CREATE TABLE {table} ({})", columns.join(", "));
    let table_item = const_str_item("CREATE_TABLE", &table, &sql);

    quote! {
        #table_item
        #( #junction_items )*
    }
}

/// The junction table for one `*Connection` field, reproducing the
/// `issue_labels`/`labels` shape in `crates/lt-storage/src/db/mod.rs`: the
/// node type's name is stripped of the owning type's name as a prefix to
/// recover the referenced entity (`IssueLabel` on `Issue` -> `Label`), which
/// names both the junction table (`issue_labels`) and the node-side FK
/// (`label_id` -> `labels(id)`). Unlike the main entity table's FKs, junction
/// FKs carry `ON DELETE CASCADE`: a link row has no meaning once either
/// endpoint is gone.
fn emit_junction_table(
    owner_type: &str,
    owner_table: &str,
    connection_type: &str,
    schema: &Schema,
) -> TokenStream {
    let connection = schema
        .object(connection_type)
        .unwrap_or_else(|| panic!("schema has no object type `{connection_type}`"));
    let node_type = connection
        .fields()
        .find_map(|(name, ty)| (name == "nodes").then(|| ty.to_string()))
        .unwrap_or_else(|| panic!("connection type `{connection_type}` has no `nodes` field"));

    let node_entity = node_type
        .strip_prefix(owner_type)
        .unwrap_or(node_type.as_str());
    let owner_snake = to_snake_case(owner_type);
    let node_table = table_name(node_entity);
    let junction_table = format!("{owner_snake}_{node_table}");
    let source_column = format!("{owner_snake}_id");
    let node_column = format!("{}_id", to_snake_case(node_entity));

    let sql = format!(
        "CREATE TABLE {junction_table} (\
         {source_column} TEXT NOT NULL REFERENCES {owner_table}(id) ON UPDATE CASCADE ON DELETE CASCADE, \
         {node_column} TEXT NOT NULL REFERENCES {node_table}(id) ON UPDATE CASCADE ON DELETE CASCADE, \
         PRIMARY KEY ({source_column}, {node_column}))"
    );

    const_str_item("CREATE_TABLE", &junction_table, &sql)
}

#[cfg(test)]
mod tests {
    use super::emit_create_table;
    use crate::schema_model::Schema;
    use crate::selection_model::parse_fragments;
    use crate::test_fixtures::{ISSUE_FRAGMENT_SRC, ISSUE_SDL, issue_generated_types};

    fn issue_fragment() -> crate::selection_model::Fragment {
        parse_fragments(ISSUE_FRAGMENT_SRC)
            .expect("fragment source parses")
            .into_iter()
            .find(|f| f.rust_name == "Issue")
            .expect("Issue fragment present")
    }

    // The `issues` `CREATE TABLE` (+ its `issue_labels` junction table).
    //
    // Two divergences from `crates/lt-storage/src/db/mod.rs`'s `MIGRATION_5`
    // are expected and owner-confirmed, both visible below:
    //  - `priority INTEGER` is new: `Issue.priority` is a fragment field
    //    MIGRATION_5 never carried.
    //  - `identifier`/`title` are `NOT NULL` here (non-`Option` scalars);
    //    MIGRATION_5 left them nullable for skeleton rows, a path `IssueRef`
    //    retires.
    #[test]
    fn issues_create_table_snapshot() {
        let schema = Schema::parse(ISSUE_SDL).expect("SDL parses");
        let generated_types = issue_generated_types();
        let tokens = emit_create_table(&issue_fragment(), &schema, &generated_types);
        let pretty = crate::format_generated("", tokens);
        insta::assert_snapshot!(pretty);
    }
}
