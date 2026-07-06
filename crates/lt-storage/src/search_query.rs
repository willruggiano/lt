/// Structured search query parser for the TUI search bar (bd-7qo).
///
/// Grammar
/// -------
/// A query string is a whitespace-separated list of tokens.
/// Each token is either a *stem* or a *free-text word*.
///
/// Stems have the form `<key>:<value>`:
///
///   sort:<field><dir>   -- sort order; dir is '+' (asc) or '-' (desc)
///   assignee:<name>     -- filter by assignee name (or "me")
///   priority:<label>    -- filter by priority (urgent/high/normal/low/none)
///   state:<name>        -- filter by workflow state name
///   team:<name>         -- filter by team name or key
///
/// All remaining tokens are concatenated and used as an FTS5 full-text query
/// against the `issues_fts` index (identifier + title columns).
///
/// Example
/// -------
///   sort:updated- assignee:me priority:urgent state:todo oauth crash
///
/// Parses as:
///   sort   -> Updated, descending
///   assignee -> "me"
///   priority -> "urgent"
///   state    -> "todo"
///   `filter.term` -> "oauth* crash*"   (prefix-matched)
///
/// Default
/// -------
/// When the user presses `/`, the search bar is pre-populated with
/// `sort:updated-` so the first thing they see is the most recently
/// updated issues in descending order.
use lt_types::issues::{AssigneeFilter, IssueFilter};
use lt_types::query::{SortDirection, SortField};

// ---------------------------------------------------------------------------
// Generated parser (bd-1pl): StemKey, StemKind, parse_query_ast_impl
// ---------------------------------------------------------------------------

// SortDirection is referenced by StemKind::Sort in the generated file; the
// `use` above brings it into scope for search_stems.rs's `include!` below.

// Include the generated enums (StemKey, StemKind) and parse_query_ast_impl().
include!(concat!(env!("OUT_DIR"), "/search_stems.rs"));

// ---------------------------------------------------------------------------
// AST types (bd-22c)
// ---------------------------------------------------------------------------

/// Byte span [start, end) within the original input string.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// A structured parse error with span and human-readable message.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    /// Byte span of the offending input region.
    pub span: Span,
    /// Human-readable description, e.g. "unknown key 'priorty', did you mean 'priority'?"
    pub message: String,
}

/// A single token in the query string, with its location in the source.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// A recognised stem: `key:value`, e.g. `sort:updated-`.
    Stem {
        span: Span,
        /// Byte span of the key part (before the colon).
        key_span: Span,
        /// Byte span of the value part (after the colon).
        val_span: Span,
        kind: StemKind,
    },
    /// A partially typed stem: the colon is present but the value is empty,
    /// or the key is a known stem key but the value is not yet valid.
    PartialStem {
        span: Span,
        key_span: Span,
        val_span: Span,
        /// The matched stem key, if the key portion is a known stem name.
        known_key: Option<StemKey>,
    },
    /// A bare word (goes to FTS).
    Word { span: Span, text: String },
}

/// A fully-parsed query AST, always constructible from any input string.
#[derive(Debug, Clone)]
pub struct QueryAst {
    /// Original input string (owned).
    pub raw: String,
    /// Ordered list of tokens (whitespace gaps are not represented).
    pub tokens: Vec<Token>,
    /// Structured parse errors collected during parsing (e.g. unknown stem keys).
    /// Always empty for well-formed input. Consumed by bd-2gj (TUI highlighting).
    pub errors: Vec<ParseError>,
}

// ---------------------------------------------------------------------------
// parse_query_ast -- Chumsky-backed parser (bd-1pl)
// ---------------------------------------------------------------------------

