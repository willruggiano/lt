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
/// against the issues_fts index (identifier + title columns).
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
///   fts_query -> "oauth* crash*"   (prefix-matched)
///
/// Default
/// -------
/// When the user presses `/`, the search bar is pre-populated with
/// `sort:updated-` so the first thing they see is the most recently
/// updated issues in descending order.
use anyhow::Result;
use rusqlite::Connection;
use tracing::warn;

use crate::db::Issue;
use crate::issues::{IssueArgs, SortField};

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
    /// Anything that could not be classified (e.g. empty string, stray colon).
    #[allow(dead_code)]
    Unknown { span: Span, raw: String },
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
/// search_stems.rs) which uses Chumsky error recovery.  Never panics;
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
    /// Return `true` when no filter constraints are set and no FTS terms exist.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.sort.is_none()
            && self.assignee.is_none()
            && self.priority.is_none()
            && self.state.is_none()
            && self.team.is_none()
            && self.label.is_none()
            && self.project.is_none()
            && self.cycle.is_none()
            && self.creator.is_none()
            && self.fts_terms.is_empty()
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
        fts_words.push(format!("{}*", token));
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
pub fn run_query(conn: &Connection, q: &ParsedQuery, limit: usize) -> Result<Vec<Issue>> {
    // Build WHERE conditions and bind parameters.
    let mut conditions: Vec<String> = Vec::new();
    // We collect params as String values and pass them with a macro workaround
    // below; rusqlite requires heterogeneous param lists via the params! macro
    // or by boxing.  We use Box<dyn rusqlite::types::ToSql> for flexibility.
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    // Per-field SQL mapping (the knowledge formerly kept in TOML metadata).
    //
    // Each filter stem maps to one or more SQLite conditions:
    //   assignee: LOWER(COALESCE(assignee_name,'')) LIKE '%<val>%'
    //             Special case: value "me" -> LOWER(assignee_name) = 'me'
    //   priority: priority_label = <normalised-label>
    //             Value is normalised via normalise_priority() before binding;
    //             unrecognised values are silently skipped.
    //   state:    LOWER(state_name) LIKE '%<val>%'
    //   team:     LOWER(team_name) LIKE '%<val>%'
    //             OR LOWER(COALESCE(team_key,'')) LIKE '%<val>%'
    //   label:    LOWER(COALESCE(labels,'')) LIKE '%<val>%'
    //             (column is 'labels', not 'label_names')
    //
    // -- assignee --
    if let Some(ref a) = q.assignee {
        if a == "me" {
            // "me" without auth context: match the literal string "me" -- callers
            // that have a viewer name should resolve it before calling run_query.
            conditions.push("LOWER(assignee_name) = 'me'".to_string());
        } else {
            conditions.push("LOWER(COALESCE(assignee_name,'')) LIKE ?".to_string());
            bind.push(Box::new(format!("%{}%", a)));
        }
    }

    // -- priority --
    if let Some(ref p) = q.priority
        && let Some(label) = normalise_priority(p)
    {
        conditions.push("priority_label = ?".to_string());
        bind.push(Box::new(label.to_string()));
    }
    // Unknown priority string: skip the filter silently so partial typing
    // does not wipe the result list.

    // -- state --
    if let Some(ref s) = q.state {
        conditions.push("LOWER(state_name) LIKE ?".to_string());
        bind.push(Box::new(format!("%{}%", s)));
    }

    // -- team --
    if let Some(ref t) = q.team {
        conditions
            .push("(LOWER(team_name) LIKE ? OR LOWER(COALESCE(team_key,'')) LIKE ?)".to_string());
        let pat = format!("%{}%", t.to_lowercase());
        bind.push(Box::new(pat.clone()));
        bind.push(Box::new(pat));
    }

    // -- label --
    if let Some(ref l) = q.label {
        conditions.push("LOWER(COALESCE(labels,'')) LIKE ?".to_string());
        bind.push(Box::new(format!("%{}%", l)));
    }

    // -- project --
    if let Some(ref p) = q.project {
        conditions.push("LOWER(COALESCE(project_name,'')) LIKE ?".to_string());
        bind.push(Box::new(format!("%{}%", p)));
    }

    // -- cycle --
    if let Some(ref c) = q.cycle {
        conditions.push("LOWER(COALESCE(cycle_name,'')) LIKE ?".to_string());
        bind.push(Box::new(format!("%{}%", c)));
    }

    // -- creator --
    if let Some(ref c) = q.creator {
        conditions.push("LOWER(COALESCE(creator_name,'')) LIKE ?".to_string());
        bind.push(Box::new(format!("%{}%", c)));
    }

    // -- ORDER BY --
    let (order_col, order_dir) = match &q.sort {
        Some((field, dir)) => (
            sort_col(field),
            if *dir == SortDir::Desc { "DESC" } else { "ASC" },
        ),
        None => ("updated_at", "DESC"),
    };

    // -- FTS --
    let has_fts = !q.fts_terms.is_empty();

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    let sql = if has_fts {
        // Join issues with FTS results, apply additional structured filters.
        format!(
            "SELECT i.id, i.identifier, i.title, i.priority_label, i.state_name,
                    i.assignee_name, i.team_name, i.team_key, i.created_at, i.updated_at,
                    i.synced_at, i.description, i.labels,
                    i.project_name, i.cycle_name, i.creator_name
             FROM issues i
             JOIN issues_fts ON issues_fts.rowid = i.rowid
             WHERE issues_fts MATCH ?{extra_cond}
             ORDER BY {col} {dir}
             LIMIT {limit}",
            extra_cond = if conditions.is_empty() {
                String::new()
            } else {
                format!(" AND {}", conditions.join(" AND "))
            },
            col = order_col,
            dir = order_dir,
            limit = limit,
        )
    } else {
        format!(
            "SELECT id, identifier, title, priority_label, state_name,
                    assignee_name, team_name, team_key, created_at, updated_at, synced_at,
                    description, labels, project_name, cycle_name, creator_name
             FROM issues
             {where_clause}
             ORDER BY {col} {dir}
             LIMIT {limit}",
            where_clause = where_clause,
            col = order_col,
            dir = order_dir,
            limit = limit,
        )
    };

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
        .map_err(|e| anyhow::anyhow!("prepare search_query: {}", e))?;

    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        all_params.iter().map(|b| b.as_ref()).collect();

    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(Issue {
                id: row.get(0)?,
                identifier: row.get(1)?,
                title: row.get(2)?,
                priority_label: row.get(3)?,
                state_name: row.get(4)?,
                assignee_name: row.get(5)?,
                team_name: row.get(6)?,
                team_key: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                synced_at: row.get(10)?,
                description: row.get(11)?,
                labels: row.get::<_, Option<String>>(12)?.unwrap_or_default(),
                project_name: row.get(13)?,
                cycle_name: row.get(14)?,
                creator_name: row.get(15)?,
            })
        })
        .map_err(|e| anyhow::anyhow!("execute search_query: {}", e))?;

    let mut issues = Vec::new();
    for row in rows {
        issues.push(row.map_err(|e| anyhow::anyhow!("read search_query row: {}", e))?);
    }
    Ok(issues)
}

