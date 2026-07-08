//! Shared helpers for GraphQL-schema-driven codegen.
//!
//! Called from `lt-upstream/build.rs` and `lt-storage/build.rs`, which are
//! thin drivers: resolve paths, load the field specs, call the codegen
//! functions here, write the results to `OUT_DIR`.

// Build scripts report failure by panicking, which is idiomatic and cannot
// propagate a `Result`; this crate exists only to be called from build
// scripts, so the same exemption applies here.
#![allow(clippy::panic)]

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

pub mod affinity;
pub mod classify;
pub mod emit_ddl;
pub mod emit_grammar;
pub mod emit_sql;
pub mod ref_fragment;
pub mod schema_model;
pub mod search_fields;
pub mod selection_model;
#[cfg(test)]
pub(crate) mod test_fixtures;

#[derive(Debug)]
pub struct SortFieldSpec {
    /// Sort key as typed by the user after "sort:" (e.g. "updated").
    pub key: String,
    /// Field name inside `IssueSortInput`.
    pub gql_field: String,
    /// SQLite column name used in ORDER BY clauses. Not read by either build
    /// script: `lt-storage/src/db/filters.rs::sort_column` maps sort fields
    /// to registered `SortCol` consts by hand (type-safe-sql-adr.md), and
    /// this field documents that mapping for humans.
    pub sql_col: String,
}