/// Parse a raw query string into a `QueryAst` with full span information.
///
/// Delegates to the generated `parse_query_ast_impl()` function (from
/// `search_stems.rs`) which uses Chumsky error recovery.  Never panics;
/// any input string yields a valid `QueryAst`.
pub fn parse_query_ast(raw: &str) -> QueryAst {
    let (tokens, errors) = parse_query_ast_impl(raw);
    for err in &errors {
        tracing::warn!(
            span_start = err.span.start,
            span_end = err.span.end,
            "search parse error: {}",
            err.message
        );
    }
    QueryAst {
        raw: raw.to_string(),
        tokens,
        errors,
    }
}

// ---------------------------------------------------------------------------
// AST -> IssueFilter lowering
// ---------------------------------------------------------------------------

/// Lower a `QueryAst` to the typed [`IssueFilter`] plus the sort stem, if
/// present. Unknown stem keys and partially-typed stems are skipped (they
/// carry no `StemKind`); free-text words join into `filter.term` with a
/// trailing `*` for FTS5 prefix matching.
pub fn lower_ast(ast: &QueryAst) -> (IssueFilter, Option<(SortField, SortDirection)>) {
    let mut filter = IssueFilter::default();
    let mut sort: Option<(SortField, SortDirection)> = None;
    let mut fts_words: Vec<String> = Vec::new();

    for token in &ast.tokens {
        match token {
            Token::Stem { kind, .. } => match kind {
                StemKind::Sort { field, dir } => sort = Some((field.clone(), *dir)),
                StemKind::Assignee { value } => {
                    filter.assignee = Some(AssigneeFilter::Contains(value.clone()));
                }
                StemKind::Priority { value } => filter.priority = value.parse().ok(),
                StemKind::State { value } => filter.state = Some(value.clone()),
                StemKind::Team { value } => filter.team = Some(value.clone()),
                StemKind::Label { value } => filter.label = Some(value.clone()),
                StemKind::Project { value } => filter.project = Some(value.clone()),
                StemKind::Cycle { value } => filter.cycle = Some(value.clone()),
                StemKind::Creator { value } => filter.creator = Some(value.clone()),
            },
            Token::PartialStem { .. } => {}
            Token::Word { text, .. } => fts_words.push(format!("{text}*")),
        }
    }

    if !fts_words.is_empty() {
        filter.term = Some(fts_words.join(" "));
    }

    (filter, sort)
}

/// Resolve `assignee:me` to the viewer's exact name. Without a synced
/// viewer, the literal value "me" stays: an exact match no real assignee
/// name equals, so the filter matches nothing rather than falling back to
/// unfiltered.
pub fn resolve_me(filter: &mut IssueFilter, viewer_name: Option<&str>) {
    if let Some(AssigneeFilter::Contains(value)) = &filter.assignee
        && value.eq_ignore_ascii_case("me")
    {
        filter.assignee = Some(AssigneeFilter::Exact(
            viewer_name.unwrap_or("me").to_string(),
        ));
    }
}

// parse_sort_value() is generated into search_stems.rs (bd-2w5). The sort
// field maps onto a registered `SortCol` via `crate::db::filters::sort_column`
// (shared with the CLI filter builder -- same generated `SortField` type),
// not a generated function, so ORDER BY text lives only in `db/sql.rs`.

// ---------------------------------------------------------------------------
// Default query string shown when the user presses /
// ---------------------------------------------------------------------------

/// The default query pre-populated in the search bar when the user presses `/`.
pub const DEFAULT_QUERY: &str = "sort:updated-";

// ---------------------------------------------------------------------------
// args_to_ast / render_filter_context (bd-3nu)
// ---------------------------------------------------------------------------