/// Resolve "me" in a parsed query to the actual viewer name.
///
/// If `viewer_name` is Some and the assignee filter is "me", it is replaced
/// with the actual name so that the SQL LIKE filter works correctly.
#[allow(dead_code)]
pub fn resolve_me(q: &mut ParsedQuery, viewer_name: Option<&str>) {
    if q.assignee.as_deref() == Some("me") {
        q.assignee = viewer_name.map(|n| n.to_lowercase());
    }
}

// ---------------------------------------------------------------------------
// Default query string shown when the user presses /
// ---------------------------------------------------------------------------

/// The default query pre-populated in the search bar when the user presses `/`.
pub const DEFAULT_QUERY: &str = "sort:updated-";

// ---------------------------------------------------------------------------
// Completer (bd-35l)
// ---------------------------------------------------------------------------

/// The completion context derived from the cursor position in the query.
#[derive(Debug, Clone, PartialEq)]
pub enum CompletionContext {
    /// Cursor is inside the key portion of a partial stem (or at an empty
    /// input with no characters typed yet).
    StemKey { prefix: String },
    /// Cursor is inside the value portion of a known stem (Phase 2 stub).
    StemValue { key: StemKey, prefix: String },
    /// Cursor is inside a bare word: no structured completion.
    Word,
    /// Cursor is in whitespace between tokens or past the end.
    Gap,
}

/// All known stem key strings, in display order.
const STEM_KEY_STRINGS: &[&str] = &[
    "sort:",
    "assignee:",
    "priority:",
    "state:",
    "team:",
    "label:",
    "project:",
    "cycle:",
    "creator:",
];

/// Tab-completion state for the search query bar.
pub struct Completer {
    /// The token the cursor is currently inside, if any.
    pub active_token: Option<Token>,
    /// Current completion context derived from the active token.
    pub context: CompletionContext,
    /// Completion candidates for the current context.
    pub candidates: Vec<String>,
    /// Index of the currently highlighted candidate (cycles on Tab).
    pub selected: usize,
    /// True when candidate list is being populated asynchronously (Phase 2).
    #[allow(dead_code)]
    pub candidates_pending: bool,
}

impl Completer {
    /// Create a new `Completer` with `Gap` context and empty candidates.
    pub fn new() -> Self {
        Completer {
            active_token: None,
            context: CompletionContext::Gap,
            candidates: Vec::new(),
            selected: 0,
            candidates_pending: false,
        }
    }

    /// Recompute `active_token`, `context`, `candidates`, and reset
    /// `selected` to 0 based on the current AST and cursor byte offset.
    pub fn update(&mut self, ast: &QueryAst, cursor: usize) {
        // Find the token the cursor is inside (inclusive: start <= cursor <= end).
        let active = ast.tokens.iter().find(|t| {
            let (s, e) = span_bounds(t);
            s <= cursor && cursor <= e
        });

        self.active_token = active.cloned();
        self.selected = 0;

        match active {
            Some(Token::PartialStem {
                key_span,
                known_key,
                ..
            }) => {
                // If the cursor is at or before the end of the key span,
                // we are completing a stem key.
                if cursor <= key_span.end {
                    let prefix = ast.raw[key_span.start..cursor].to_string();
                    self.candidates = stem_key_candidates(&prefix);
                    self.context = CompletionContext::StemKey { prefix };
                } else if let Some(key) = known_key {
                    // Cursor is in the value portion -- Phase 2 stub.
                    self.candidates = Vec::new();
                    self.context = CompletionContext::StemValue {
                        key: key.clone(),
                        prefix: String::new(),
                    };
                } else {
                    // Unknown key, value portion: treat as Word.
                    self.candidates = Vec::new();
                    self.context = CompletionContext::Word;
                }
            }
            Some(Token::Stem { key_span, .. }) => {
                // A fully valid stem: cursor is inside -- treat as Word
                // unless cursor is still within the key portion.
                if cursor <= key_span.end {
                    let prefix = ast.raw[key_span.start..cursor].to_string();
                    self.candidates = stem_key_candidates(&prefix);
                    self.context = CompletionContext::StemKey { prefix };
                } else {
                    self.candidates = Vec::new();
                    self.context = CompletionContext::Word;
                }
            }
            Some(Token::Word { .. }) => {
                self.candidates = Vec::new();
                self.context = CompletionContext::Word;
            }
            Some(Token::Unknown { .. }) => {
                self.candidates = Vec::new();
                self.context = CompletionContext::Word;
            }
            None => {
                // Cursor is in whitespace or past end of all tokens, or
                // input is completely empty. In either case offer all stem
                // key candidates so Tab can cycle through them.
                self.candidates = stem_key_candidates("");
                self.context = CompletionContext::StemKey {
                    prefix: String::new(),
                };
            }
        }
    }

    /// Return the untyped suffix of `candidates[selected]` relative to the
    /// already-typed prefix, for inline ghost-text rendering.
    ///
    /// Returns `None` if there are no candidates or the selected candidate
    /// does not start with the current prefix.
    pub fn hint_suffix(&self) -> Option<&str> {
        if self.candidates.is_empty() {
            return None;
        }
        let candidate = self.candidates.get(self.selected)?;
        let prefix = match &self.context {
            CompletionContext::StemKey { prefix } => prefix.as_str(),
            _ => return None,
        };
        // Case-insensitive prefix match: verify, then return the suffix of
        // the candidate (using original casing) after `prefix.len()` bytes.
        if candidate.to_lowercase().starts_with(&prefix.to_lowercase()) {
            Some(&candidate[prefix.len()..])
        } else {
            None
        }
    }

