// Shared build-script helpers for the GraphQL-schema-driven sort/search codegen,
// `include!`d by both `lt-types/build.rs` (SortField/build_sort) and
// `lt-storage/build.rs` (search stems): SortFieldSpec, base_type_name,
// extract_input_object_fields, validate_sort_fields, to_pascal_case. Kept in
// one file so the two build scripts do not carry duplicate copies of the
// schema-parsing and PascalCase helpers.
//
// The including build script provides the `use` items these depend on
// (`HashMap`, `graphql_parser` types, `serde::Deserialize`, `proc_macro2`,
// `quote`).

#[derive(Debug, Deserialize)]
struct SortFieldSpec {
    /// Sort key as typed by the user after "sort:" (e.g. "updated").
    key: String,
    /// Field name inside `IssueSortInput` (schema-validated).
    gql_field: String,
    /// SQLite column name used in ORDER BY clauses. Not read by either build
    /// script: `lt-storage/src/db/filters.rs::sort_column` maps sort fields
    /// to registered `SortCol` consts by hand (type-safe-sql-adr.md), and
    /// this field is kept only so the TOML documents that mapping for humans.
    #[allow(dead_code)]
    sql_col: String,
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
fn extract_input_object_fields(schema_src: &str, input_type: &str) -> HashMap<String, String> {
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
fn validate_sort_fields(
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
fn to_pascal_case(s: &str) -> String {
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
