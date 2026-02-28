// build.rs
//
// Phase 1 (bd-3mw): validate the allowlist against the GraphQL schema.
// Phase 2 (bd-1pl): generate search_stems.rs from the validated allowlist.
// Phase 3 (bd-117): rewrite code generation with quote + prettyplease.
// Phase 4 (bd-2w5): generate SortField enum and sort helpers from IssueSortInput.
//
// cargo:rerun-if-changed directives ensure the build script re-runs whenever
// the schema or the allowlist changes.

use std::{collections::HashMap, env, fs, path::Path};

use graphql_parser::schema::{Definition, Document, TypeDefinition};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Allowlist config types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AllowlistConfig {
    field: Vec<FieldSpec>,
    #[serde(default)]
    sort_field: Vec<SortFieldSpec>,
}

#[derive(Debug, Deserialize)]
struct FieldSpec {
    /// Stem key as typed by the user (e.g. "assignee").
    key: String,
    /// Field name inside IssueFilter (schema-validated).
    gql_field: String,
    /// Expected base GraphQL type name (schema-validated).
    gql_type: String,
}

#[derive(Debug, Deserialize)]
struct SortFieldSpec {
    /// Sort key as typed by the user after "sort:" (e.g. "updated").
    key: String,
    /// Field name inside IssueSortInput (schema-validated).
    gql_field: String,
    /// SQLite column name used in ORDER BY clauses.
    sql_col: String,
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

/// Parse the GraphQL schema and return the set of field names in IssueSortInput.
fn extract_issue_sort_input_fields(schema_src: &str) -> HashMap<String, String> {
    let doc: Document<String> = graphql_parser::parse_schema(schema_src)
        .unwrap_or_else(|e| panic!("Failed to parse GraphQL schema: {e}"));

    for def in &doc.definitions {
        if let Definition::TypeDefinition(TypeDefinition::InputObject(input)) = def {
            if input.name == "IssueSortInput" {
                let mut map = HashMap::new();
                for field in &input.fields {
                    let base = base_type_name(&field.value_type).to_string();
                    map.insert(field.name.clone(), base);
                }
                return map;
            }
        }
    }

    panic!("IssueSortInput input type not found in the GraphQL schema");
}

/// Convert a lowercase key to PascalCase for use as an enum variant name.
fn to_pascal_case(s: &str) -> String {
    s.split(|c: char| c == '_' || c == '-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Code generation (quote-based) -- filter stems
// ---------------------------------------------------------------------------

/// Generate the StemKey enum.
///
/// One variant per TOML field (in order) plus the hard-coded Sort variant.
fn gen_stem_key_enum(fields: &[FieldSpec]) -> TokenStream {
    let variants: Vec<proc_macro2::Ident> = fields
        .iter()
        .map(|f| format_ident!("{}", to_pascal_case(&f.key)))
        .collect();

    quote! {
        /// The key side of a stem token (used for completion context).
        #[derive(Debug, Clone, PartialEq)]
        pub enum StemKey {
            Sort,
            #( #variants, )*
        }
    }
}

/// Generate the StemKind enum.
///
/// Sort carries (field, dir); every TOML field carries a String value.
fn gen_stem_kind_enum(fields: &[FieldSpec]) -> TokenStream {
    let variants: Vec<proc_macro2::Ident> = fields
        .iter()
        .map(|f| format_ident!("{}", to_pascal_case(&f.key)))
        .collect();

    quote! {
        /// The fully-parsed meaning of a recognised stem.
        #[derive(Debug, Clone, PartialEq)]
        pub enum StemKind {
            Sort { field: SortField, dir: SortDir },
            #( #variants { value: String }, )*
        }
    }
}

/// Generate `impl From<&QueryAst> for ParsedQuery`.
///
/// Emits one match arm per TOML field.  The `sort:` arm is hard-coded.
fn gen_from_ast(fields: &[FieldSpec]) -> TokenStream {
    let variants: Vec<proc_macro2::Ident> = fields
        .iter()
        .map(|f| format_ident!("{}", to_pascal_case(&f.key)))
        .collect();

    let field_idents: Vec<proc_macro2::Ident> =
        fields.iter().map(|f| format_ident!("{}", f.key)).collect();

    // let mut <field>: Option<String> = None; declarations
    let field_decls = field_idents.iter().map(|id| {
        quote! { let mut #id: Option<String> = None; }
    });

    // StemKind::<Variant> { value } => { <field> = Some(value.clone()); }
    let stem_arms = variants
        .iter()
        .zip(field_idents.iter())
        .map(|(variant, field_id)| {
            quote! {
                StemKind::#variant { value } => {
                    #field_id = Some(value.clone());
                }
            }
        });

    // ParsedQuery { <field>, ... } struct literal fields
    let struct_fields = field_idents.iter().map(|id| {
        quote! { #id, }
    });

    quote! {
        impl From<&QueryAst> for ParsedQuery {
            /// Derive a SQL-ready `ParsedQuery` from the AST.
            ///
            /// Generated from the TOML allowlist by build.rs (bd-1pl).
            /// One match arm per allowlist entry.  sort: is hard-coded.
            fn from(ast: &QueryAst) -> Self {
                let mut sort: Option<(SortField, SortDir)> = None;
                #( #field_decls )*
                let mut fts_words: Vec<String> = Vec::new();

                for token in &ast.tokens {
                    match token {
                        Token::Stem { kind, .. } => match kind {
                            StemKind::Sort { field, dir } => {
                                sort = Some((field.clone(), dir.clone()));
                            }
                            #( #stem_arms )*
                        },
                        Token::PartialStem { .. } => {}
                        Token::Word { text, .. } => {
                            fts_words.push(format!("{}*", text));
                        }
                        Token::Unknown { raw: raw_slice, .. } => {
                            if !raw_slice.is_empty() {
                                fts_words.push(format!("{}*", raw_slice));
                            }
                        }
                    }
                }

                ParsedQuery {
                    sort,
                    #( #struct_fields )*
                    fts_terms: fts_words.join(" "),
                }
            }
        }
    }
}

/// Generate `parse_query_ast_impl(raw: &str) -> (Vec<Token>, Vec<ParseError>)`.
///
/// The function uses a Chumsky 0.9 parser to tokenise the raw query string.
/// Chumsky handles the low-level character matching and provides spans via
/// `map_with_span`.  The semantic classification of stems (key -> StemKind)
/// is done in a post-parse step that also emits ParseErrors for unknown keys.
fn gen_parser_fn(fields: &[FieldSpec]) -> TokenStream {
    // Build the list of string literals for all known keys: ["sort", "assignee", ...]
    let key_strs: Vec<proc_macro2::Literal> = {
        let mut v = vec![proc_macro2::Literal::string("sort")];
        for f in fields {
            v.push(proc_macro2::Literal::string(&f.key));
        }
        v
    };

    // Build match arms for each TOML field.
    let toml_arms = fields.iter().map(|f| {
        let key_str = &f.key;
        let variant = format_ident!("{}", to_pascal_case(key_str));
        quote! {
            #key_str => {
                if val.is_empty() {
                    (Token::PartialStem {
                        span: Span { start: tok_start, end: tok_end },
                        key_span: Span { start: tok_start, end: colon_pos },
                        val_span: Span { start: colon_pos + 1, end: tok_end },
                        known_key: Some(StemKey::#variant),
                    }, None)
                } else {
                    (Token::Stem {
                        span: Span { start: tok_start, end: tok_end },
                        key_span: Span { start: tok_start, end: colon_pos },
                        val_span: Span { start: colon_pos + 1, end: tok_end },
                        kind: StemKind::#variant { value: val.to_lowercase() },
                    }, None)
                }
            }
        }
    });

    quote! {
        /// Parse a raw query string into tokens and errors using a Chumsky 0.9 parser.
        ///
        /// The Chumsky parser handles character-level tokenisation and provides byte
        /// span information via `map_with_span`.  Semantic classification of each
        /// token (key -> StemKind) is done in a second pass that emits ParseErrors
        /// for unknown stem keys with optional "did you mean?" suggestions.
        fn parse_query_ast_impl(raw: &str) -> (Vec<Token>, Vec<ParseError>) {
            use chumsky::prelude::{Parser, filter, just, end};
            use chumsky::error::Simple;

            // Step 1: Chumsky tokeniser.
            //
            // Grammar (over char stream):
            //   query     = (ws* raw_tok)* ws* end
            //   raw_tok   = stem | word
            //   stem      = key_chars ':' val_chars  -> (key, Some(val))
            //   word      = non_ws+                  -> (word, None)
            //   key_chars = (non-ws and non-colon)+
            //   val_chars = non-ws*
            //   ws        = ASCII whitespace

            let ws = filter::<char, _, Simple<char>>(|c: &char| c.is_ascii_whitespace())
                .repeated();
            let non_ws = filter::<char, _, Simple<char>>(|c: &char| !c.is_ascii_whitespace());
            let non_ws_no_colon = filter::<char, _, Simple<char>>(|c: &char| {
                !c.is_ascii_whitespace() && *c != ':'
            });

            let key_chars = non_ws_no_colon
                .repeated()
                .at_least(1)
                .collect::<String>();

            let val_chars = non_ws.repeated().collect::<String>();

            // stem: key ':' val  -> (key, Some(val))
            let stem = key_chars
                .clone()
                .then_ignore(just(':'))
                .then(val_chars)
                .map(|(k, v)| (k, Some(v)));

            // word: non_ws+ -> (word, None)
            let word = non_ws
                .repeated()
                .at_least(1)
                .collect::<String>()
                .map(|w| (w, None::<String>));

            // Try stem before word so "key:" is a stem with empty value.
            // map_with_span gives us the byte span (Range<usize>) of each token.
            let raw_tok = stem
                .or(word)
                .map_with_span(|(k, v), span: std::ops::Range<usize>| (k, v, span));

            let parser = ws
                .clone()
                .ignore_then(raw_tok.then_ignore(ws.clone()).repeated())
                .then_ignore(end());

            let chars: Vec<char> = raw.chars().collect();

            // On parse failure (should never happen for this grammar), fall back
            // to an empty token list rather than panicking.
            let raw_toks: Vec<(String, Option<String>, std::ops::Range<usize>)> =
                match parser.parse(chars.as_slice()) {
                    Ok(toks) => toks,
                    Err(_errs) => Vec::new(),
                };

            // Step 2: classify each raw token into a typed Token variant.
            //
            // Chumsky span indices are char indices.  For ASCII query strings,
            // char index == byte index, so we use them directly as byte offsets.

            let known_keys: &[&str] = &[ #( #key_strs, )* ];

            fn edit_dist(a: &str, b: &str) -> usize {
                let av: Vec<char> = a.chars().collect();
                let bv: Vec<char> = b.chars().collect();
                let m = av.len();
                let n = bv.len();
                if m == 0 { return n; }
                if n == 0 { return m; }
                let mut dp = vec![vec![0usize; n + 1]; m + 1];
                for i in 0..=m { dp[i][0] = i; }
                for j in 0..=n { dp[0][j] = j; }
                for i in 1..=m {
                    for j in 1..=n {
                        dp[i][j] = if av[i - 1] == bv[j - 1] {
                            dp[i - 1][j - 1]
                        } else {
                            1 + dp[i - 1][j].min(dp[i][j - 1]).min(dp[i - 1][j - 1])
                        };
                    }
                }
                dp[m][n]
            }

            fn closest_key<'k>(unknown: &str, keys: &[&'k str]) -> Option<&'k str> {
                let mut best: Option<(&'k str, usize)> = None;
                for &k in keys {
                    let d = edit_dist(unknown, k);
                    if d <= 2 {
                        if best.map_or(true, |(_, bd)| d < bd) {
                            best = Some((k, d));
                        }
                    }
                }
                best.map(|(k, _)| k)
            }

            let mut tokens: Vec<Token> = Vec::new();
            let mut errors: Vec<ParseError> = Vec::new();

            for (key_or_word, maybe_val, span) in raw_toks {
                let tok_start = span.start;
                let tok_end = span.end;

                match maybe_val {
                    None => {
                        // Bare word -- no colon.
                        tokens.push(Token::Word {
                            span: Span { start: tok_start, end: tok_end },
                            text: key_or_word,
                        });
                    }
                    Some(val) => {
                        // key:val -- colon at tok_start + key_or_word.len().
                        let colon_pos = tok_start + key_or_word.len();
                        let key_lower = key_or_word.to_lowercase();

                        let (tok, err): (Token, Option<ParseError>) = match key_lower.as_str() {
                            "sort" => {
                                if let Some((field, dir)) = parse_sort_value(&val) {
                                    (Token::Stem {
                                        span: Span { start: tok_start, end: tok_end },
                                        key_span: Span { start: tok_start, end: colon_pos },
                                        val_span: Span { start: colon_pos + 1, end: tok_end },
                                        kind: StemKind::Sort { field, dir },
                                    }, None)
                                } else {
                                    (Token::PartialStem {
                                        span: Span { start: tok_start, end: tok_end },
                                        key_span: Span { start: tok_start, end: colon_pos },
                                        val_span: Span { start: colon_pos + 1, end: tok_end },
                                        known_key: Some(StemKey::Sort),
                                    }, None)
                                }
                            }
                            #( #toml_arms )*
                            _ => {
                                let suggestion = closest_key(&key_lower, known_keys);
                                let message = match suggestion {
                                    Some(s) => format!(
                                        "unknown filter key '{}' -- did you mean '{}'?",
                                        key_or_word, s
                                    ),
                                    None => format!("unknown filter key '{}'", key_or_word),
                                };
                                let err = ParseError {
                                    span: Span { start: tok_start, end: colon_pos },
                                    message,
                                };
                                (Token::PartialStem {
                                    span: Span { start: tok_start, end: tok_end },
                                    key_span: Span { start: tok_start, end: colon_pos },
                                    val_span: Span { start: colon_pos + 1, end: tok_end },
                                    known_key: None,
                                }, Some(err))
                            }
                        };

                        tokens.push(tok);
                        if let Some(e) = err {
                            errors.push(e);
                        }
                    }
                }
            }

            (tokens, errors)
        }
    }
}

// ---------------------------------------------------------------------------
// Code generation (quote-based) -- sort field
// ---------------------------------------------------------------------------

/// Generate the SortField enum with label() and next() impls.
///
/// Variants are in TOML order.  label() returns the user-facing key string.
/// next() cycles through variants in order, wrapping around.
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
        /// Generated from [[sort_field]] entries in build/search_filter_fields.toml
        /// by build.rs (bd-2w5). Do not edit by hand.
        #[derive(Clone, Debug, PartialEq, clap::ValueEnum)]
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

            /// Cycle to the next sort field, wrapping around at the end.
            pub fn next(&self) -> Self {
                match self {
                    #( #next_arms )*
                    #wrap_arm
                }
            }
        }
    }
}