#[derive(Debug)]
pub struct FieldSpec {
    /// Stem key as typed by the user (e.g. "assignee").
    pub key: String,
    /// Field name inside `IssueFilter`.
    pub gql_field: String,
    /// Expected base GraphQL type name.
    pub gql_type: String,
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

// ---------------------------------------------------------------------------
// Code generation (quote-based) -- sort field
// ---------------------------------------------------------------------------

/// Generate the `SortField` enum with `label()`, `from_key()`, and `next()`.
///
/// Variants are in spec order.  `label()` returns the user-facing key string.
/// `next()` cycles through variants in order, wrapping around.
// Build-script-only helper: `validate_sort_fields` already asserted the list
// is non-empty before this is called, so these `.expect()`s cannot fire.
#[allow(clippy::expect_used)]
pub fn gen_sort_field_enum(sort_fields: &[SortFieldSpec]) -> TokenStream {
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
        /// Generated from `lt_schema_codegen::search_fields::sort_fields()` by
        /// build.rs (bd-2w5). Do not edit by hand.
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
pub fn gen_build_sort(sort_fields: &[SortFieldSpec]) -> TokenStream {
    let match_arms = sort_field_arms(sort_fields, |f| &f.gql_field);

    quote! {
        /// Build the Linear GraphQL `sort` argument JSON for the given field and direction.
        ///
        /// Generated from `lt_schema_codegen::search_fields::sort_fields()` by
        /// build.rs (bd-2w5). Do not edit by hand.
        pub fn build_sort(field: &SortField, desc: bool) -> serde_json::Value {
            let order = if desc { "Descending" } else { "Ascending" };
            let field_key = match field {
                #( #match_arms )*
            };
            serde_json::json!([{ field_key: { "order": order } }])
        }
    }
}

/// Generate `parse_sort_value(value: &str) -> Option<(SortField, SortDirection)>`.
///
/// Strips an optional '+' or '-' suffix, then matches the field name.
pub fn gen_parse_sort_value(sort_fields: &[SortFieldSpec]) -> TokenStream {
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
        fn parse_sort_value(value: &str) -> Option<(SortField, SortDirection)> {
            let (field_str, dir) = if let Some(s) = value.strip_suffix('-') {
                (s, SortDirection::Descending)
            } else if let Some(s) = value.strip_suffix('+') {
                (s, SortDirection::Ascending)
            } else {
                (value, SortDirection::Ascending)
            };

            let field = match field_str.to_lowercase().as_str() {
                #( #match_arms )*
                _ => return None,
            };

            Some((field, dir))
        }
    }
}

// ---------------------------------------------------------------------------
// Code generation (quote-based) -- filter stems
// ---------------------------------------------------------------------------

/// `PascalCase` enum-variant idents, one per field spec (in order).
fn field_variants(fields: &[FieldSpec]) -> Vec<proc_macro2::Ident> {
    fields
        .iter()
        .map(|f| format_ident!("{}", to_pascal_case(&f.key)))
        .collect()
}

/// Generate the `StemKey` enum.
///
/// One variant per field spec (in order) plus the hard-coded Sort variant.
pub fn gen_stem_key_enum(fields: &[FieldSpec]) -> TokenStream {
    let variants = field_variants(fields);

    quote! {
        /// The key side of a stem token (used for completion context).
        #[derive(Debug, Clone, PartialEq)]
        pub enum StemKey {
            Sort,
            #( #variants, )*
        }
    }
}

/// Generate the `StemKind` enum.
///
/// Sort carries (field, dir); every field spec carries a String value.
pub fn gen_stem_kind_enum(fields: &[FieldSpec]) -> TokenStream {
    let variants = field_variants(fields);

    quote! {
        /// The fully-parsed meaning of a recognised stem.
        #[derive(Debug, Clone, PartialEq)]
        pub enum StemKind {
            Sort { field: SortField, dir: SortDirection },
            #( #variants { value: String }, )*
        }
    }
}

/// Generate `parse_query_ast_impl(raw: &str) -> (Vec<Token>, Vec<ParseError>)`.
///
/// The function uses a Chumsky 0.9 parser to tokenise the raw query string.
/// Chumsky handles the low-level character matching and provides spans via
/// `map_with_span`.  The semantic classification of stems (key -> `StemKind`)
/// is done in a post-parse step that also emits `ParseErrors` for unknown keys.
// The generated `parse_query_ast_impl` is one dispatch arm per allowlist
// entry plus a hand-rolled tokeniser, so this generator function is long by
// construction; that is the only allow here.
#[allow(clippy::too_many_lines)]
pub fn gen_parser_fn(fields: &[FieldSpec]) -> TokenStream {
    // Build the list of string literals for all known keys: ["sort", "assignee", ...]
    let key_strs: Vec<proc_macro2::Literal> = {
        let mut v = vec![proc_macro2::Literal::string("sort")];
        for f in fields {
            v.push(proc_macro2::Literal::string(&f.key));
        }
        v
    };

    // Build match arms for each field spec.
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
        /// Levenshtein edit distance between two strings, over chars.
        fn edit_dist(a: &str, b: &str) -> usize {
            let av: Vec<char> = a.chars().collect();
            let bv: Vec<char> = b.chars().collect();
            let m = av.len();
            let n = bv.len();
            if m == 0 { return n; }
            if n == 0 { return m; }
            let mut dp = vec![vec![0usize; n + 1]; m + 1];
            for (i, row) in dp.iter_mut().enumerate() { row[0] = i; }
            for (j, cell) in dp[0].iter_mut().enumerate() { *cell = j; }
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

        /// Closest known key within edit distance 2 of `unknown`, for "did you
        /// mean?" suggestions.
        fn closest_key<'k>(unknown: &str, keys: &[&'k str]) -> Option<&'k str> {
            let mut best: Option<(&'k str, usize)> = None;
            for &k in keys {
                let d = edit_dist(unknown, k);
                if d <= 2 && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((k, d));
                }
            }
            best.map(|(k, _)| k)
        }

        /// Parse a raw query string into tokens and errors using a Chumsky 0.13 parser.
        ///
        /// The Chumsky parser handles character-level tokenisation and provides byte
        /// span information via `map_with`.  Semantic classification of each
        /// token (key -> `StemKind`) is done in a second pass that emits `ParseError`s
        /// for unknown stem keys with optional "did you mean?" suggestions.
        // One dispatch arm is generated per allowlist entry, so this function
        // exceeds the line budget by construction; that is the only allow here.
        #[allow(clippy::too_many_lines)]
        fn parse_query_ast_impl(raw: &str) -> (Vec<Token>, Vec<ParseError>) {
            use chumsky::prelude::*;
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

            type RawTok = (String, Option<String>, std::ops::Range<usize>);

            // The parser is defined inside a nested fn so the input/error lifetime
            // (`'src`) is declared once and inferred through the whole grammar.
            fn token_parser<'src>(
            ) -> impl Parser<'src, &'src [char], Vec<RawTok>, extra::Err<Simple<'src, char>>>
            {
                let ws = any::<&'src [char], extra::Err<Simple<'src, char>>>()
                    .filter(|c: &char| c.is_ascii_whitespace())
                    .repeated();
                let non_ws = any::<&'src [char], extra::Err<Simple<'src, char>>>()
                    .filter(|c: &char| !c.is_ascii_whitespace());
                let non_ws_no_colon = any::<&'src [char], extra::Err<Simple<'src, char>>>()
                    .filter(|c: &char| !c.is_ascii_whitespace() && *c != ':');

                let key_chars = non_ws_no_colon
                    .repeated()
                    .at_least(1)
                    .collect::<String>();

                let val_chars = non_ws.repeated().collect::<String>();

                // stem: key ':' val  -> (key, Some(val))
                let stem = key_chars
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
                // map_with gives us the byte span (Range<usize>) of each token.
                let raw_tok = stem.or(word).map_with(|(k, v), e| {
                    let span = e.span();
                    (k, v, span.start..span.end)
                });

                ws.ignore_then(raw_tok.then_ignore(ws).repeated().collect::<Vec<_>>())
                    .then_ignore(end())
            }

            let chars: Vec<char> = raw.chars().collect();

            // On parse failure (should never happen for this grammar), fall back
            // to an empty token list rather than panicking.
            let raw_toks: Vec<RawTok> = token_parser()
                .parse(chars.as_slice())
                .into_result()
                .unwrap_or_default();

            // Step 2: classify each raw token into a typed Token variant.
            //
            // Chumsky span indices are char indices.  For ASCII query strings,
            // char index == byte index, so we use them directly as byte offsets.

            let known_keys: &[&str] = &[ #( #key_strs, )* ];

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
                                        "unknown filter key '{key_or_word}' -- did you mean '{s}'?"
                                    ),
                                    None => format!("unknown filter key '{key_or_word}'"),
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
// Shared emitter helpers (`emit_ddl`, `emit_sql`)
// ---------------------------------------------------------------------------

