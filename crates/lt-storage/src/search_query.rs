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
///   `fts_query` -> "oauth* crash*"   (prefix-matched)
///
/// Default
/// -------
/// When the user presses `/`, the search bar is pre-populated with
/// `sort:updated-` so the first thing they see is the most recently
/// updated issues in descending order.
use anyhow::Result;
use lt_types::query::{IssueQuery, SortField};
use lt_types::types::Issue;
use rusqlite::Connection;
use tracing::warn;

// ---------------------------------------------------------------------------
// Generated parser (bd-1pl): StemKey, StemKind, parse_query_ast_impl,
// From<&QueryAst> for ParsedQuery, apply_generated_conditions
// ---------------------------------------------------------------------------

// SortDir is referenced by StemKind::Sort in the generated file.
// We forward-declare the sort direction enum here so it is in scope when
// search_stems.rs is included below.
//
// NOTE: SortDir is also used by ParsedQuery and related helpers defined later
// in this file.  The include! expands here, so it sees SortDir.

/// Direction suffix on a sort stem.
#[derive(Debug, Clone, PartialEq)]
pub enum SortDir {
    /// Ascending ('+' suffix or no suffix).
    Asc,
    /// Descending ('-' suffix).
    Desc,
}

// Include the generated enums (StemKey, StemKind), parse_query_ast_impl(), and
// From<&QueryAst> for ParsedQuery.
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
        warn!(
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

// From<&QueryAst> for ParsedQuery is generated in search_stems.rs (bd-1pl).

// ---------------------------------------------------------------------------
// ParsedQuery -- result of parsing a raw query string
// ---------------------------------------------------------------------------

/// A fully parsed search query.
#[derive(Debug, Clone)]
pub struct ParsedQuery {
    /// Sort field, if a `sort:` stem was present.
    pub sort: Option<(SortField, SortDir)>,
    /// Assignee filter value (raw string, "me" is treated specially at query time).
    pub assignee: Option<String>,
    /// Priority filter label (normalised to lowercase).
    pub priority: Option<String>,
    /// State filter (substring match, lowercased).
    pub state: Option<String>,
    /// Team filter (substring match).
    pub team: Option<String>,
    /// Label filter (substring match, lowercased).
    pub label: Option<String>,
    /// Project filter (substring match, lowercased).
    pub project: Option<String>,
    /// Cycle filter (substring match, lowercased).
    pub cycle: Option<String>,
    /// Creator filter (substring match, lowercased).
    pub creator: Option<String>,
    /// Free-text words joined into an FTS5 query.  Empty string means no FTS.
    pub fts_terms: String,
}

impl ParsedQuery {
    /// Return `true` when any filter constraint (beyond sort) is active.
    pub fn has_filters(&self) -> bool {
        self.assignee.is_some()
            || self.priority.is_some()
            || self.state.is_some()
            || self.team.is_some()
            || self.label.is_some()
            || self.project.is_some()
            || self.cycle.is_some()
            || self.creator.is_some()
            || !self.fts_terms.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a raw query string typed into the TUI search bar.
///
/// Unknown stems are treated as free-text words so that partial typing
/// (e.g. `sort:`) does not produce hard errors.
///
/// NOTE: Production code now uses `ParsedQuery::from(&QueryAst)` instead.
/// This function is retained for unit tests that verify parity between the
/// two parsing paths.
#[cfg(test)]
pub fn parse_query(raw: &str) -> ParsedQuery {
    let mut sort: Option<(SortField, SortDir)> = None;
    let mut assignee: Option<String> = None;
    let mut priority: Option<String> = None;
    let mut state: Option<String> = None;
    let mut team: Option<String> = None;
    let mut label: Option<String> = None;
    let mut project: Option<String> = None;
    let mut cycle: Option<String> = None;
    let mut creator: Option<String> = None;
    let mut fts_words: Vec<String> = Vec::new();

    for token in raw.split_whitespace() {
        if let Some((key, value)) = token.split_once(':') {
            match key.to_lowercase().as_str() {
                "sort" => {
                    if let Some((field, dir)) = parse_sort_value(value) {
                        sort = Some((field, dir));
                        continue;
                    }
                    // Unrecognised sort value -- fall through to fts_words.
                }
                "assignee" if !value.is_empty() => {
                    assignee = Some(value.to_lowercase());
                    continue;
                }
                "priority" if !value.is_empty() => {
                    priority = Some(value.to_lowercase());
                    continue;
                }
                "state" if !value.is_empty() => {
                    state = Some(value.to_lowercase());
                    continue;
                }
                "team" if !value.is_empty() => {
                    team = Some(value.to_string());
                    continue;
                }
                "label" if !value.is_empty() => {
                    label = Some(value.to_lowercase());
                    continue;
                }
                "project" if !value.is_empty() => {
                    project = Some(value.to_lowercase());
                    continue;
                }
                "cycle" if !value.is_empty() => {
                    cycle = Some(value.to_lowercase());
                    continue;
                }
                "creator" if !value.is_empty() => {
                    creator = Some(value.to_lowercase());
                    continue;
                }
                _ => {}
            }
        }
        // Plain word -- add to FTS query with prefix wildcard for incremental matching.
        fts_words.push(format!("{token}*"));
    }

    let fts_terms = fts_words.join(" ");

    ParsedQuery {
        sort,
        assignee,
        priority,
        state,
        team,
        label,
        project,
        cycle,
        creator,
        fts_terms,
    }
}

// parse_sort_value() is generated into search_stems.rs (bd-2w5).
// sort_col() is generated into search_stems.rs (bd-2w5).

// ---------------------------------------------------------------------------
// Normalise priority label
// ---------------------------------------------------------------------------

/// Normalise a user-supplied priority string to the DB label, or return `None`
/// when the string is not a recognised priority.
fn normalise_priority(s: &str) -> Option<&'static str> {
    match s.to_lowercase().as_str() {
        "none" | "no" | "0" => Some("No priority"),
        "urgent" | "1" => Some("Urgent"),
        "high" | "2" => Some("High"),
        "medium" | "3" => Some("Medium"),
        "low" | "4" => Some("Low"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// SQL execution
// ---------------------------------------------------------------------------

/// Execute a `ParsedQuery` against the local SQLite database.
///
/// Returns up to `limit` matching `Issue` rows.
///
/// # Errors
///
/// Returns an error if the SQLite query fails (e.g. FTS index unavailable).
// Build the structured WHERE conditions and their bound parameters from a
// parsed query. Conditions reference the read model's join aliases (`i` issues,
// `s` state, `t` team, `ua` assignee, `uc` creator, `p` project, `c` cycle).
// Each filter stem maps to one or more SQLite conditions:
//   assignee: LOWER(COALESCE(ua.name,'')) LIKE '%<val>%'
//             Special case: value "me" -> LOWER(ua.name) = 'me'
//   priority: i.priority_label = <normalised-label>
//             Value is normalised via normalise_priority() before binding;
//             unrecognised values are silently skipped.
//   state:    LOWER(s.name) LIKE '%<val>%'
//   team:     LOWER(t.name) LIKE '%<val>%' OR LOWER(COALESCE(i.team_id,'')) LIKE '%<val>%'
//   label:    EXISTS join over issue_labels/labels matching LOWER(l.name)
fn build_conditions(q: &ParsedQuery) -> (Vec<String>, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut conditions: Vec<String> = Vec::new();
    // rusqlite requires heterogeneous param lists via the params! macro or by
    // boxing. We box with Box<dyn ToSql> for flexibility.
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    // -- assignee --
    if let Some(ref a) = q.assignee {
        if a == "me" {
            // "me" without auth context: match the literal string "me" -- callers
            // that have a viewer name should resolve it before calling run_query.
            conditions.push("LOWER(ua.name) = 'me'".to_string());
        } else {
            conditions.push("LOWER(COALESCE(ua.name,'')) LIKE ?".to_string());
            bind.push(Box::new(format!("%{a}%")));
        }
    }

    // -- priority --
    if let Some(ref p) = q.priority
        && let Some(label) = normalise_priority(p)
    {
        conditions.push("i.priority_label = ?".to_string());
        bind.push(Box::new(label.to_string()));
    }
    // Unknown priority string: skip the filter silently so partial typing
    // does not wipe the result list.

    // -- state --
    if let Some(ref s) = q.state {
        conditions.push("LOWER(s.name) LIKE ?".to_string());
        bind.push(Box::new(format!("%{s}%")));
    }

    // -- team --
    if let Some(ref t) = q.team {
        conditions
            .push("(LOWER(t.name) LIKE ? OR LOWER(COALESCE(i.team_id,'')) LIKE ?)".to_string());
        let pat = format!("%{}%", t.to_lowercase());
        bind.push(Box::new(pat.clone()));
        bind.push(Box::new(pat));
    }

    // -- label --
    if let Some(ref l) = q.label {
        conditions.push(
            "EXISTS (SELECT 1 FROM issue_labels il JOIN labels lb ON lb.id = il.label_id
                     WHERE il.issue_id = i.id AND LOWER(lb.name) LIKE ?)"
                .to_string(),
        );
        bind.push(Box::new(format!("%{l}%")));
    }

    // -- project --
    if let Some(ref p) = q.project {
        conditions.push("LOWER(COALESCE(p.name,'')) LIKE ?".to_string());
        bind.push(Box::new(format!("%{p}%")));
    }

    // -- cycle --
    if let Some(ref c) = q.cycle {
        conditions.push("LOWER(COALESCE(c.name,'')) LIKE ?".to_string());
        bind.push(Box::new(format!("%{c}%")));
    }

    // -- creator --
    if let Some(ref c) = q.creator {
        conditions.push("LOWER(COALESCE(uc.name,'')) LIKE ?".to_string());
        bind.push(Box::new(format!("%{c}%")));
    }

    (conditions, bind)
}

/// Build the final SELECT statement for the given query and structured
/// conditions. FTS queries join against `issues_fts`; non-FTS queries scan
/// `issues` directly.
fn build_sql(q: &ParsedQuery, conditions: &[String], limit: usize) -> String {
    let (order_col, order_dir) = match &q.sort {
        Some((field, dir)) => (
            sort_col(field),
            if *dir == SortDir::Desc { "DESC" } else { "ASC" },
        ),
        None => ("i.updated_at", "DESC"),
    };

    let cols = crate::db::ISSUE_COLUMNS;
    let joins = crate::db::ISSUE_JOINS;

    if q.fts_terms.is_empty() {
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };
        format!(
            "SELECT {cols}
             FROM issues i
             {joins}
             {where_clause}
             ORDER BY {order_col} {order_dir}
             LIMIT {limit}",
        )
    } else {
        // Join issues with FTS results, apply additional structured filters.
        let extra_cond = if conditions.is_empty() {
            String::new()
        } else {
            format!(" AND {}", conditions.join(" AND "))
        };
        format!(
            "SELECT {cols}
             FROM issues i
             JOIN issues_fts ON issues_fts.rowid = i.rowid
             {joins}
             WHERE issues_fts MATCH ?{extra_cond}
             ORDER BY {order_col} {order_dir}
             LIMIT {limit}",
        )
    }
}

pub fn run_query(conn: &Connection, q: &ParsedQuery, limit: usize) -> Result<Vec<Issue>> {
    let (conditions, bind) = build_conditions(q);

    let has_fts = !q.fts_terms.is_empty();
    let sql = build_sql(q, &conditions, limit);

    // Build the final param list: for FTS queries the FTS term goes first.
    let all_params: Vec<Box<dyn rusqlite::types::ToSql>> = if has_fts {
        let mut v: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(q.fts_terms.clone())];
        v.extend(bind);
        v
    } else {
        bind
    };

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| anyhow::anyhow!("prepare search_query: {e}"))?;

    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        all_params.iter().map(std::convert::AsRef::as_ref).collect();

    let rows = stmt
        .query_map(param_refs.as_slice(), crate::db::issue_from_row)
        .map_err(|e| anyhow::anyhow!("execute search_query: {e}"))?;

    let mut issues = Vec::new();
    for row in rows {
        issues.push(row.map_err(|e| anyhow::anyhow!("read search_query row: {e}"))?);
    }
    Ok(issues)
}

/// Resolve "me" in a parsed query to the actual viewer name.
///
/// If `viewer_name` is Some and the assignee filter is "me", it is replaced
/// with the actual name so that the SQL LIKE filter works correctly.
pub fn resolve_me(q: &mut ParsedQuery, viewer_name: Option<&str>) {
    if q.assignee.as_deref() == Some("me") {
        q.assignee = viewer_name.map(str::to_lowercase);
    }
}

// ---------------------------------------------------------------------------
// Default query string shown when the user presses /
// ---------------------------------------------------------------------------

/// The default query pre-populated in the search bar when the user presses `/`.
pub const DEFAULT_QUERY: &str = "sort:updated-";

// ---------------------------------------------------------------------------
// args_to_ast / render_filter_context (bd-3nu)
// ---------------------------------------------------------------------------

/// Convert CLI `IssueQuery` into a `QueryAst` suitable for use as the initial
/// filter state.
///
/// Builds a space-separated query string from the args fields (team, assignee,
/// state, priority, sort) and passes it through `parse_query_ast()` so the
/// resulting AST is always structurally valid.
pub fn args_to_ast(args: &IssueQuery) -> QueryAst {
    let mut parts: Vec<String> = Vec::new();
    if let Some(ref t) = args.team {
        parts.push(format!("team:{t}"));
    }
    if let Some(ref a) = args.assignee {
        parts.push(format!("assignee:{a}"));
    }
    if let Some(ref s) = args.state {
        parts.push(format!("state:{s}"));
    }
    if let Some(ref p) = args.priority {
        parts.push(format!("priority:{p}"));
    }
    let dir = if args.desc { "-" } else { "+" };
    parts.push(format!("sort:{}{}", args.sort.label(), dir));
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
                        SortDir::Desc => "-",
                        SortDir::Asc => "+",
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
    fn parse_empty_string() {
        let q = parse_query("");
        assert!(q.sort.is_none());
        assert!(q.fts_terms.is_empty());
    }

    #[test]
    fn parse_default_query() {
        let q = parse_query(DEFAULT_QUERY);
        let (field, dir) = q.sort.unwrap();
        assert!(matches!(field, SortField::Updated));
        assert_eq!(dir, SortDir::Desc);
        assert!(q.fts_terms.is_empty());
    }

    #[test]
    fn parse_sort_asc_plus() {
        let q = parse_query("sort:priority+");
        let (field, dir) = q.sort.unwrap();
        assert!(matches!(field, SortField::Priority));
        assert_eq!(dir, SortDir::Asc);
    }

    #[test]
    fn parse_sort_no_suffix_defaults_asc() {
        let q = parse_query("sort:title");
        let (field, dir) = q.sort.unwrap();
        assert!(matches!(field, SortField::Title));
        assert_eq!(dir, SortDir::Asc);
    }

    #[test]
    fn parse_assignee_me() {
        let q = parse_query("assignee:me");
        assert_eq!(q.assignee.as_deref(), Some("me"));
    }

    #[test]
    fn parse_priority_urgent() {
        let q = parse_query("priority:urgent");
        assert_eq!(q.priority.as_deref(), Some("urgent"));
    }

    #[test]
    fn parse_state_todo() {
        let q = parse_query("state:todo");
        assert_eq!(q.state.as_deref(), Some("todo"));
    }

    #[test]
    fn parse_fts_words() {
        let q = parse_query("oauth crash");
        assert_eq!(q.fts_terms, "oauth* crash*");
    }

    #[test]
    fn parse_mixed_query() {
        let q = parse_query("sort:updated- assignee:me priority:urgent state:todo oauth crash");
        let (field, dir) = q.sort.clone().unwrap();
        assert!(matches!(field, SortField::Updated));
        assert_eq!(dir, SortDir::Desc);
        assert_eq!(q.assignee.as_deref(), Some("me"));
        assert_eq!(q.priority.as_deref(), Some("urgent"));
        assert_eq!(q.state.as_deref(), Some("todo"));
        assert_eq!(q.fts_terms, "oauth* crash*");
    }

    #[test]
    fn parse_unknown_sort_field_goes_to_fts() {
        let q = parse_query("sort:bogus");
        // bogus field -> no sort set, "sort:bogus" goes to fts
        assert!(q.sort.is_none());
        assert_eq!(q.fts_terms, "sort:bogus*");
    }

    #[test]
    fn parse_unknown_stem_goes_to_fts() {
        let q = parse_query("foo:bar baz");
        assert_eq!(q.fts_terms, "foo:bar* baz*");
    }

    #[test]
    fn resolve_me_replaces_with_viewer_name() {
        let mut q = parse_query("assignee:me");
        resolve_me(&mut q, Some("Alice"));
        assert_eq!(q.assignee.as_deref(), Some("alice"));
    }

    #[test]
    fn resolve_me_no_viewer_clears_assignee() {
        let mut q = parse_query("assignee:me");
        resolve_me(&mut q, None);
        assert!(q.assignee.is_none());
    }

    #[test]
    fn normalise_priority_variants() {
        assert_eq!(normalise_priority("urgent"), Some("Urgent"));
        assert_eq!(normalise_priority("1"), Some("Urgent"));
        assert_eq!(normalise_priority("high"), Some("High"));
        assert_eq!(normalise_priority("medium"), Some("Medium"));
        assert_eq!(normalise_priority("low"), Some("Low"));
        assert_eq!(normalise_priority("none"), Some("No priority"));
        assert_eq!(normalise_priority("bogus"), None);
    }

    // -----------------------------------------------------------------------
    // parse_query_ast tests (bd-22c)
    // -----------------------------------------------------------------------

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
        // "foo" at [0,3), "bar" at [4,7)
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
                assert_eq!(*dir, SortDir::Desc);
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

    // Parity tests: From<&QueryAst> must produce identical results to parse_query.

    fn assert_from_ast_parity(raw: &str) {
        let q1 = parse_query(raw);
        let ast = parse_query_ast(raw);
        let q2 = ParsedQuery::from(&ast);
        assert_eq!(q1.sort, q2.sort);
        assert_eq!(q1.assignee, q2.assignee);
        assert_eq!(q1.priority, q2.priority);
        assert_eq!(q1.state, q2.state);
        assert_eq!(q1.team, q2.team);
        assert_eq!(q1.fts_terms, q2.fts_terms);
    }

    #[test]
    fn from_ast_parity_empty() {
        assert_from_ast_parity("");
    }

    #[test]
    fn from_ast_parity_full_query() {
        assert_from_ast_parity("sort:updated- assignee:me priority:urgent state:todo oauth crash");
    }

    #[test]
    fn from_ast_unknown_sort_field_not_fts() {
        // "sort:bogus" is a PartialStem -- should produce no FTS terms, not "sort:bogus*".
        let ast = parse_query_ast("sort:bogus");
        let q = ParsedQuery::from(&ast);
        assert_eq!(q.fts_terms, "");
        assert_eq!(q.sort, None);
    }

    #[test]
    fn from_ast_unknown_stem_skipped() {
        // "foo:bar" is an unknown PartialStem and must not be emitted as FTS.
        // "baz" is a plain Word and should be emitted.
        let ast = parse_query_ast("foo:bar baz");
        let q = ParsedQuery::from(&ast);
        assert_eq!(q.fts_terms, "baz*");
    }

    // -----------------------------------------------------------------------
}

#[cfg(test)]
mod run_query_tests {
    use lt_types::types;
    use rusqlite::Connection;

    use super::*;
    use crate::db;

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
        }
    }

    /// A baseline issue; tests override only the fields a filter targets. Entity
    /// ids mirror names (the team id is its key) so the relational upsert
    /// reconstructs them.
    fn issue(id: &str, title: &str) -> Issue {
        Issue {
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
        let mut conn = Connection::open_in_memory().unwrap();
        db::run_migrations(&mut conn).unwrap();

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

        db::upsert_issues(&conn, &[r1, r2, r3]).unwrap();
        conn
    }

    fn ids(issues: &[Issue]) -> Vec<&str> {
        issues.iter().map(|i| i.id.inner()).collect()
    }

    #[test]
    fn fts_term_matches_title_tokens() {
        let conn = test_db();
        let q = parse_query("oauth");
        let got = run_query(&conn, &q, 50).unwrap();
        // Both "fix oauth login" and "oauth token refresh" match; default sort
        // is updated DESC, so id 1 (newer) precedes id 3.
        assert_eq!(ids(&got), ["1", "3"]);
    }

    #[test]
    fn fts_term_combined_with_structured_filter() {
        let conn = test_db();
        let q = parse_query("oauth state:done");
        let got = run_query(&conn, &q, 50).unwrap();
        assert_eq!(ids(&got), ["3"]);
    }

    #[test]
    fn assignee_filter_matches_substring() {
        let conn = test_db();
        let q = parse_query("assignee:ali");
        assert_eq!(ids(&run_query(&conn, &q, 50).unwrap()), ["1"]);
    }

    #[test]
    fn assignee_me_literal_without_resolution_matches_nothing() {
        let conn = test_db();
        let q = parse_query("assignee:me");
        assert!(run_query(&conn, &q, 50).unwrap().is_empty());
    }

    #[test]
    fn priority_filter_normalises_label() {
        let conn = test_db();
        let q = parse_query("priority:urgent");
        assert_eq!(ids(&run_query(&conn, &q, 50).unwrap()), ["1"]);
    }

    #[test]
    fn unknown_priority_is_skipped_not_applied() {
        let conn = test_db();
        let q = parse_query("priority:bogus");
        // The unrecognised value drops the filter, so all rows return.
        assert_eq!(run_query(&conn, &q, 50).unwrap().len(), 3);
    }

    #[test]
    fn team_filter_matches_name_or_key() {
        let conn = test_db();
        let by_key = parse_query("team:des");
        assert_eq!(ids(&run_query(&conn, &by_key, 50).unwrap()), ["2"]);
        let by_name = parse_query("team:engineering");
        assert_eq!(ids(&run_query(&conn, &by_name, 50).unwrap()), ["1", "3"]);
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
            let q = parse_query(stem);
            assert_eq!(ids(&run_query(&conn, &q, 50).unwrap()), ["2"], "for {stem}");
        }
    }

    #[test]
    fn state_filter_substring_match() {
        let conn = test_db();
        let q = parse_query("state:progress");
        assert_eq!(ids(&run_query(&conn, &q, 50).unwrap()), ["2"]);
    }

    #[test]
    fn sort_and_limit_apply() {
        let conn = test_db();
        // Ascending by priority label is alphabetical: High, Low, Urgent.
        let q = parse_query("sort:priority+");
        let got = run_query(&conn, &q, 50).unwrap();
        assert_eq!(ids(&got), ["2", "3", "1"]);

        let limited = run_query(&conn, &parse_query("sort:updated-"), 2).unwrap();
        assert_eq!(limited.len(), 2);
    }
}