/// Generate `parse_sort_value(value: &str) -> Option<(SortField, SortDir)>`.
///
/// Strips an optional '+' or '-' suffix, then matches the field name.
fn gen_parse_sort_value(sort_fields: &[SortFieldSpec]) -> TokenStream {
    let match_arms = sort_fields.iter().map(|f| {
        let key_str = &f.key;
        let variant = format_ident!("{}", to_pascal_case(key_str));
        quote! {
            #key_str => SortField::#variant,
        }
    });

    // Build doc comment listing accepted forms.
    let doc_lines: Vec<String> = sort_fields
        .iter()
        .flat_map(|f| vec![format!("  `{0}-`   `{0}+`   `{0}`", f.key)])
        .collect();
    let doc_str = format!(
        "Parse the value portion of a `sort:` stem.\n\nAccepted forms:\n{}",
        doc_lines.join("\n")
    );

    quote! {
        #[doc = #doc_str]
        fn parse_sort_value(value: &str) -> Option<(SortField, SortDir)> {
            let (field_str, dir) = if let Some(s) = value.strip_suffix('-') {
                (s, SortDir::Desc)
            } else if let Some(s) = value.strip_suffix('+') {
                (s, SortDir::Asc)
            } else {
                (value, SortDir::Asc)
            };

            let field = match field_str.to_lowercase().as_str() {
                #( #match_arms )*
                _ => return None,
            };

            Some((field, dir))
        }
    }
}