/// Convert a typed filter/sort into a `QueryAst` suitable for use as the
/// initial filter state.
///
/// Builds a space-separated query string from the filter's team, assignee,
/// state, and priority fields plus the sort field/direction, and passes it
/// through `parse_query_ast()` so the resulting AST is always structurally
/// valid.
pub fn args_to_ast(filter: &IssueFilter, sort: &SortField, direction: SortDirection) -> QueryAst {
    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = &filter.team {
        parts.push(format!("team:{t}"));
    }
    match &filter.assignee {
        Some(AssigneeFilter::Exact(a) | AssigneeFilter::Contains(a)) => {
            parts.push(format!("assignee:{a}"));
        }
        Some(AssigneeFilter::IsNull) | None => {}
    }
    if let Some(s) = &filter.state {
        parts.push(format!("state:{s}"));
    }
    if let Some(p) = filter.priority {
        parts.push(format!("priority:{}", p.0));
    }
    let dir = if direction == SortDirection::Descending {
        "-"
    } else {
        "+"
    };
    parts.push(format!("sort:{}{}", sort.label(), dir));
    parse_query_ast(&parts.join(" "))
}

/// Render a `QueryAst` as a compact filter context string for display in the
/// TUI header.
///
/// Iterates the AST tokens and formats each recognised stem as `key:value`,
/// joining with two spaces.  `PartialStem` tokens are skipped so
/// partially-typed input is not shown in the header.
pub fn render_filter_context(ast: &QueryAst) -> String {
    let mut parts: Vec<String> = Vec::new();
    for token in &ast.tokens {
        match token {
            Token::Stem { kind, .. } => match kind {
                StemKind::Sort { field, dir } => {
                    let d = match dir {
                        SortDirection::Descending => "-",
                        SortDirection::Ascending => "+",
                    };
                    parts.push(format!("sort:{}{}", field.label(), d));
                }
                StemKind::Assignee { value } => parts.push(format!("assignee:{value}")),
                StemKind::Priority { value } => parts.push(format!("priority:{value}")),
                StemKind::State { value } => parts.push(format!("state:{value}")),
                StemKind::Team { value } => parts.push(format!("team:{value}")),
                StemKind::Label { value } => parts.push(format!("label:{value}")),
                StemKind::Project { value } => parts.push(format!("project:{value}")),
                StemKind::Cycle { value } => parts.push(format!("cycle:{value}")),
                StemKind::Creator { value } => parts.push(format!("creator:{value}")),
            },
            Token::Word { text, .. } => parts.push(text.clone()),
            Token::PartialStem { .. } => {} // skip in header display
        }
    }
    parts.join("  ")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ast_empty_input() {
        let ast = parse_query_ast("");
        assert_eq!(ast.raw, "");
        assert!(ast.tokens.is_empty());
    }

    #[test]
    fn ast_whitespace_only() {
        let ast = parse_query_ast("   ");
        assert!(ast.tokens.is_empty());
    }

    #[test]
    fn ast_single_word_span() {
        let ast = parse_query_ast("hello");
        assert_eq!(ast.tokens.len(), 1);
        match &ast.tokens[0] {
            Token::Word { span, text } => {
                assert_eq!(span.start, 0);
                assert_eq!(span.end, 5);
                assert_eq!(text, "hello");
            }
            other => panic!("expected Word, got {other:?}"),
        }
    }

    #[test]
    fn ast_two_words_spans() {
        let ast = parse_query_ast("foo bar");
        assert_eq!(ast.tokens.len(), 2);
        match &ast.tokens[0] {
            Token::Word { span, text } => {
                assert_eq!((span.start, span.end), (0, 3));
                assert_eq!(text, "foo");
            }
            other => panic!("expected Word, got {other:?}"),
        }
        match &ast.tokens[1] {
            Token::Word { span, text } => {
                assert_eq!((span.start, span.end), (4, 7));
                assert_eq!(text, "bar");
            }
            other => panic!("expected Word, got {other:?}"),
        }
    }

    #[test]
    fn ast_valid_sort_stem() {
        let ast = parse_query_ast("sort:updated-");
        assert_eq!(ast.tokens.len(), 1);
        match &ast.tokens[0] {
            Token::Stem {
                span,
                key_span,
                val_span,
                kind: StemKind::Sort { field, dir },
            } => {
                assert_eq!((span.start, span.end), (0, 13));
                assert_eq!((key_span.start, key_span.end), (0, 4));
                assert_eq!((val_span.start, val_span.end), (5, 13));
                assert!(matches!(field, SortField::Updated));
                assert_eq!(*dir, SortDirection::Descending);
            }
            other => panic!("expected Stem(Sort), got {other:?}"),
        }
    }

    #[test]
    fn ast_partial_sort_empty_value() {
        // "sort:" -- known key, empty value
        let ast = parse_query_ast("sort:");
        assert_eq!(ast.tokens.len(), 1);
        match &ast.tokens[0] {
            Token::PartialStem {
                span,
                key_span,
                val_span,
                known_key,
            } => {
                assert_eq!((span.start, span.end), (0, 5));
                assert_eq!((key_span.start, key_span.end), (0, 4));
                // val_span is empty: start == end == 5
                assert_eq!((val_span.start, val_span.end), (5, 5));
                assert_eq!(*known_key, Some(StemKey::Sort));
            }
            other => panic!("expected PartialStem, got {other:?}"),
        }
    }

    #[test]
    fn ast_partial_sort_invalid_value() {
        let ast = parse_query_ast("sort:bogus");
        assert_eq!(ast.tokens.len(), 1);
        match &ast.tokens[0] {
            Token::PartialStem { known_key, .. } => {
                assert_eq!(*known_key, Some(StemKey::Sort));
            }
            other => panic!("expected PartialStem, got {other:?}"),
        }
    }

    #[test]
    fn ast_unknown_key_partial_stem() {
        let ast = parse_query_ast("foo:bar");
        assert_eq!(ast.tokens.len(), 1);
        match &ast.tokens[0] {
            Token::PartialStem { known_key, .. } => {
                assert_eq!(*known_key, None);
            }
            other => panic!("expected PartialStem(known_key=None), got {other:?}"),
        }
    }

    #[test]
    fn ast_spans_cover_all_non_whitespace() {
        let raw = "  sort:updated-  foo  ";
        let ast = parse_query_ast(raw);
        // Every token span should point into raw correctly.
        for token in &ast.tokens {
            let (start, end) = match token {
                Token::Stem { span, .. }
                | Token::PartialStem { span, .. }
                | Token::Word { span, .. } => (span.start, span.end),
            };
            // Bounds check.
            assert!(end <= raw.len(), "end={} > raw.len()={}", end, raw.len());
            // Must be valid UTF-8 boundaries.
            assert!(raw.is_char_boundary(start));
            assert!(raw.is_char_boundary(end));
            // The slice must not contain ASCII whitespace.
            let slice = &raw[start..end];
            assert!(
                !slice.contains(|c: char| c.is_ascii_whitespace()),
                "token span contains whitespace: {slice:?}"
            );
        }
    }

    #[test]
    fn ast_assignee_stem() {
        let ast = parse_query_ast("assignee:me");
        assert_eq!(ast.tokens.len(), 1);
        match &ast.tokens[0] {
            Token::Stem {
                kind: StemKind::Assignee { value },
                ..
            } => {
                assert_eq!(value, "me");
            }
            other => panic!("expected Stem(Assignee), got {other:?}"),
        }
    }

    #[test]
    fn ast_partial_assignee_empty() {
        let ast = parse_query_ast("assignee:");
        match &ast.tokens[0] {
            Token::PartialStem { known_key, .. } => {
                assert_eq!(*known_key, Some(StemKey::Assignee));
            }
            other => panic!("expected PartialStem, got {other:?}"),
        }
    }

    // -- lower_ast --------------------------------------------------------

    #[test]
    fn lower_ast_empty() {
        let ast = parse_query_ast("");
        let (filter, sort) = lower_ast(&ast);
        assert_eq!(filter, IssueFilter::default());
        assert!(sort.is_none());
    }

    #[test]
    fn lower_ast_default_query() {
        let ast = parse_query_ast(DEFAULT_QUERY);
        let (_, sort) = lower_ast(&ast);
        let (field, dir) = sort.unwrap();
        assert!(matches!(field, SortField::Updated));
        assert_eq!(dir, SortDirection::Descending);
    }

    #[test]
    fn lower_ast_sort_asc_plus() {
        let ast = parse_query_ast("sort:priority+");
        let (_, sort) = lower_ast(&ast);
        let (field, dir) = sort.unwrap();
        assert!(matches!(field, SortField::Priority));
        assert_eq!(dir, SortDirection::Ascending);
    }

    #[test]
    fn lower_ast_assignee_me() {
        let ast = parse_query_ast("assignee:me");
        let (filter, _) = lower_ast(&ast);
        assert_eq!(
            filter.assignee,
            Some(AssigneeFilter::Contains("me".to_string()))
        );
    }

    #[test]
    fn lower_ast_priority_urgent() {
        let ast = parse_query_ast("priority:urgent");
        let (filter, _) = lower_ast(&ast);
        assert_eq!(filter.priority.map(|p| p.0), Some(1));
    }

    #[test]
    fn lower_ast_unknown_priority_is_skipped() {
        let ast = parse_query_ast("priority:bogus");
        let (filter, _) = lower_ast(&ast);
        assert!(filter.priority.is_none());
    }

    #[test]
    fn lower_ast_state_todo() {
        let ast = parse_query_ast("state:todo");
        let (filter, _) = lower_ast(&ast);
        assert_eq!(filter.state.as_deref(), Some("todo"));
    }

    #[test]
    fn lower_ast_fts_words() {
        let ast = parse_query_ast("oauth crash");
        let (filter, _) = lower_ast(&ast);
        assert_eq!(filter.term.as_deref(), Some("oauth* crash*"));
    }

    #[test]
    fn lower_ast_mixed_query() {
        let ast =
            parse_query_ast("sort:updated- assignee:me priority:urgent state:todo oauth crash");
        let (filter, sort) = lower_ast(&ast);
        let (field, dir) = sort.unwrap();
        assert!(matches!(field, SortField::Updated));
        assert_eq!(dir, SortDirection::Descending);
        assert_eq!(
            filter.assignee,
            Some(AssigneeFilter::Contains("me".to_string()))
        );
        assert_eq!(filter.priority.map(|p| p.0), Some(1));
        assert_eq!(filter.state.as_deref(), Some("todo"));
        assert_eq!(filter.term.as_deref(), Some("oauth* crash*"));
    }

    #[test]
    fn lower_ast_unknown_sort_field_goes_to_fts() {
        // "sort:bogus" is a PartialStem -- goes to fts, not treated as sort.
        let ast = parse_query_ast("sort:bogus");
        let (filter, sort) = lower_ast(&ast);
        assert_eq!(filter.term, None);
        assert!(sort.is_none());
    }

    #[test]
    fn lower_ast_unknown_stem_skipped() {
        // "foo:bar" is an unknown PartialStem and must not be emitted as FTS.
        // "baz" is a plain Word and should be emitted.
        let ast = parse_query_ast("foo:bar baz");
        let (filter, _) = lower_ast(&ast);
        assert_eq!(filter.term.as_deref(), Some("baz*"));
    }

    #[test]
    fn lower_ast_label_project_cycle_creator() {
        let ast = parse_query_ast("label:backend project:platform cycle:seven creator:carol");
        let (filter, _) = lower_ast(&ast);
        assert_eq!(filter.label.as_deref(), Some("backend"));
        assert_eq!(filter.project.as_deref(), Some("platform"));
        assert_eq!(filter.cycle.as_deref(), Some("seven"));
        assert_eq!(filter.creator.as_deref(), Some("carol"));
    }

    #[test]
    fn lower_ast_team() {
        let ast = parse_query_ast("team:eng");
        let (filter, _) = lower_ast(&ast);
        assert_eq!(filter.team.as_deref(), Some("eng"));
    }

    // -- resolve_me ---------------------------------------------------------

    #[test]
    fn resolve_me_replaces_with_viewer_name() {
        let ast = parse_query_ast("assignee:me");
        let (mut filter, _) = lower_ast(&ast);
        resolve_me(&mut filter, Some("Alice"));
        assert_eq!(
            filter.assignee,
            Some(AssigneeFilter::Exact("Alice".to_string()))
        );
    }

    #[test]
    fn resolve_me_no_viewer_keeps_literal_me() {
        let ast = parse_query_ast("assignee:me");
        let (mut filter, _) = lower_ast(&ast);
        resolve_me(&mut filter, None);
        assert_eq!(
            filter.assignee,
            Some(AssigneeFilter::Exact("me".to_string()))
        );
    }

    #[test]
    fn resolve_me_leaves_other_assignees_untouched() {
        let ast = parse_query_ast("assignee:bob");
        let (mut filter, _) = lower_ast(&ast);
        resolve_me(&mut filter, Some("Alice"));
        assert_eq!(
            filter.assignee,
            Some(AssigneeFilter::Contains("bob".to_string()))
        );
    }
}

