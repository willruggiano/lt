//! Shared helpers for the GraphQL-schema-driven sort/search codegen, used as a
//! build dependency by `lt-types/build.rs` and `lt-storage/build.rs`.

// Build scripts report failure by panicking, which is idiomatic and cannot
// propagate a `Result`; this crate exists only to be called from build
// scripts, so the same exemption applies here.
#![allow(clippy::panic)]

use std::collections::HashMap;

use graphql_parser::schema::{Definition, Document, TypeDefinition};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct SortFieldSpec {
    /// Sort key as typed by the user after "sort:" (e.g. "updated").
    pub key: String,
    /// Field name inside `IssueSortInput` (schema-validated).
    pub gql_field: String,
    /// SQLite column name used in ORDER BY clauses. Not read by either build
    /// script: `lt-storage/src/db/filters.rs::sort_column` maps sort fields
    /// to registered `SortCol` consts by hand (type-safe-sql-adr.md), and
    /// this field is kept only so the TOML documents that mapping for humans.
    pub sql_col: String,
}

/// Recursively unwrap NonNull/List wrappers and return the base named type.
fn base_type_name<'a>(ty: &'a graphql_parser::schema::Type<'a, String>) -> &'a str {
    use graphql_parser::schema::Type;
    match ty {
        Type::NamedType(name) => name.as_str(),
        Type::NonNullType(inner) | Type::ListType(inner) => base_type_name(inner),
    }
}

/// Parse the GraphQL schema and return a map of `input_type`'s field names to
/// their base type names.
pub fn extract_input_object_fields(schema_src: &str, input_type: &str) -> HashMap<String, String> {
    let doc: Document<String> = graphql_parser::parse_schema(schema_src)
        .unwrap_or_else(|e| panic!("Failed to parse GraphQL schema: {e}"));

    for def in &doc.definitions {
        if let Definition::TypeDefinition(TypeDefinition::InputObject(input)) = def
            && input.name == input_type
        {
            let mut map = HashMap::new();
            for field in &input.fields {
                let base = base_type_name(&field.value_type).to_string();
                map.insert(field.name.clone(), base);
            }
            return map;
        }
    }

    panic!("{input_type} input type not found in the GraphQL schema");
}

/// Validate every `[[sort_field]]` entry against `IssueSortInput` in the schema
/// and require at least one entry so the generated `SortField` enum is
/// non-empty. Panics (build-script convention) on any mismatch.
// Internal build-time helper with exactly two callers, both passing the map
// returned by `extract_input_object_fields` (default-hasher `HashMap`);
// generalizing over `BuildHasher` adds no real value here.
#[allow(clippy::implicit_hasher)]
pub fn validate_sort_fields(
    sort_fields: &[SortFieldSpec],
    issue_sort_input_fields: &HashMap<String, String>,
) {
    let mut errors: Vec<String> = Vec::new();
    for spec in sort_fields {
        if !issue_sort_input_fields.contains_key(&spec.gql_field) {
            errors.push(format!(
                "  sort_field key '{}': field '{}' does not exist in IssueSortInput",
                spec.key, spec.gql_field
            ));
        }
    }

    assert!(
        errors.is_empty(),
        "build.rs: sort_field validation failed against IssueSortInput schema:\n{}\n\
         Fix [[sort_field]] entries in build/search_filter_fields.toml.",
        errors.join("\n")
    );

    assert!(
        !sort_fields.is_empty(),
        "build.rs: [[sort_field]] list in search_filter_fields.toml is empty"
    );
}

/// Convert a lowercase key to `PascalCase` for use as an enum variant name.
pub fn to_pascal_case(s: &str) -> String {
    s.split(['_', '-'])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{SortFieldSpec, extract_input_object_fields, to_pascal_case, validate_sort_fields};

    #[test]
    fn to_pascal_case_converts_snake_and_kebab_case() {
        assert_eq!(to_pascal_case("updated_at"), "UpdatedAt");
        assert_eq!(to_pascal_case("foo-bar"), "FooBar");
        assert_eq!(to_pascal_case("title"), "Title");
    }

    const SCHEMA: &str = r"
        input IssueSortInput {
            createdAt: DateTime
            title: [String!]!
        }
    ";

    #[test]
    fn extract_input_object_fields_unwraps_nonnull_and_list() {
        let fields = extract_input_object_fields(SCHEMA, "IssueSortInput");
        assert_eq!(
            fields.get("createdAt").map(String::as_str),
            Some("DateTime")
        );
        assert_eq!(fields.get("title").map(String::as_str), Some("String"));
    }

    fn spec(key: &str, gql_field: &str) -> SortFieldSpec {
        SortFieldSpec {
            key: key.to_string(),
            gql_field: gql_field.to_string(),
            sql_col: "col".to_string(),
        }
    }

    #[test]
    fn validate_sort_fields_accepts_known_fields() {
        let fields = extract_input_object_fields(SCHEMA, "IssueSortInput");
        validate_sort_fields(&[spec("updated", "createdAt")], &fields);
    }

    #[test]
    #[should_panic(expected = "does not exist in IssueSortInput")]
    fn validate_sort_fields_panics_on_unknown_gql_field() {
        let fields = extract_input_object_fields(SCHEMA, "IssueSortInput");
        validate_sort_fields(&[spec("bogus", "nope")], &fields);
    }
}