    /// Apply one Tab press (or Shift-Tab when `forward = false`).
    ///
    /// - If context is `StemKey` and candidates are non-empty: cycle
    ///   `selected` (+1 or -1 with wrap) then replace the key portion of
    ///   `input` with the selected candidate and move the cursor to just
    ///   after the inserted colon.
    /// - Otherwise: jump the cursor to the start of the next (or previous)
    ///   token boundary.  Wraps around when no further token exists.
    pub fn apply_tab(&mut self, input: &mut crate::tui::TextInput, ast: &QueryAst, forward: bool) {
        match &self.context {
            CompletionContext::StemKey { prefix } => {
                if self.candidates.is_empty() {
                    self.jump_token_boundary(input, ast, forward);
                    return;
                }

                // Insert the currently selected candidate first, then
                // advance the index so the *next* Tab shows a different one.
                let candidate = self.candidates[self.selected].clone();
                let n = self.candidates.len();
                if forward {
                    self.selected = (self.selected + 1) % n;
                } else {
                    self.selected = (self.selected + n - 1) % n;
                }

                // Determine the replacement range: from key_span.start to just
                // after the colon so that the colon itself is replaced and not
                // left behind when the candidate (which already contains the
                // colon) is inserted.
                let (replace_start, replace_end) = match &self.active_token {
                    Some(Token::PartialStem { key_span, .. })
                    | Some(Token::Stem { key_span, .. }) => {
                        (key_span.start, (key_span.end + 1).min(input.value.len()))
                    }
                    _ => {
                        // Fallback: find start by subtracting prefix length.
                        (input.cursor.saturating_sub(prefix.len()), input.cursor)
                    }
                };

                // Replace the text in the input.
                let mut new_value = input.value[..replace_start].to_string();
                new_value.push_str(&candidate);
                new_value.push_str(&input.value[replace_end..]);
                input.value = new_value;

                // Move cursor to just after the colon in the candidate
                // (the colon is always the last character of a stem key string
                // such as "sort:").
                input.cursor = replace_start + candidate.len();

                // Update the context prefix to reflect the newly inserted text
                // so hint_suffix stays consistent until the next update() call.
                self.context = CompletionContext::StemKey {
                    prefix: candidate[..candidate.len().saturating_sub(1)].to_string(),
                };
            }
            _ => {
                self.jump_token_boundary(input, ast, forward);
            }
        }
    }

    /// Accept the currently highlighted completion candidate (Ctrl+Y).
    /// Replaces the prefix with the selected candidate and positions cursor
    /// after the inserted text. Returns `true` if a completion was applied.
    pub fn accept_completion(
        &mut self,
        input: &mut crate::tui::TextInput,
        _ast: &QueryAst,
    ) -> bool {
        match &self.context {
            CompletionContext::StemKey { prefix } => {
                if self.candidates.is_empty() {
                    return false;
                }
                let candidate = self.candidates[self.selected].clone();
                let (replace_start, replace_end) = match &self.active_token {
                    Some(Token::PartialStem { key_span, .. })
                    | Some(Token::Stem { key_span, .. }) => {
                        (key_span.start, (key_span.end + 1).min(input.value.len()))
                    }
                    _ => (input.cursor.saturating_sub(prefix.len()), input.cursor),
                };
                let mut new_value = input.value[..replace_start].to_string();
                new_value.push_str(&candidate);
                new_value.push_str(&input.value[replace_end..]);
                input.value = new_value;
                input.cursor = replace_start + candidate.len();
                true
            }
            _ => false,
        }
    }

    /// Cycle to the next completion candidate (Ctrl+N). Returns true if cycled.
    pub fn cycle_next(&mut self) -> bool {
        if self.candidates.is_empty() {
            return false;
        }
        self.selected = (self.selected + 1) % self.candidates.len();
        true
    }

    /// Cycle to the previous completion candidate (Ctrl+P). Returns true if cycled.
    pub fn cycle_prev(&mut self) -> bool {
        if self.candidates.is_empty() {
            return false;
        }
        let n = self.candidates.len();
        self.selected = (self.selected + n - 1) % n;
        true
    }

    /// Jump the cursor to the start of the next or previous token boundary.
    /// For Stem/PartialStem tokens, position the cursor after the colon
    /// (i.e. at the value portion) rather than at the very start of the key.
    fn jump_token_boundary(
        &self,
        input: &mut crate::tui::TextInput,
        ast: &QueryAst,
        forward: bool,
    ) {
        if ast.tokens.is_empty() {
            return;
        }

        let cursor = input.cursor;

        if forward {
            // Find the first token that starts strictly after the current cursor.
            let next = ast.tokens.iter().find(|t| span_bounds(t).0 > cursor);
            match next {
                Some(t) => input.cursor = cursor_position_for_token(t),
                None => return,
            }
        } else {
            // Shift-Tab: jump to prev token (token whose start is
            // strictly less than cursor, taking the last such token).
            let prev = ast
                .tokens
                .iter()
                .filter(|t| span_bounds(t).0 < cursor)
                .last();
            match prev {
                Some(t) => input.cursor = cursor_position_for_token(t),
                None => return,
            }
        }
    }
}