#[cfg(test)]
mod merged_read_tests {
    use lt_types::issues::IssuesVariables;
    use lt_types::types;
    use rusqlite::Connection;

    use super::*;
    use crate::db;
    use crate::db::query_issues;

    fn user(name: &str) -> types::User {
        types::User {
            id: name.into(),
            name: name.to_string(),
        }
    }

    fn state(name: &str) -> types::WorkflowState {
        types::WorkflowState {
            id: name.into(),
            name: name.to_string(),
            position: 1.0,
        }
    }

    /// A baseline issue; tests override only the fields a filter targets. Entity
    /// ids mirror names (the team id is its key) so the relational upsert
    /// reconstructs them.
    fn issue(id: &str, title: &str) -> types::Issue {
        types::Issue {
            id: id.into(),
            identifier: format!("ENG-{id}"),
            title: title.to_string(),
            priority_label: "Medium".to_string(),
            priority: lt_types::scalars::Priority(3),
            state: state("Todo"),
            assignee: None,
            team: types::Team {
                id: "ENG".into(),
                name: "Engineering".to_string(),
            },
            description: None,
            labels: types::IssueLabelConnection { nodes: Vec::new() },
            project: None,
            cycle: None,
            creator: None,
            parent: None,
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
        }
    }

    fn test_db() -> Connection {
        let database = db::Database::memory().unwrap();
        let conn = database.connect().unwrap();

        let mut r1 = issue("1", "fix oauth login");
        r1.priority_label = "Urgent".to_string();
        r1.assignee = Some(user("Alice"));
        r1.updated_at = "2026-01-05T00:00:00Z".parse().unwrap();

        let mut r2 = issue("2", "render markdown");
        r2.priority_label = "High".to_string();
        r2.state = state("In Progress");
        r2.assignee = Some(user("Bob"));
        r2.team = types::Team {
            id: "DES".into(),
            name: "Design".to_string(),
        };
        r2.updated_at = "2026-01-04T00:00:00Z".parse().unwrap();
        r2.labels = types::IssueLabelConnection {
            nodes: vec![
                types::IssueLabel {
                    id: "backend".into(),
                    name: "backend".to_string(),
                },
                types::IssueLabel {
                    id: "urgent".into(),
                    name: "urgent".to_string(),
                },
            ],
        };
        r2.project = Some(types::Project {
            id: "Platform".into(),
            name: "Platform".to_string(),
        });
        r2.cycle = Some(types::Cycle {
            id: "Cycle 7".into(),
            name: Some("Cycle 7".to_string()),
        });
        r2.creator = Some(user("Carol"));

        let mut r3 = issue("3", "oauth token refresh");
        r3.priority_label = "Low".to_string();
        r3.state = state("Done");
        r3.updated_at = "2026-01-03T00:00:00Z".parse().unwrap();

        // Sync owns workflow states -- issue upserts never write them, so
        // every state a fixture's issues reference must already be locally
        // known for the read model's `JOIN` to resolve the row.
        db::upsert_team_state(&conn, "ENG", &state("Todo")).unwrap();
        db::upsert_team_state(&conn, "DES", &state("In Progress")).unwrap();
        db::upsert_team_state(&conn, "ENG", &state("Done")).unwrap();

        db::upsert_issues(&conn, &[r1, r2, r3]).unwrap();
        conn
    }

