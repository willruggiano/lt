// Build scripts report failure by panicking, which is idiomatic and cannot
// propagate a Result; the crate-wide panic-safety lints do not apply here.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::doc_markdown
)]

use std::path::Path;
use std::{env, fs};

use lt_schema_codegen::{
    SortFieldSpec, extract_input_object_fields, to_pascal_case, validate_sort_fields,
};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Allowlist config types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AllowlistConfig {
    #[serde(default)]
    sort_field: Vec<SortFieldSpec>,
}

// ---------------------------------------------------------------------------
// Code generation (quote-based) -- sort field
// ---------------------------------------------------------------------------

/// Generate the `SortField` enum with `label()`, `from_key()`, and `next()`.
///
/// Variants are in TOML order.  `label()` returns the user-facing key string.
/// `next()` cycles through variants in order, wrapping around.
fn gen_sort_field_enum(sort_fields: &[SortFieldSpec]) -> TokenStream {
    let variants: Vec<proc_macro2::Ident> = sort_fields
        .iter()
        .map(|f| format_ident!("{}", to_pascal_case(&f.key)))
        .collect();

    let label_arms = sort_fields.iter().map(|f| {
        let variant = format_ident!("{}", to_pascal_case(&f.key));
        let key_str = &f.key;
        quote! {
            SortField::#variant => #key_str,
        }
    });

    let from_key_arms = sort_fields.iter().map(|f| {
        let variant = format_ident!("{}", to_pascal_case(&f.key));
        let key_str = &f.key;
        quote! {
            #key_str => Some(SortField::#variant),
        }
    });

    // next(): each variant maps to the next one in order, last wraps to first.
    let next_arms = variants.windows(2).map(|w| {
        let cur = &w[0];
        let nxt = &w[1];
        quote! {
            SortField::#cur => SortField::#nxt,
        }
    });
    // Last variant wraps to first.
    let last_variant = variants.last().expect("sort_field list must not be empty");
    let first_variant = variants.first().expect("sort_field list must not be empty");
    let wrap_arm = quote! {
        SortField::#last_variant => SortField::#first_variant,
    };

    quote! {
        /// A sort field for the issues list.
        ///
        /// Generated from `[[sort_field]]` entries in `build/search_filter_fields.toml`
        /// by build.rs (bd-2w5). Do not edit by hand.
        ///
        /// Kept clap-free so the data layer carries no CLI-parsing dependency;
        /// the `lt-cli` argument parser maps strings via [`SortField::from_key`].
        #[derive(Clone, Debug, PartialEq)]
        pub enum SortField {
            #( #variants, )*
        }

        impl SortField {
            /// The user-facing sort key string (as typed after "sort:").
            pub fn label(&self) -> &'static str {
                match self {
                    #( #label_arms )*
                }
            }

            /// Parse a sort key string (as typed after "sort:") into a variant.
            pub fn from_key(key: &str) -> Option<Self> {
                match key {
                    #( #from_key_arms )*
                    _ => None,
                }
            }

            /// Cycle to the next sort field, wrapping around at the end.
            #[must_use]
            pub fn next(&self) -> Self {
                match self {
                    #( #next_arms )*
                    #wrap_arm
                }
            }
        }
    }
}

/// `SortField::<Variant> => <value>,` match arms, one per sort field, where
/// `value` selects the per-field string literal to map onto.
///
/// Only `lt-types/build.rs` calls this (for `gen_build_sort`'s `gql_field`
/// arms): `lt-storage/build.rs` used to call it too (for a generated
/// `sort_col()`), but that mapping is now hand-written in
/// `lt-storage/src/db/filters.rs::sort_column` (type-safe-sql-adr.md), so
/// this stays local rather than in the shared `schema_codegen.rs`.
fn sort_field_arms(
    sort_fields: &[SortFieldSpec],
    value: impl Fn(&SortFieldSpec) -> &str,
) -> Vec<TokenStream> {
    sort_fields
        .iter()
        .map(|f| {
            let variant = format_ident!("{}", to_pascal_case(&f.key));
            let value = value(f);
            quote! {
                SortField::#variant => #value,
            }
        })
        .collect()
}

