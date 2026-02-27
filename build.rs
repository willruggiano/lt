// build.rs
//
// Phase 1 (bd-3mw): validate the allowlist against the GraphQL schema.
// Phase 2 (bd-1pl): generate search_stems.rs from the validated allowlist.
//
// cargo:rerun-if-changed directives ensure the build script re-runs whenever
// the schema or the allowlist changes.

use std::{collections::HashMap, env, fs, path::Path};

use graphql_parser::schema::{Definition, Document, TypeDefinition};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Allowlist config types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AllowlistConfig {
    field: Vec<FieldSpec>,
}

#[derive(Debug, Deserialize)]
struct FieldSpec {
    /// Stem key as typed by the user (e.g. "assignee").
    key: String,
    /// Field name inside IssueFilter (schema-validated).
    gql_field: String,
    /// Expected base GraphQL type name (schema-validated).
    gql_type: String,
    /// Human-readable value placeholder for error messages.
    #[allow(dead_code)]
    value_hint: String,
    /// SQLite column to match against.
    #[allow(dead_code)]
    sql_col: String,
    /// SQL operator: "LIKE" or "=".
    #[allow(dead_code)]
    sql_op: String,
    /// Whether to wrap both sides in LOWER().
    #[allow(dead_code)]
    sql_lower: bool,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Recursively unwrap NonNull/List wrappers and return the base named type.
fn base_type_name<'a>(ty: &'a graphql_parser::schema::Type<'a, String>) -> &'a str {
    use graphql_parser::schema::Type;
    match ty {
        Type::NamedType(name) => name.as_str(),
        Type::NonNullType(inner) => base_type_name(inner),
        Type::ListType(inner) => base_type_name(inner),
    }
}

/// Parse the GraphQL schema and return a map of IssueFilter field names to
/// their base type names.
fn extract_issue_filter_fields(schema_src: &str) -> HashMap<String, String> {
    let doc: Document<String> = graphql_parser::parse_schema(schema_src)
        .unwrap_or_else(|e| panic!("Failed to parse GraphQL schema: {e}"));

    for def in &doc.definitions {
        if let Definition::TypeDefinition(TypeDefinition::InputObject(input)) = def {
            if input.name == "IssueFilter" {
                let mut map = HashMap::new();
                for field in &input.fields {
                    let base = base_type_name(&field.value_type).to_string();
                    map.insert(field.name.clone(), base);
                }
                return map;
            }
        }
    }

    panic!("IssueFilter input type not found in the GraphQL schema");
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let manifest = Path::new(&manifest_dir);

    // Tell cargo to re-run this script when these files change.
    println!(
        "cargo:rerun-if-changed={}",
        manifest
            .join("docs/reference/linear-schema-definition.graphql")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest
            .join("build/search_filter_fields.toml")
            .display()
    );
    println!("cargo:rerun-if-changed=build.rs");

    // -----------------------------------------------------------------------
    // Load the allowlist
    // -----------------------------------------------------------------------
    let toml_path = manifest.join("build/search_filter_fields.toml");
    let toml_src = fs::read_to_string(&toml_path).unwrap_or_else(|e| {
        panic!(
            "Cannot read allowlist at {}: {e}",
            toml_path.display()
        )
    });
    let config: AllowlistConfig = toml::from_str(&toml_src).unwrap_or_else(|e| {
        panic!(
            "Failed to parse allowlist TOML at {}: {e}",
            toml_path.display()
        )
    });

    // -----------------------------------------------------------------------
    // Load and parse the GraphQL schema
    // -----------------------------------------------------------------------
    let schema_path = manifest.join("docs/reference/linear-schema-definition.graphql");
    let schema_src = fs::read_to_string(&schema_path).unwrap_or_else(|e| {
        panic!(
            "Cannot read GraphQL schema at {}: {e}",
            schema_path.display()
        )
    });
    let issue_filter_fields = extract_issue_filter_fields(&schema_src);

    // -----------------------------------------------------------------------
    // Validate every allowlist entry against the schema
    // -----------------------------------------------------------------------
    let mut validation_errors: Vec<String> = Vec::new();

    for spec in &config.field {
        match issue_filter_fields.get(&spec.gql_field) {
            None => {
                validation_errors.push(format!(
                    "  allowlist key '{}': field '{}' does not exist in IssueFilter",
                    spec.key, spec.gql_field
                ));
            }
            Some(actual_type) => {
                if actual_type != &spec.gql_type {
                    validation_errors.push(format!(
                        "  allowlist key '{}': field '{}' has type '{}' in the schema \
                         but the allowlist declares '{}'",
                        spec.key, spec.gql_field, actual_type, spec.gql_type
                    ));
                }
            }
        }
    }

    if !validation_errors.is_empty() {
        panic!(
            "build.rs: allowlist validation failed against IssueFilter schema:\n{}\n\
             Fix build/search_filter_fields.toml or update the GraphQL schema snapshot.",
            validation_errors.join("\n")
        );
    }

    // -----------------------------------------------------------------------
    // Emit search_stems.rs
    //
    // The full code-generation step is implemented in the next bead (bd-1pl).
    // For now we emit a placeholder so that any include!() in search_query.rs
    // compiles without error.
    // -----------------------------------------------------------------------
    let stems_path = Path::new(&out_dir).join("search_stems.rs");
    fs::write(
        &stems_path,
        "// search_stems.rs -- generated by build.rs (bd-3mw placeholder)\n\
         // Full generation is implemented in bd-1pl.\n",
    )
    .unwrap_or_else(|e| panic!("Cannot write {}: {e}", stems_path.display()));
}