    fn ids(issues: &[types::Issue]) -> Vec<&str> {
        issues.iter().map(|i| i.id.inner()).collect()
    }

    /// Run `query` through the same lowering the TUI's search bar uses,
    /// against the merged read entry point.
    fn run(conn: &Connection, query: &str, limit: i32) -> Vec<types::Issue> {
        let ast = parse_query_ast(query);
        let (filter, sort) = lower_ast(&ast);
        let vars = IssuesVariables {
            filter: (filter != IssueFilter::default()).then_some(filter),
            sort: sort.map(|(field, direction)| lt_types::issues::IssueSort { field, direction }),
            first: Some(limit),
            after: None,
        };
        query_issues(conn, &vars).unwrap().nodes
    }

    #[test]
    fn fts_term_matches_title_tokens() {
        let conn = test_db();
        // Both "fix oauth login" and "oauth token refresh" match; default sort
        // is updated DESC, so id 1 (newer) precedes id 3.
        assert_eq!(ids(&run(&conn, "oauth", 50)), ["1", "3"]);
    }

    #[test]
    fn fts_term_combined_with_structured_filter() {
        let conn = test_db();
        assert_eq!(ids(&run(&conn, "oauth state:done", 50)), ["3"]);
    }

    #[test]
    fn assignee_filter_matches_substring() {
        let conn = test_db();
        assert_eq!(ids(&run(&conn, "assignee:ali", 50)), ["1"]);
    }