/// Generate `build_sort(field: &SortField, desc: bool) -> serde_json::Value`.
///
/// Produces the JSON payload for the Linear GraphQL `sort` argument.
fn gen_build_sort(sort_fields: &[SortFieldSpec]) -> TokenStream {
    let match_arms = sort_field_arms(sort_fields, |f| &f.gql_field);

    quote! {
        /// Build the Linear GraphQL `sort` argument JSON for the given field and direction.
        ///
        /// Generated from `[[sort_field]]` entries in `build/search_filter_fields.toml`
        /// by build.rs (bd-2w5). Do not edit by hand.
        pub fn build_sort(field: &SortField, desc: bool) -> serde_json::Value {
            let order = if desc { "Descending" } else { "Ascending" };
            let field_key = match field {
                #( #match_arms )*
            };
            serde_json::json!([{ field_key: { "order": order } }])
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let manifest = Path::new(&manifest_dir);
    let schema_path = manifest.join("../../build/linear-schema-definition.graphql");
    let toml_path = manifest.join("../../build/search_filter_fields.toml");

    // Tell cargo to re-run this script when these files change.
    println!("cargo:rerun-if-changed={}", schema_path.display());
    println!("cargo:rerun-if-changed={}", toml_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    // Make the snapshot available to `#[cynic::schema("linear")]` and the
    // `QueryFragment` derives, which read it from `$OUT_DIR/cynic-schemas`.
    cynic_codegen::register_schema("linear")
        .from_sdl_file(&schema_path)
        .expect("registering Linear schema with cynic")
        .as_default()
        .expect("setting cynic default schema");

    // -----------------------------------------------------------------------
    // Load the allowlist
    // -----------------------------------------------------------------------
    let toml_src = fs::read_to_string(&toml_path)
        .unwrap_or_else(|e| panic!("Cannot read allowlist at {}: {e}", toml_path.display()));
    let config: AllowlistConfig = toml::from_str(&toml_src).unwrap_or_else(|e| {
        panic!(
            "Failed to parse allowlist TOML at {}: {e}",
            toml_path.display()
        )
    });

    // -----------------------------------------------------------------------
    // Load and parse the GraphQL schema, validate sort fields
    // -----------------------------------------------------------------------
    let schema_src = fs::read_to_string(&schema_path).unwrap_or_else(|e| {
        panic!(
            "Cannot read GraphQL schema at {}: {e}",
            schema_path.display()
        )
    });
    let issue_sort_input_fields = extract_input_object_fields(&schema_src, "IssueSortInput");

    validate_sort_fields(&config.sort_field, &issue_sort_input_fields);

    let sort_fields = &config.sort_field;

    // -----------------------------------------------------------------------
    // Generate sort_field.rs  (included in src/query.rs)
    // -----------------------------------------------------------------------
    let sort_field_enum = gen_sort_field_enum(sort_fields);

    let sort_field_combined: TokenStream = quote! {
        #sort_field_enum
    };

    let sort_field_tree = syn::parse2(sort_field_combined)
        .unwrap_or_else(|e| panic!("build.rs: failed to parse sort_field TokenStream: {e}"));
    let sort_field_formatted = prettyplease::unparse(&sort_field_tree);

    let sort_field_header = "// sort_field.rs -- generated by build.rs (bd-2w5)\n\
                             // DO NOT EDIT -- regenerate by running `cargo build`\n\n";
    let sort_field_src = format!("{sort_field_header}{sort_field_formatted}");

    let sort_field_path = Path::new(&out_dir).join("sort_field.rs");
    fs::write(&sort_field_path, &sort_field_src)
        .unwrap_or_else(|e| panic!("Cannot write {}: {e}", sort_field_path.display()));

    // -----------------------------------------------------------------------
    // Generate sort_build.rs  (included in src/query.rs)
    // -----------------------------------------------------------------------
    let build_sort_fn = gen_build_sort(sort_fields);

    let sort_build_combined: TokenStream = quote! {
        #build_sort_fn
    };

    let sort_build_tree = syn::parse2(sort_build_combined)
        .unwrap_or_else(|e| panic!("build.rs: failed to parse sort_build TokenStream: {e}"));
    let sort_build_formatted = prettyplease::unparse(&sort_build_tree);

    let sort_build_header = "// sort_build.rs -- generated by build.rs (bd-2w5)\n\
                             // DO NOT EDIT -- regenerate by running `cargo build`\n\n";
    let sort_build_src = format!("{sort_build_header}{sort_build_formatted}");

    let sort_build_path = Path::new(&out_dir).join("sort_build.rs");
    fs::write(&sort_build_path, &sort_build_src)
        .unwrap_or_else(|e| panic!("Cannot write {}: {e}", sort_build_path.display()));
}