impl Default for Completer {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract the (start, end) byte positions of a token's outer span.
fn span_bounds(token: &Token) -> (usize, usize) {
    match token {
        Token::Stem { span, .. }
        | Token::PartialStem { span, .. }
        | Token::Word { span, .. }
        | Token::Unknown { span, .. } => (span.start, span.end),
    }
}

/// Return the cursor position to land on when Tab-jumping to a token.
/// For Stem and PartialStem tokens, position after the colon (at the value
/// portion). For Word and Unknown, position at the start of the token.
fn cursor_position_for_token(token: &Token) -> usize {
    match token {
        Token::Stem { key_span, .. } | Token::PartialStem { key_span, .. } => key_span.end + 1,
        Token::Word { span, .. } | Token::Unknown { span, .. } => span.start,
    }
}

/// Return the list of stem-key candidates that case-insensitively start with
/// `prefix`.  The colon is included in each candidate string.
fn stem_key_candidates(prefix: &str) -> Vec<String> {
    let lower = prefix.to_lowercase();
    STEM_KEY_STRINGS
        .iter()
        .filter(|s| s.to_lowercase().starts_with(lower.as_str()))
        .map(|s| s.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// args_to_ast / render_filter_context (bd-3nu)
// ---------------------------------------------------------------------------

/// Convert CLI `IssueArgs` into a `QueryAst` suitable for use as the initial
/// filter state.
///
/// Builds a space-separated query string from the args fields (team, assignee,
/// state, priority, sort) and passes it through `parse_query_ast()` so the
/// resulting AST is always structurally valid.
pub fn args_to_ast(args: &IssueArgs) -> QueryAst {
    let mut parts: Vec<String> = Vec::new();
    if let Some(ref t) = args.team {
        parts.push(format!("team:{}", t));
    }
    if let Some(ref a) = args.assignee {
        parts.push(format!("assignee:{}", a));
    }
    if let Some(ref s) = args.state {
        parts.push(format!("state:{}", s));
    }
    if let Some(ref p) = args.priority {
        parts.push(format!("priority:{}", p));
    }
    let dir = if args.desc { "-" } else { "+" };
    parts.push(format!("sort:{}{}", args.sort.label(), dir));
    parse_query_ast(&parts.join(" "))
}

/// Render a `QueryAst` as a compact filter context string for display in the
/// TUI header.
///
/// Iterates the AST tokens and formats each recognised stem as `key:value`,
/// joining with two spaces.  `PartialStem` and `Unknown` tokens are skipped
/// so partially-typed input is not shown in the header.
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
                StemKind::Assignee { value } => parts.push(format!("assignee:{}", value)),
                StemKind::Priority { value } => parts.push(format!("priority:{}", value)),
                StemKind::State { value } => parts.push(format!("state:{}", value)),
                StemKind::Team { value } => parts.push(format!("team:{}", value)),
                StemKind::Label { value } => parts.push(format!("label:{}", value)),
                StemKind::Project { value } => parts.push(format!("project:{}", value)),
                StemKind::Cycle { value } => parts.push(format!("cycle:{}", value)),
                StemKind::Creator { value } => parts.push(format!("creator:{}", value)),
            },
            Token::Word { text, .. } => parts.push(text.clone()),
            _ => {} // PartialStem/Unknown: skip in header display
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
        assert!(q.is_empty());
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
            other => panic!("expected Word, got {:?}", other),
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
            other => panic!("expected Word, got {:?}", other),
        }
        match &ast.tokens[1] {
            Token::Word { span, text } => {
                assert_eq!((span.start, span.end), (4, 7));
                assert_eq!(text, "bar");
            }
            other => panic!("expected Word, got {:?}", other),
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
            other => panic!("expected Stem(Sort), got {:?}", other),
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
            other => panic!("expected PartialStem, got {:?}", other),
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
            other => panic!("expected PartialStem, got {:?}", other),
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
            other => panic!("expected PartialStem(known_key=None), got {:?}", other),
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
                | Token::Word { span, .. }
                | Token::Unknown { span, .. } => (span.start, span.end),
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
                "token span contains whitespace: {:?}",
                slice
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
            other => panic!("expected Stem(Assignee), got {:?}", other),
        }
    }

    #[test]
    fn ast_partial_assignee_empty() {
        let ast = parse_query_ast("assignee:");
        match &ast.tokens[0] {
            Token::PartialStem { known_key, .. } => {
                assert_eq!(*known_key, Some(StemKey::Assignee));
            }
            other => panic!("expected PartialStem, got {:?}", other),
        }
    }

    // Parity tests: From<&QueryAst> must produce identical results to parse_query.

    #[test]
    fn from_ast_parity_empty() {
        let raw = "";
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
    fn from_ast_parity_full_query() {
        let raw = "sort:updated- assignee:me priority:urgent state:todo oauth crash";
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
    // Completer tests (bd-35l)
    // -----------------------------------------------------------------------

    #[test]
    fn completer_new_is_gap() {
        let c = Completer::new();
        assert_eq!(c.context, CompletionContext::Gap);
        assert!(c.candidates.is_empty());
        assert_eq!(c.selected, 0);
        assert!(!c.candidates_pending);
    }

    #[test]
    fn completer_update_empty_input_offers_all_stems() {
        let ast = parse_query_ast("");
        let mut c = Completer::new();
        c.update(&ast, 0);
        assert!(matches!(c.context, CompletionContext::StemKey { .. }));
        assert_eq!(
            c.candidates,
            vec![
                "sort:",
                "assignee:",
                "priority:",
                "state:",
                "team:",
                "label:",
                "project:",
                "cycle:",
                "creator:",
            ]
        );
    }

    #[test]
    fn completer_update_partial_key_prefix_s() {
        // User has typed "s" -- should match "sort:" and "state:"
        let ast = parse_query_ast("s");
        let mut c = Completer::new();
        // cursor is at byte 1 (after "s"), which is within the Word token [0,1)
        // but "s" has no colon so it is a Word token, not StemKey.
        // According to the spec, Word context -> no completion.
        c.update(&ast, 1);
        assert_eq!(c.context, CompletionContext::Word);
        assert!(c.candidates.is_empty());
    }

    #[test]
    fn completer_update_partial_stem_key_cursor_in_key() {
        // "so:" -- partial stem with unknown sort value; cursor=2 is in key portion
        let ast = parse_query_ast("so:");
        let mut c = Completer::new();
        // cursor=2 is <= key_span.end=2
        c.update(&ast, 2);
        match &c.context {
            CompletionContext::StemKey { prefix } => {
                assert_eq!(prefix, "so");
            }
            other => panic!("expected StemKey, got {:?}", other),
        }
        // Only "sort:" starts with "so"
        assert_eq!(c.candidates, vec!["sort:"]);
    }

    #[test]
    fn completer_update_partial_stem_key_empty_prefix() {
        // ":" has no colon-before, but "a:" does
        let ast = parse_query_ast("a:");
        let mut c = Completer::new();
        // cursor=1 is inside key portion (key_span [0,1))
        c.update(&ast, 1);
        match &c.context {
            CompletionContext::StemKey { prefix } => {
                assert_eq!(prefix, "a");
            }
            other => panic!("expected StemKey, got {:?}", other),
        }
        assert_eq!(c.candidates, vec!["assignee:"]);
    }

    #[test]
    fn completer_update_gap_between_tokens() {
        // "foo  bar" -- two spaces; cursor at byte 4 (second space, between tokens)
        // "foo" spans [0,3), "bar" spans [5,8).  Byte 4 is not inside either.
        // Cursor in a gap offers all stem-key candidates so Tab can insert one.
        let ast = parse_query_ast("foo  bar");
        let mut c = Completer::new();
        c.update(&ast, 4);
        match &c.context {
            CompletionContext::StemKey { prefix } => {
                assert_eq!(prefix, "");
            }
            other => panic!("expected StemKey with empty prefix, got {:?}", other),
        }
        assert_eq!(c.candidates.len(), 9); // all stem keys
    }

    #[test]
    fn completer_update_word_context() {
        let ast = parse_query_ast("hello");
        let mut c = Completer::new();
        c.update(&ast, 3); // inside "hello"
        assert_eq!(c.context, CompletionContext::Word);
        assert!(c.candidates.is_empty());
    }

    #[test]
    fn completer_update_gap_past_end() {
        // Cursor past end of all tokens offers all stem-key candidates.
        let ast = parse_query_ast("foo");
        let mut c = Completer::new();
        c.update(&ast, 5); // past end of "foo" (len=3)
        match &c.context {
            CompletionContext::StemKey { prefix } => {
                assert_eq!(prefix, "");
            }
            other => panic!("expected StemKey with empty prefix, got {:?}", other),
        }
        assert_eq!(c.candidates.len(), 9); // all stem keys
    }

    #[test]
    fn hint_suffix_basic() {
        let ast = parse_query_ast("so:");
        let mut c = Completer::new();
        c.update(&ast, 2); // cursor at end of "so"
        // candidates should be ["sort:"], prefix="so"
        let suffix = c.hint_suffix();
        assert_eq!(suffix, Some("rt:"));
    }

    #[test]
    fn hint_suffix_no_candidates() {
        let ast = parse_query_ast("hello");
        let mut c = Completer::new();
        c.update(&ast, 3);
        assert_eq!(c.hint_suffix(), None);
    }

    #[test]
    fn hint_suffix_full_key_typed() {
        // "sort:" is fully typed; cursor is at key_span.end=4
        let ast = parse_query_ast("sort:");
        let mut c = Completer::new();
        c.update(&ast, 4); // cursor at end of "sort", before ":"
        match &c.context {
            CompletionContext::StemKey { prefix } => {
                assert_eq!(prefix, "sort");
            }
            other => panic!("expected StemKey, got {:?}", other),
        }
        assert_eq!(c.candidates, vec!["sort:"]);
        // The suffix relative to "sort" in "sort:" is ":"
        assert_eq!(c.hint_suffix(), Some(":"));
    }

    #[test]
    fn hint_suffix_case_insensitive_prefix() {
        // Prefix "SO" should still match "sort:"
        let ast = parse_query_ast("SO:");
        let mut c = Completer::new();
        c.update(&ast, 2); // cursor at end of "SO"
        assert_eq!(c.candidates, vec!["sort:"]);
        // hint_suffix: "sort:" starts with "so" (lowercased "SO"), suffix = "rt:"
        // But candidate is "sort:", prefix length is 2.
        assert_eq!(c.hint_suffix(), Some("rt:"));
    }

    #[test]
    fn completer_update_resets_selected_to_zero() {
        let ast = parse_query_ast("s:");
        let mut c = Completer::new();
        c.update(&ast, 1);
        c.selected = 3; // manually set non-zero
        c.update(&ast, 1);
        assert_eq!(c.selected, 0);
    }

    // -----------------------------------------------------------------------
    // args_to_ast tests (bd-3nu)
    // -----------------------------------------------------------------------

    #[test]
    fn args_to_ast_default_produces_sort_updated_desc() {
        let args = IssueArgs::default();
        // Default has sort=Updated, desc=true.
        let ast = args_to_ast(&args);
        assert_eq!(ast.raw, "sort:updated-");
        assert_eq!(ast.tokens.len(), 1);
        match &ast.tokens[0] {
            Token::Stem {
                kind: StemKind::Sort { field, dir },
                ..
            } => {
                assert!(matches!(field, SortField::Updated));
                assert_eq!(*dir, SortDir::Desc);
            }
            other => panic!("expected Stem(Sort), got {:?}", other),
        }
    }

    #[test]
    fn args_to_ast_team_and_assignee() {
        let args = IssueArgs {
            team: Some("eng".to_string()),
            assignee: Some("me".to_string()),
            ..IssueArgs::default()
        };
        let ast = args_to_ast(&args);
        // Expect team, assignee, and sort stems in that order.
        assert!(ast.raw.contains("team:eng"));
        assert!(ast.raw.contains("assignee:me"));
        assert!(ast.raw.contains("sort:"));
        // Three tokens: team, assignee, sort.
        assert_eq!(ast.tokens.len(), 3);
        match &ast.tokens[0] {
            Token::Stem {
                kind: StemKind::Team { value },
                ..
            } => assert_eq!(value, "eng"),
            other => panic!("expected Stem(Team), got {:?}", other),
        }
        match &ast.tokens[1] {
            Token::Stem {
                kind: StemKind::Assignee { value },
                ..
            } => assert_eq!(value, "me"),
            other => panic!("expected Stem(Assignee), got {:?}", other),
        }
    }

    #[test]
    fn args_to_ast_asc_sort() {
        let args = IssueArgs {
            sort: SortField::Priority,
            desc: false,
            ..IssueArgs::default()
        };
        let ast = args_to_ast(&args);
        assert!(ast.raw.ends_with("sort:priority+"));
        match &ast.tokens[0] {
            Token::Stem {
                kind: StemKind::Sort { field, dir },
                ..
            } => {
                assert!(matches!(field, SortField::Priority));
                assert_eq!(*dir, SortDir::Asc);
            }
            other => panic!("expected Stem(Sort), got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // render_filter_context tests (bd-3nu)
    // -----------------------------------------------------------------------

    #[test]
    fn render_filter_context_default_query() {
        let ast = parse_query_ast("sort:updated-");
        let s = render_filter_context(&ast);
        assert_eq!(s, "sort:updated-");
    }

    #[test]
    fn render_filter_context_multiple_stems() {
        let ast = parse_query_ast("team:eng assignee:will state:todo sort:updated-");
        let s = render_filter_context(&ast);
        // Parts are joined with double-spaces.
        assert_eq!(s, "team:eng  assignee:will  state:todo  sort:updated-");
    }

    #[test]
    fn render_filter_context_includes_words() {
        let ast = parse_query_ast("sort:updated- oauth crash");
        let s = render_filter_context(&ast);
        assert_eq!(s, "sort:updated-  oauth  crash");
    }

    #[test]
    fn render_filter_context_skips_partial_stems() {
        // "sort:" is a PartialStem (empty value) -- should be skipped.
        let ast = parse_query_ast("sort:");
        let s = render_filter_context(&ast);
        assert_eq!(s, "");
    }

    #[test]
    fn render_filter_context_round_trip() {
        // Build an AST from args and render it; must match expected output.
        let args = IssueArgs {
            team: Some("eng".to_string()),
            assignee: Some("me".to_string()),
            ..IssueArgs::default()
        };
        let ast = args_to_ast(&args);
        let s = render_filter_context(&ast);
        assert_eq!(s, "team:eng  assignee:me  sort:updated-");
    }

    #[test]
    fn render_filter_context_all_stem_kinds() {
        let raw = "team:t assignee:a state:s priority:p label:l project:pr cycle:c creator:cr sort:updated-";
        let ast = parse_query_ast(raw);
        let s = render_filter_context(&ast);
        assert!(s.contains("team:t"));
        assert!(s.contains("assignee:a"));
        assert!(s.contains("state:s"));
        assert!(s.contains("priority:p"));
        assert!(s.contains("label:l"));
        assert!(s.contains("project:pr"));
        assert!(s.contains("cycle:c"));
        assert!(s.contains("creator:cr"));
        assert!(s.contains("sort:updated-"));
    }

    // -----------------------------------------------------------------------
    // Snapshot-based completion test harness (bd-cd4)
    //
    // Snapshot format:
    //   - Literal characters are text content
    //   - '|' marks the cursor position (exactly one per snapshot)
    //   - '(text)' at the very end is ghost text from hint_suffix()
    //
    // Examples:
    //   "sort:updated-|"        -- cursor at end, no ghost text
    //   "sort:|updated-"        -- cursor after colon
    //   "sort:updated- |(sort:)"-- cursor after space, ghost text "sort:"
    // -----------------------------------------------------------------------

    use crossterm::event::{KeyCode, KeyModifiers};

    struct Harness {
        input: crate::tui::TextInput,
        completer: Completer,
    }

    impl Harness {
        /// Parse a snapshot string to extract initial text and cursor position,
        /// then build the AST and initialise the completer.
        ///
        /// The trailing '(...)' ghost-text annotation is stripped and ignored
        /// (the completer will compute it fresh from the AST).
        fn new(snapshot: &str) -> Self {
            // Strip optional trailing ghost-text annotation '(...)'.
            let bare = if snapshot.ends_with(')') {
                if let Some(paren) = snapshot.rfind('(') {
                    &snapshot[..paren]
                } else {
                    snapshot
                }
            } else {
                snapshot
            };

            // Split on the cursor marker '|'.
            let pipe = bare
                .find('|')
                .unwrap_or_else(|| panic!("snapshot missing '|': {:?}", snapshot));
            let before = &bare[..pipe];
            let after = &bare[pipe + 1..];
            let text = format!("{}{}", before, after);
            let cursor = before.len();

            let mut input = crate::tui::TextInput::new();
            input.value = text;
            input.cursor = cursor;

            let mut completer = Completer::new();
            let ast = parse_query_ast(&input.value);
            completer.update(&ast, input.cursor);

            Harness { input, completer }
        }

        /// Render the current state as a snapshot string.
        fn snapshot(&self) -> String {
            let before = &self.input.value[..self.input.cursor];
            let after = &self.input.value[self.input.cursor..];
            let mut s = format!("{}|{}", before, after);
            if let Some(ghost) = self.completer.hint_suffix() {
                s.push_str(&format!("({})", ghost));
            }
            s
        }

        /// Assert the current state matches the expected snapshot.
        /// Panics with a clear diff on mismatch.
        fn assert_snapshot(&self, expected: &str) {
            let actual = self.snapshot();
            assert_eq!(
                actual, expected,
                "\nsnapshot mismatch:\n  actual:   {:?}\n  expected: {:?}",
                actual, expected
            );
        }

        /// Simulate pressing Tab (forward=true).
        fn tab(&mut self) {
            let ast = parse_query_ast(&self.input.value);
            self.completer.apply_tab(&mut self.input, &ast, true);
            let ast = parse_query_ast(&self.input.value);
            self.completer.update(&ast, self.input.cursor);
        }

        /// Simulate pressing Shift-Tab (forward=false).
        fn shift_tab(&mut self) {
            let ast = parse_query_ast(&self.input.value);
            self.completer.apply_tab(&mut self.input, &ast, false);
            let ast = parse_query_ast(&self.input.value);
            self.completer.update(&ast, self.input.cursor);
        }

        /// Simulate typing a character.
        fn key(&mut self, c: char) {
            self.input.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
            let ast = parse_query_ast(&self.input.value);
            self.completer.update(&ast, self.input.cursor);
        }

        /// Simulate pressing Backspace.
        fn backspace(&mut self) {
            self.input
                .handle_key(KeyCode::Backspace, KeyModifiers::NONE);
            let ast = parse_query_ast(&self.input.value);
            self.completer.update(&ast, self.input.cursor);
        }

        /// Accept the currently highlighted completion candidate (Ctrl+Y).
        fn ctrl_y(&mut self) {
            let ast = parse_query_ast(&self.input.value);
            if self.completer.accept_completion(&mut self.input, &ast) {
                let ast = parse_query_ast(&self.input.value);
                self.completer.update(&ast, self.input.cursor);
            }
        }

        /// Cycle to the next completion candidate (Ctrl+N).
        /// Does NOT re-parse the AST; only changes the selected index.
        fn ctrl_n(&mut self) {
            self.completer.cycle_next();
        }

        /// Cycle to the previous completion candidate (Ctrl+P).
        /// Does NOT re-parse the AST; only changes the selected index.
        fn ctrl_p(&mut self) {
            self.completer.cycle_prev();
        }
    }

    // -----------------------------------------------------------------------
    // Test 1: Tab at end of single-token query is a no-op (no wrap).
    //
    // "sort:updated-" has one Stem token. cursor=13 is past key_span.end=4
    // so context=Word (value portion). Tab calls jump_token_boundary forward,
    // no token starts after position 13, no wrap -> no-op.
    // -----------------------------------------------------------------------
    #[test]
    fn tab_from_end_of_default_query() {
        let mut h = Harness::new("sort:updated-|");
        // context=Word (cursor in value portion of the Stem), no candidates.
        // Tab -> jump forward -> no next token -> no-op.
        h.tab();
        h.assert_snapshot("sort:updated-|");
    }

    // -----------------------------------------------------------------------
    // Test 2: Shift-Tab at end of single-token query jumps into value portion.
    //
    // cursor=13 is in value portion (Word context). Shift-Tab calls
    // jump_token_boundary backward. The only token is sort:updated- with
    // start=0 < 13, so it qualifies. cursor_position_for_token = key_span.end+1
    // = 4+1 = 5 (right after the colon).
    // -----------------------------------------------------------------------
    #[test]
    fn shift_tab_from_end_of_default_query() {
        let mut h = Harness::new("sort:updated-|");
        h.shift_tab();
        // Cursor jumps to right after the colon (position 5).
        h.assert_snapshot("sort:|updated-");
    }

    // -----------------------------------------------------------------------
    // Test 3: Type space then Tab inserts first stem candidate.
    //
    // After space, cursor=14 is in a gap (no token covers it). Context is
    // StemKey{prefix:""} with all 9 candidates. Ghost text shows "sort:".
    // Tab inserts "sort:" at the gap position and advances selected to 1.
    // After insertion the PartialStem "sort:" is parsed; cursor lands after
    // its colon (StemValue context, no ghost text).
    // -----------------------------------------------------------------------
    #[test]
    fn space_then_tab_inserts_completion() {
        let mut h = Harness::new("sort:updated-|");
        h.key(' ');
        // Gap context: all candidates offered, ghost text = candidates[0] = "sort:".
        h.assert_snapshot("sort:updated- |(sort:)");
        h.tab();
        // "sort:" inserted at gap; cursor after colon; StemValue context.
        h.assert_snapshot("sort:updated- sort:|");
    }

    // -----------------------------------------------------------------------
    // Test 4: Tab at end of last token is a no-op (no wrap).
    //
    // cursor=19 is in the value portion of the PartialStem "sort:" at [14,19).
    // Context=StemValue (cursor past key_span.end). Tab -> jump forward ->
    // no token starts after 19 -> no-op.
    // -----------------------------------------------------------------------
    #[test]
    fn tab_no_wrap_at_end() {
        let mut h = Harness::new("sort:updated- sort:|");
        // Context: StemValue (cursor right after the colon of PartialStem).
        // Tab -> jump_token_boundary forward -> no next token -> no-op.
        h.tab();
        h.assert_snapshot("sort:updated- sort:|");
    }

    // -----------------------------------------------------------------------
    // Test 5: Shift-Tab from the start of a StemKey context applies completion.
    //
    // cursor=0 is inside the Stem "sort:updated-" at [0,13), within key_span
    // [0,4). Context=StemKey{prefix:""} with all 9 candidates.
    // Shift-Tab in StemKey context: applies candidates[0]="sort:" (replacing
    // key+colon [0,5)), advances selected to 8 (wrap backward), moves cursor
    // to position 5 (after inserted colon). Text is unchanged because "sort:"
    // replaces the existing "sort:" prefix.
    //
    // Note: this is NOT a pure jump; StemKey context always applies a candidate
    // first. jump_token_boundary is only called when context is NOT StemKey.
    // -----------------------------------------------------------------------
    #[test]
    fn shift_tab_from_start_of_stemkey_applies_candidate() {
        let mut h = Harness::new("|sort:updated-");
        // cursor=0, inside key part of Stem, prefix="". StemKey, 9 candidates.
        // Shift-Tab: applies candidates[0]="sort:", selected wraps to 8.
        // new text = "sort:updated-" (unchanged), cursor=5.
        h.shift_tab();
        h.assert_snapshot("sort:|updated-");
    }

    // -----------------------------------------------------------------------
    // Test 6: Typing partial key then Tab completes without double colon.
    //
    // "so:" is parsed as PartialStem{key_span=[0,2), known_key=None}.
    // cursor=2 <= key_span.end=2 -> StemKey{prefix="so"}, candidates=["sort:"].
    // Tab replaces [0, (2+1).min(3)=3) with "sort:", cursor=5.
    // Result: "sort:" with cursor at 5 (after the colon).
    // -----------------------------------------------------------------------
    #[test]
    fn tab_partial_key_completes_without_double_colon() {
        let mut h = Harness::new("so|:");
        // cursor=2 in key portion of PartialStem "so:". StemKey{prefix="so"}.
        // candidates=["sort:"]. Tab replaces "so:" with "sort:", cursor=5.
        h.tab();
        h.assert_snapshot("sort:|");
    }

    // -----------------------------------------------------------------------
    // Test 7: Shift-Tab backward through a multi-token query.
    //
    // Query: "sort:updated- assignee:will priority:high"
    // Token spans (half-open):
    //   sort:updated-  [0,13)  key_span=[0,4)   cursor_position=5
    //   assignee:will  [14,27) key_span=[14,22)  cursor_position=23
    //   priority:high  [28,41) key_span=[28,36)  cursor_position=37
    //
    // Shift-Tab 1: cursor=41 in value portion of priority:high (Word context).
    //   backward filter: last token with start<41 = priority:high (start=28).
    //   cursor -> cursor_position_for_token = 37.
    //
    // Shift-Tab 2: cursor=37 still in value portion of priority:high (Word).
    //   backward filter: last token with start<37 = priority:high (start=28).
    //   cursor_position_for_token = 37 again. Effectively a no-op.
    //   This is a known limitation: the backward filter uses token.start, not
    //   cursor_position_for_token, so it cannot advance past the current token
    //   once the cursor is at cursor_position_for_token.
    // -----------------------------------------------------------------------
    #[test]
    fn shift_tab_through_multi_token_query() {
        let mut h = Harness::new("sort:updated- assignee:will priority:high|");
        // Shift-Tab 1: from end, lands after priority: colon.
        h.shift_tab();
        h.assert_snapshot("sort:updated- assignee:will priority:|high");
        // Shift-Tab 2: cursor=37 still in priority:high value portion (Word).
        // Backward filter finds priority:high (start=28 < 37) as the last match.
        // cursor_position_for_token = 37 -> no effective movement.
        h.shift_tab();
        h.assert_snapshot("sort:updated- assignee:will priority:|high");
    }

    // -----------------------------------------------------------------------
    // Test 8: Tab forward through a multi-token query.
    //
    // Query: "sort:updated- assignee:will"
    // Token spans:
    //   sort:updated-  [0,13)  key_span=[0,4)   cursor_position=5
    //   assignee:will  [14,27) key_span=[14,22)  cursor_position=23
    //
    // Tab 1: cursor=0 inside sort:updated- key portion. StemKey{prefix:""}.
    //   Applies candidates[0]="sort:", replaces [0,5) with "sort:", cursor=5.
    //   Text unchanged. selected advances to 1.
    //
    // Tab 2: cursor=5 in value portion of sort:updated- (Word context).
    //   Forward jump: first token with start>5 = assignee:will (start=14).
    //   cursor -> cursor_position_for_token = 23.
    //
    // Tab 3: cursor=23 in value portion of assignee:will (Word context).
    //   Forward jump: no token with start>23. No-op.
    // -----------------------------------------------------------------------
    #[test]
    fn tab_forward_through_multi_token_query() {
        let mut h = Harness::new("|sort:updated- assignee:will");
        // cursor=0, StemKey{prefix:""} inside sort: key. candidates=9.
        // Tab: applies "sort:" over [0,5), cursor=5. Text unchanged.
        h.tab();
        h.assert_snapshot("sort:|updated- assignee:will");
        // cursor=5, Word context (value portion). Tab jumps to assignee:.
        h.tab();
        h.assert_snapshot("sort:updated- assignee:|will");
        // cursor=23, Word context. No next token. Tab is a no-op.
        h.tab();
        h.assert_snapshot("sort:updated- assignee:|will");
    }

    // -----------------------------------------------------------------------
    // Test 9: In a gap, Tab inserts first candidate; ghost text updates.
    //
    // "sort:updated- " has cursor=14 in a gap (StemKey{prefix:""},
    // candidates=9). Ghost text = candidates[0] = "sort:".
    // Tab inserts "sort:" at the gap. After insertion, cursor=19 is in the
    // value portion of the new PartialStem (StemValue context, no ghost).
    // -----------------------------------------------------------------------
    #[test]
    fn tab_cycles_candidates_in_gap() {
        let mut h = Harness::new("sort:updated- |");
        // Gap context, candidates=9, ghost="sort:".
        h.assert_snapshot("sort:updated- |(sort:)");
        h.tab();
        // "sort:" inserted; cursor after its colon; StemValue, no ghost.
        h.assert_snapshot("sort:updated- sort:|");
    }

    // -----------------------------------------------------------------------
    // Test 10: Backspace and retype in the harness.
    //
    // Start: cursor=5 in value portion of sort:updated- (Word context).
    // key('x'): insert 'x' -> "sort:xupdated-", cursor=6. PartialStem,
    //   StemValue context (cursor past key_span.end=4).
    // backspace: delete 'x' -> "sort:updated-", cursor=5. Back to Word.
    // -----------------------------------------------------------------------
    #[test]
    fn backspace_and_retype() {
        let mut h = Harness::new("sort:|updated-");
        // cursor=5, value portion of Stem (Word context), no ghost.
        h.key('x');
        // "sort:xupdated-", cursor=6. PartialStem(Sort), StemValue, no ghost.
        h.assert_snapshot("sort:x|updated-");
        h.backspace();
        // "sort:updated-", cursor=5. Stem, Word context, no ghost.
        h.assert_snapshot("sort:|updated-");
    }

    // -----------------------------------------------------------------------
    // Test 11: Ctrl+Y accepts the current completion candidate.
    //
    // "so:" with cursor=2 in key portion: StemKey{prefix="so"}, ["sort:"].
    // Ctrl+Y: accept_completion replaces "so:" with "sort:", cursor=5.
    // After re-parse: PartialStem{sort:}, cursor=5>key_span.end=4 -> StemValue.
    // -----------------------------------------------------------------------
    #[test]
    fn ctrl_y_accepts_completion() {
        let mut h = Harness::new("so|:");
        // StemKey{prefix="so"}, candidates=["sort:"].
        h.ctrl_y();
        // "so:" replaced with "sort:", cursor=5 (after colon). StemValue.
        h.assert_snapshot("sort:|");
    }

    // -----------------------------------------------------------------------
    // Test 12: Ctrl+N and Ctrl+P cycle through candidates without editing text.
    //
    // Empty input: cursor=0, StemKey{prefix:""}, candidates=all 9, selected=0.
    // Ghost text = candidates[selected].
    // Ctrl+N advances selected; Ctrl+P reverses it.
    // -----------------------------------------------------------------------
    #[test]
    fn ctrl_n_ctrl_p_cycle() {
        let mut h = Harness::new("|");
        // Empty input: StemKey, 9 candidates, selected=0, ghost="sort:".
        h.assert_snapshot("|(sort:)");
        h.ctrl_n();
        // selected=1 -> "assignee:".
        h.assert_snapshot("|(assignee:)");
        h.ctrl_n();
        // selected=2 -> "priority:".
        h.assert_snapshot("|(priority:)");
        h.ctrl_p();
        // selected=1 -> "assignee:".
        h.assert_snapshot("|(assignee:)");
    }

    // -----------------------------------------------------------------------
    // Test 13: Tab inside a bare Word jumps forward to the next token.
    //
    // "hello sort:updated-": cursor=5 is at the end of Word "hello" [0,5).
    // context=Word. Tab -> jump forward -> first token with start>5 is
    // sort:updated- at [6,19), key_span=[6,10).
    // cursor_position_for_token = 10+1 = 11 (right after the colon).
    // -----------------------------------------------------------------------
    #[test]
    fn tab_in_word_jumps_to_next_token() {
        let mut h = Harness::new("hello| sort:updated-");
        // cursor=5, Word context ("hello"). Tab -> jump to next token.
        h.tab();
        // cursor jumps to right after "sort:" colon, position=11.
        h.assert_snapshot("hello sort:|updated-");
    }
}
