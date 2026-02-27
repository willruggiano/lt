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

/// Convert a lowercase key to PascalCase for use as an enum variant name.
fn to_pascal_case(s: &str) -> String {
    s.split(|c: char| c == '_' || c == '-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                }
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Code generation
// ---------------------------------------------------------------------------

/// Generate the StemKey enum.
///
/// One variant per TOML field (in order) plus the hard-coded Sort variant.
fn gen_stem_key_enum(fields: &[FieldSpec]) -> String {
    let mut s = String::new();
    s.push_str("/// The key side of a stem token (used for completion context).\n");
    s.push_str("#[derive(Debug, Clone, PartialEq)]\n");
    s.push_str("pub enum StemKey {\n");
    s.push_str("    Sort,\n");
    for f in fields {
        s.push_str(&format!("    {},\n", to_pascal_case(&f.key)));
    }
    s.push_str("}\n");
    s
}

/// Generate the StemKind enum.
///
/// Sort carries (field, dir); every TOML field carries a String value.
fn gen_stem_kind_enum(fields: &[FieldSpec]) -> String {
    let mut s = String::new();
    s.push_str("/// The fully-parsed meaning of a recognised stem.\n");
    s.push_str("#[derive(Debug, Clone, PartialEq)]\n");
    s.push_str("pub enum StemKind {\n");
    s.push_str("    Sort { field: SortField, dir: SortDir },\n");
    for f in fields {
        s.push_str(&format!(
            "    {} {{ value: String }},\n",
            to_pascal_case(&f.key)
        ));
    }
    s.push_str("}\n");
    s
}

/// Generate `parse_query_ast_impl(raw: &str) -> (Vec<Token>, Vec<ParseError>)`.
///
/// The function uses a Chumsky 0.9 parser to tokenise the raw query string.
/// Chumsky handles the low-level character matching and provides spans via
/// `map_with_span`.  The semantic classification of stems (key -> StemKind)
/// is done in a post-parse step that also emits ParseErrors for unknown keys.
///
/// Using push_str instead of format!() avoids escaping every {{ and }} in
/// the generated Rust code.
fn gen_parser_fn(fields: &[FieldSpec]) -> String {
    // Build the quoted string list of all known stem keys for "did you mean?".
    let key_list: Vec<String> = {
        let mut v = vec!["\"sort\"".to_string()];
        for f in fields {
            v.push(format!("\"{}\"", f.key));
        }
        v
    };
    let known_keys_array = format!("[{}]", key_list.join(", "));

    // Build match arms for each TOML field (key -> Token classification).
    let mut toml_match_arms = String::new();
    for f in fields {
        let variant = to_pascal_case(&f.key);
        let key = &f.key;
        // Each arm is like:
        //   "assignee" => {
        //       if val.is_empty() { (Token::PartialStem {...}, None) }
        //       else { (Token::Stem { ..., kind: StemKind::Assignee { value: val.to_lowercase() } }, None) }
        //   }
        toml_match_arms.push_str(&format!(
            concat!(
                "                    \"{key}\" => {{\n",
                "                        if val.is_empty() {{\n",
                "                            (Token::PartialStem {{\n",
                "                                span: Span {{ start: tok_start, end: tok_end }},\n",
                "                                key_span: Span {{ start: tok_start, end: colon_pos }},\n",
                "                                val_span: Span {{ start: colon_pos + 1, end: tok_end }},\n",
                "                                known_key: Some(StemKey::{variant}),\n",
                "                            }}, None)\n",
                "                        }} else {{\n",
                "                            (Token::Stem {{\n",
                "                                span: Span {{ start: tok_start, end: tok_end }},\n",
                "                                key_span: Span {{ start: tok_start, end: colon_pos }},\n",
                "                                val_span: Span {{ start: colon_pos + 1, end: tok_end }},\n",
                "                                kind: StemKind::{variant} {{ value: val.to_lowercase() }},\n",
                "                            }}, None)\n",
                "                        }}\n",
                "                    }}\n",
            ),
            key = key,
            variant = variant,
        ));
    }

    let mut s = String::new();

    // Function header and doc comment.
    s.push_str("\n");
    s.push_str("/// Parse a raw query string into tokens and errors using a Chumsky 0.9 parser.\n");
    s.push_str("///\n");
    s.push_str("/// The Chumsky parser handles character-level tokenisation and provides byte\n");
    s.push_str("/// span information via `map_with_span`.  Semantic classification of each\n");
    s.push_str("/// token (key -> StemKind) is done in a second pass that emits ParseErrors\n");
    s.push_str("/// for unknown stem keys with optional \"did you mean?\" suggestions.\n");
    s.push_str("fn parse_query_ast_impl(raw: &str) -> (Vec<Token>, Vec<ParseError>) {\n");

    // Imports inside the function (they are module-scoped at include! site so
    // function-level use is fine too).
    s.push_str("    use chumsky::prelude::{Parser, filter, just, end};\n");
    s.push_str("    use chumsky::error::Simple;\n");
    s.push_str("\n");

    // Chumsky parser construction.
    s.push_str("    // Step 1: Chumsky tokeniser.\n");
    s.push_str("    //\n");
    s.push_str("    // Grammar (over char stream):\n");
    s.push_str("    //   query     = (ws* raw_tok)* ws* end\n");
    s.push_str("    //   raw_tok   = stem | word\n");
    s.push_str("    //   stem      = key_chars ':' val_chars  -> (key, Some(val))\n");
    s.push_str("    //   word      = non_ws+                  -> (word, None)\n");
    s.push_str("    //   key_chars = (non-ws and non-colon)+\n");
    s.push_str("    //   val_chars = non-ws*\n");
    s.push_str("    //   ws        = ASCII whitespace\n");
    s.push_str("\n");
    s.push_str("    let ws = filter::<char, _, Simple<char>>(|c: &char| c.is_ascii_whitespace())\n");
    s.push_str("        .repeated();\n");
    s.push_str("    let non_ws = filter::<char, _, Simple<char>>(|c: &char| !c.is_ascii_whitespace());\n");
    s.push_str("    let non_ws_no_colon = filter::<char, _, Simple<char>>(|c: &char| {\n");
    s.push_str("        !c.is_ascii_whitespace() && *c != ':'\n");
    s.push_str("    });\n");
    s.push_str("\n");
    s.push_str("    let key_chars = non_ws_no_colon\n");
    s.push_str("        .repeated()\n");
    s.push_str("        .at_least(1)\n");
    s.push_str("        .collect::<String>();\n");
    s.push_str("\n");
    s.push_str("    let val_chars = non_ws.repeated().collect::<String>();\n");
    s.push_str("\n");
    s.push_str("    // stem: key ':' val  -> (key, Some(val))\n");
    s.push_str("    let stem = key_chars\n");
    s.push_str("        .clone()\n");
    s.push_str("        .then_ignore(just(':'))\n");
    s.push_str("        .then(val_chars)\n");
    s.push_str("        .map(|(k, v)| (k, Some(v)));\n");
    s.push_str("\n");
    s.push_str("    // word: non_ws+ -> (word, None)\n");
    s.push_str("    let word = non_ws\n");
    s.push_str("        .repeated()\n");
    s.push_str("        .at_least(1)\n");
    s.push_str("        .collect::<String>()\n");
    s.push_str("        .map(|w| (w, None::<String>));\n");
    s.push_str("\n");
    s.push_str("    // Try stem before word so \"key:\" is a stem with empty value.\n");
    s.push_str("    // map_with_span gives us the byte span (Range<usize>) of each token.\n");
    s.push_str("    let raw_tok = stem\n");
    s.push_str("        .or(word)\n");
    s.push_str("        .map_with_span(|(k, v), span: std::ops::Range<usize>| (k, v, span));\n");
    s.push_str("\n");
    s.push_str("    let parser = ws\n");
    s.push_str("        .clone()\n");
    s.push_str("        .ignore_then(raw_tok.then_ignore(ws.clone()).repeated())\n");
    s.push_str("        .then_ignore(end());\n");
    s.push_str("\n");
    s.push_str("    let chars: Vec<char> = raw.chars().collect();\n");
    s.push_str("\n");
    s.push_str("    // On parse failure (should never happen for this grammar), fall back\n");
    s.push_str("    // to an empty token list rather than panicking.\n");
    s.push_str("    let raw_toks: Vec<(String, Option<String>, std::ops::Range<usize>)> =\n");
    s.push_str("        match parser.parse(chars.as_slice()) {\n");
    s.push_str("            Ok(toks) => toks,\n");
    s.push_str("            Err(_errs) => Vec::new(),\n");
    s.push_str("        };\n");
    s.push_str("\n");

    // Step 2: semantic classification.
    s.push_str("    // Step 2: classify each raw token into a typed Token variant.\n");
    s.push_str("    //\n");
    s.push_str("    // Chumsky span indices are char indices.  For ASCII query strings,\n");
    s.push_str("    // char index == byte index, so we use them directly as byte offsets.\n");
    s.push_str("\n");

    // Known keys array (generated).
    s.push_str("    let known_keys: &[&str] = &");
    s.push_str(&known_keys_array);
    s.push_str(";\n");
    s.push_str("\n");

    // edit_dist helper.
    s.push_str("    fn edit_dist(a: &str, b: &str) -> usize {\n");
    s.push_str("        let av: Vec<char> = a.chars().collect();\n");
    s.push_str("        let bv: Vec<char> = b.chars().collect();\n");
    s.push_str("        let m = av.len();\n");
    s.push_str("        let n = bv.len();\n");
    s.push_str("        if m == 0 { return n; }\n");
    s.push_str("        if n == 0 { return m; }\n");
    s.push_str("        let mut dp = vec![vec![0usize; n + 1]; m + 1];\n");
    s.push_str("        for i in 0..=m { dp[i][0] = i; }\n");
    s.push_str("        for j in 0..=n { dp[0][j] = j; }\n");
    s.push_str("        for i in 1..=m {\n");
    s.push_str("            for j in 1..=n {\n");
    s.push_str("                dp[i][j] = if av[i - 1] == bv[j - 1] {\n");
    s.push_str("                    dp[i - 1][j - 1]\n");
    s.push_str("                } else {\n");
    s.push_str("                    1 + dp[i - 1][j].min(dp[i][j - 1]).min(dp[i - 1][j - 1])\n");
    s.push_str("                };\n");
    s.push_str("            }\n");
    s.push_str("        }\n");
    s.push_str("        dp[m][n]\n");
    s.push_str("    }\n");
    s.push_str("\n");

    // closest_key helper.
    s.push_str("    fn closest_key<'k>(unknown: &str, keys: &[&'k str]) -> Option<&'k str> {\n");
    s.push_str("        let mut best: Option<(&'k str, usize)> = None;\n");
    s.push_str("        for &k in keys {\n");
    s.push_str("            let d = edit_dist(unknown, k);\n");
    s.push_str("            if d <= 2 {\n");
    s.push_str("                if best.map_or(true, |(_, bd)| d < bd) {\n");
    s.push_str("                    best = Some((k, d));\n");
    s.push_str("                }\n");
    s.push_str("            }\n");
    s.push_str("        }\n");
    s.push_str("        best.map(|(k, _)| k)\n");
    s.push_str("    }\n");
    s.push_str("\n");

    // Main loop.
    s.push_str("    let mut tokens: Vec<Token> = Vec::new();\n");
    s.push_str("    let mut errors: Vec<ParseError> = Vec::new();\n");
    s.push_str("\n");
    s.push_str("    for (key_or_word, maybe_val, span) in raw_toks {\n");
    s.push_str("        let tok_start = span.start;\n");
    s.push_str("        let tok_end = span.end;\n");
    s.push_str("\n");
    s.push_str("        match maybe_val {\n");
    s.push_str("            None => {\n");
    s.push_str("                // Bare word -- no colon.\n");
    s.push_str("                tokens.push(Token::Word {\n");
    s.push_str("                    span: Span { start: tok_start, end: tok_end },\n");
    s.push_str("                    text: key_or_word,\n");
    s.push_str("                });\n");
    s.push_str("            }\n");
    s.push_str("            Some(val) => {\n");
    s.push_str("                // key:val -- colon at tok_start + key_or_word.len().\n");
    s.push_str("                let colon_pos = tok_start + key_or_word.len();\n");
    s.push_str("                let key_lower = key_or_word.to_lowercase();\n");
    s.push_str("\n");
    s.push_str("                let (tok, err): (Token, Option<ParseError>) = match key_lower.as_str() {\n");
    s.push_str("                    \"sort\" => {\n");
    s.push_str("                        if let Some((field, dir)) = parse_sort_value(&val) {\n");
    s.push_str("                            (Token::Stem {\n");
    s.push_str("                                span: Span { start: tok_start, end: tok_end },\n");
    s.push_str("                                key_span: Span { start: tok_start, end: colon_pos },\n");
    s.push_str("                                val_span: Span { start: colon_pos + 1, end: tok_end },\n");
    s.push_str("                                kind: StemKind::Sort { field, dir },\n");
    s.push_str("                            }, None)\n");
    s.push_str("                        } else {\n");
    s.push_str("                            (Token::PartialStem {\n");
    s.push_str("                                span: Span { start: tok_start, end: tok_end },\n");
    s.push_str("                                key_span: Span { start: tok_start, end: colon_pos },\n");
    s.push_str("                                val_span: Span { start: colon_pos + 1, end: tok_end },\n");
    s.push_str("                                known_key: Some(StemKey::Sort),\n");
    s.push_str("                            }, None)\n");
    s.push_str("                        }\n");
    s.push_str("                    }\n");

    // Generated TOML field arms.
    s.push_str(&toml_match_arms);

    // Unknown key fallthrough arm.
    s.push_str("                    _ => {\n");
    s.push_str("                        let suggestion = closest_key(&key_lower, known_keys);\n");
    s.push_str("                        let message = match suggestion {\n");
    s.push_str("                            Some(s) => format!(\n");
    s.push_str("                                \"unknown filter key '{}' -- did you mean '{}'?\",\n");
    s.push_str("                                key_or_word, s\n");
    s.push_str("                            ),\n");
    s.push_str("                            None => format!(\"unknown filter key '{}'\", key_or_word),\n");
    s.push_str("                        };\n");
    s.push_str("                        let err = ParseError {\n");
    s.push_str("                            span: Span { start: tok_start, end: colon_pos },\n");
    s.push_str("                            message,\n");
    s.push_str("                        };\n");
    s.push_str("                        (Token::PartialStem {\n");
    s.push_str("                            span: Span { start: tok_start, end: tok_end },\n");
    s.push_str("                            key_span: Span { start: tok_start, end: colon_pos },\n");
    s.push_str("                            val_span: Span { start: colon_pos + 1, end: tok_end },\n");
    s.push_str("                            known_key: None,\n");
    s.push_str("                        }, Some(err))\n");
    s.push_str("                    }\n");
    s.push_str("                };\n");
    s.push_str("\n");
    s.push_str("                tokens.push(tok);\n");
    s.push_str("                if let Some(e) = err {\n");
    s.push_str("                    errors.push(e);\n");
    s.push_str("                }\n");
    s.push_str("            }\n");
    s.push_str("        }\n");
    s.push_str("    }\n");
    s.push_str("\n");
    s.push_str("    (tokens, errors)\n");
    s.push_str("}\n");

    s
}

/// Generate `impl From<&QueryAst> for ParsedQuery`.
///
/// Emits one match arm per TOML field.  The `sort:` arm is hard-coded.
/// The `priority:` arm sets the raw value (normalise_priority is applied
/// in run_query, not here, preserving the existing behaviour).
fn gen_from_ast(fields: &[FieldSpec]) -> String {
    let mut s = String::new();

    s.push_str("impl From<&QueryAst> for ParsedQuery {\n");
    s.push_str("    /// Derive a SQL-ready `ParsedQuery` from the AST.\n");
    s.push_str("    ///\n");
    s.push_str("    /// Generated from the TOML allowlist by build.rs (bd-1pl).\n");
    s.push_str("    /// One match arm per allowlist entry.  sort: is hard-coded.\n");
    s.push_str("    fn from(ast: &QueryAst) -> Self {\n");
    s.push_str("        let mut sort: Option<(SortField, SortDir)> = None;\n");

    for f in fields {
        s.push_str(&format!(
            "        let mut {}: Option<String> = None;\n",
            f.key
        ));
    }

    s.push_str("        let mut fts_words: Vec<String> = Vec::new();\n");
    s.push_str("\n");
    s.push_str("        for token in &ast.tokens {\n");
    s.push_str("            match token {\n");
    s.push_str("                Token::Stem { kind, .. } => match kind {\n");
    s.push_str("                    StemKind::Sort { field, dir } => {\n");
    s.push_str("                        sort = Some((field.clone(), dir.clone()));\n");
    s.push_str("                    }\n");

    for f in fields {
        let variant = to_pascal_case(&f.key);
        s.push_str(&format!(
            "                    StemKind::{} {{ value }} => {{\n",
            variant
        ));
        s.push_str(&format!(
            "                        {} = Some(value.clone());\n",
            f.key
        ));
        s.push_str("                    }\n");
    }

    s.push_str("                },\n");
    s.push_str("                Token::PartialStem { .. } => {}\n");
    s.push_str("                Token::Word { text, .. } => {\n");
    s.push_str("                    fts_words.push(format!(\"{}*\", text));\n");
    s.push_str("                }\n");
    s.push_str("                Token::Unknown { raw: raw_slice, .. } => {\n");
    s.push_str("                    if !raw_slice.is_empty() {\n");
    s.push_str("                        fts_words.push(format!(\"{}*\", raw_slice));\n");
    s.push_str("                    }\n");
    s.push_str("                }\n");
    s.push_str("            }\n");
    s.push_str("        }\n");
    s.push_str("\n");
    s.push_str("        ParsedQuery {\n");
    s.push_str("            sort,\n");

    for f in fields {
        s.push_str(&format!("            {},\n", f.key));
    }

    s.push_str("            fts_terms: fts_words.join(\" \"),\n");
    s.push_str("        }\n");
    s.push_str("    }\n");
    s.push_str("}\n");

    s
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
    // Generate search_stems.rs
    // -----------------------------------------------------------------------
    let fields = &config.field;

    let mut out = String::new();
    out.push_str("// search_stems.rs -- generated by build.rs (bd-1pl)\n");
    out.push_str("// DO NOT EDIT -- regenerate by running `cargo build`\n\n");

    out.push_str(&gen_stem_key_enum(fields));
    out.push('\n');
    out.push_str(&gen_stem_kind_enum(fields));
    out.push('\n');
    out.push_str(&gen_parser_fn(fields));
    out.push('\n');
    out.push_str(&gen_from_ast(fields));

    let stems_path = Path::new(&out_dir).join("search_stems.rs");
    fs::write(&stems_path, &out)
        .unwrap_or_else(|e| panic!("Cannot write {}: {e}", stems_path.display()));
}