    #[test]
    fn assignee_me_literal_without_resolution_matches_nothing() {
        let conn = test_db();
        // No resolve_me call: "me" stays a substring match against no real name.
        assert!(run(&conn, "assignee:me", 50).is_empty());
    }

    #[test]
    fn priority_filter_normalises_label() {
        let conn = test_db();
        assert_eq!(ids(&run(&conn, "priority:urgent", 50)), ["1"]);
    }

    #[test]
    fn unknown_priority_is_skipped_not_applied() {
        let conn = test_db();
        // The unrecognised value drops the filter, so all rows return.
        assert_eq!(run(&conn, "priority:bogus", 50).len(), 3);
    }

    #[test]
    fn team_filter_matches_name_or_key() {
        let conn = test_db();
        assert_eq!(ids(&run(&conn, "team:des", 50)), ["2"]);
        assert_eq!(ids(&run(&conn, "team:engineering", 50)), ["1", "3"]);
    }

    #[test]
    fn label_project_cycle_creator_filters() {
        let conn = test_db();
        for stem in [
            "label:backend",
            "project:platform",
            "cycle:cycle",
            "creator:carol",
        ] {
            assert_eq!(ids(&run(&conn, stem, 50)), ["2"], "for {stem}");
        }
    }

    #[test]
    fn state_filter_substring_match() {
        let conn = test_db();
        assert_eq!(ids(&run(&conn, "state:progress", 50)), ["2"]);
    }

    #[test]
    fn sort_and_limit_apply() {
        let conn = test_db();
        // Ascending by priority label is alphabetical: High, Low, Urgent.
        assert_eq!(ids(&run(&conn, "sort:priority+", 50)), ["2", "3", "1"]);
        assert_eq!(run(&conn, "sort:updated-", 2).len(), 2);
    }
}