/// The generated table name for a GraphQL object type: `WorkflowState` ->
/// `workflow_states`.
pub(crate) fn table_name(graphql_type: &str) -> String {
    format!("{}s", classify::to_snake_case(graphql_type))
}

/// A `pub const <PREFIX>_<NAME>: &str = <sql>;` declaration -- the shared
/// tail every SQL-emitting generator (`emit_ddl`, `emit_sql`) produces, kept
/// in one place so the ident-and-declare step is not duplicated per emitter.
pub(crate) fn const_str_item(prefix: &str, name: &str, sql: &str) -> TokenStream {
    let ident = format_ident!("{}_{}", prefix, name.to_uppercase());
    quote! { pub const #ident: &str = #sql; }
}

// ---------------------------------------------------------------------------
// Pretty-printing
// ---------------------------------------------------------------------------

/// Parse a generated `TokenStream` and pretty-print it via `prettyplease`,
/// prefixing the given file-header comment. Shared by both build scripts so
/// each generated file goes through the same formatting step.
pub fn format_generated(header: &str, tokens: TokenStream) -> String {
    let tree = syn::parse2(tokens)
        .unwrap_or_else(|e| panic!("failed to parse generated TokenStream: {e}"));
    format!("{header}{}", prettyplease::unparse(&tree))
}

#[cfg(test)]
mod tests {
    use super::to_pascal_case;

    #[test]
    fn to_pascal_case_converts_snake_and_kebab_case() {
        assert_eq!(to_pascal_case("updated_at"), "UpdatedAt");
        assert_eq!(to_pascal_case("foo-bar"), "FooBar");
        assert_eq!(to_pascal_case("title"), "Title");
    }
}