/// Generate `sort_col(field: &SortField) -> &'static str`.
///
/// Maps each SortField variant to its SQLite column name.
fn gen_sort_col(sort_fields: &[SortFieldSpec]) -> TokenStream {
    let match_arms = sort_fields.iter().map(|f| {
        let variant = format_ident!("{}", to_pascal_case(&f.key));
        let col = &f.sql_col;
        quote! {
            SortField::#variant => #col,
        }
    });

    quote! {
        /// Map a sort field to the corresponding SQLite column name.
        ///
        /// Generated from [[sort_field]] entries in build/search_filter_fields.toml
        /// by build.rs (bd-2w5). Do not edit by hand.
        fn sort_col(field: &SortField) -> &'static str {
            match field {
                #( #match_arms )*
            }
        }
    }
}

/// Generate `build_sort(field: &SortField, desc: bool) -> serde_json::Value`.
///
/// Produces the JSON payload for the Linear GraphQL `sort` argument.
fn gen_build_sort(sort_fields: &[SortFieldSpec]) -> TokenStream {
    let match_arms = sort_fields.iter().map(|f| {
        let variant = format_ident!("{}", to_pascal_case(&f.key));
        let gql = &f.gql_field;
        quote! {
            SortField::#variant => #gql,
        }
    });

    quote! {
        /// Build the Linear GraphQL `sort` argument JSON for the given field and direction.
        ///
        /// Generated from [[sort_field]] entries in build/search_filter_fields.toml
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

    // Tell cargo to re-run this script when these files change.
    println!(
        "cargo:rerun-if-changed={}",
        manifest
            .join("docs/reference/linear-schema-definition.graphql")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("build/search_filter_fields.toml").display()
    );
    println!("cargo:rerun-if-changed=build.rs");

    // -----------------------------------------------------------------------
    // Load the allowlist
    // -----------------------------------------------------------------------
    let toml_path = manifest.join("build/search_filter_fields.toml");
    let toml_src = fs::read_to_string(&toml_path)
        .unwrap_or_else(|e| panic!("Cannot read allowlist at {}: {e}", toml_path.display()));
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
    let issue_sort_input_fields = extract_issue_sort_input_fields(&schema_src);

    // -----------------------------------------------------------------------
    // Validate every filter allowlist entry against the schema
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
    // Validate every sort_field entry against IssueSortInput in the schema
    // -----------------------------------------------------------------------
    let mut sort_validation_errors: Vec<String> = Vec::new();

    for spec in &config.sort_field {
        if !issue_sort_input_fields.contains_key(&spec.gql_field) {
            sort_validation_errors.push(format!(
                "  sort_field key '{}': field '{}' does not exist in IssueSortInput",
                spec.key, spec.gql_field
            ));
        }
    }

    if !sort_validation_errors.is_empty() {
        panic!(
            "build.rs: sort_field validation failed against IssueSortInput schema:\n{}\n\
             Fix [[sort_field]] entries in build/search_filter_fields.toml.",
            sort_validation_errors.join("\n")
        );
    }

    // Require at least one sort_field entry so the generated enum is non-empty.
    if config.sort_field.is_empty() {
        panic!("build.rs: [[sort_field]] list in search_filter_fields.toml is empty");
    }

    let sort_fields = &config.sort_field;

    // -----------------------------------------------------------------------
    // Generate sort_field.rs  (included in src/issues/mod.rs)
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
    let sort_field_src = format!("{}{}", sort_field_header, sort_field_formatted);

    let sort_field_path = Path::new(&out_dir).join("sort_field.rs");
    fs::write(&sort_field_path, &sort_field_src)
        .unwrap_or_else(|e| panic!("Cannot write {}: {e}", sort_field_path.display()));

    // -----------------------------------------------------------------------
    // Generate sort_build.rs  (included in src/issues/sort.rs)
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
    let sort_build_src = format!("{}{}", sort_build_header, sort_build_formatted);

    let sort_build_path = Path::new(&out_dir).join("sort_build.rs");
    fs::write(&sort_build_path, &sort_build_src)
        .unwrap_or_else(|e| panic!("Cannot write {}: {e}", sort_build_path.display()));

    // -----------------------------------------------------------------------
    // Generate search_stems.rs
    // -----------------------------------------------------------------------
    let fields = &config.field;

    let stem_key_enum = gen_stem_key_enum(fields);
    let stem_kind_enum = gen_stem_kind_enum(fields);
    let parse_sort_value_fn = gen_parse_sort_value(sort_fields);
    let sort_col_fn = gen_sort_col(sort_fields);
    let parser_fn = gen_parser_fn(fields);
    let from_ast_impl = gen_from_ast(fields);

    // Combine all fragments into a single TokenStream.
    // parse_sort_value and sort_col must come before parser_fn which calls them.
    let combined: TokenStream = quote! {
        #stem_key_enum
        #stem_kind_enum
        #parse_sort_value_fn
        #sort_col_fn
        #parser_fn
        #from_ast_impl
    };

    // Parse and pretty-print via prettyplease.
    let syntax_tree = syn::parse2(combined)
        .unwrap_or_else(|e| panic!("build.rs: failed to parse generated TokenStream: {e}"));
    let formatted = prettyplease::unparse(&syntax_tree);

    let file_header = "// search_stems.rs -- generated by build.rs (bd-117, bd-2w5)\n\
                       // DO NOT EDIT -- regenerate by running `cargo build`\n\n";
    let out_src = format!("{}{}", file_header, formatted);

    let stems_path = Path::new(&out_dir).join("search_stems.rs");
    fs::write(&stems_path, &out_src)
        .unwrap_or_else(|e| panic!("Cannot write {}: {e}", stems_path.display()));
}
