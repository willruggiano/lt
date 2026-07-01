//! Tab-completion for the search query bar.
//!
//! The query *parser* and SQL builder live in `lt_runtime::search_query`; this
//! module is the TUI-side completion that drives a [`crate::text_input::TextInput`]
//! against that parser's tokens.

use lt_runtime::search_query::{QueryAst, StemKey, Token, parse_query_ast};

use crate::text_input::TextInput;

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
}

impl Completer {
    /// Create a new `Completer` with `Gap` context and empty candidates.
    pub fn new() -> Self {
        Completer {
            active_token: None,
            context: CompletionContext::Gap,
            candidates: Vec::new(),
            selected: 0,
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
    /// - If `input.selection_end` is set: accept the current selection by
    ///   moving the cursor to `selection_end` and clearing the selection,
    ///   then return without jumping further.
    /// - If context is `StemKey` and candidates are non-empty: cycle
    ///   `selected` (+1 or -1 with wrap) then replace the key portion of
    ///   `input` with the selected candidate and move the cursor to just
    ///   after the inserted colon.
    /// - Otherwise: jump the cursor to the start of the next (or previous)
    ///   token boundary.  Wraps around when no further token exists.
    pub fn apply_tab(&mut self, input: &mut TextInput, ast: &QueryAst, forward: bool) {
        // If a selection is active, accept it then jump in the requested direction.
        //
        // For Tab (forward): advance cursor to selection_end so the current
        // token is "behind" us, then jump to the next token.
        //
        // For Shift-Tab (backward): leave cursor where it is (start of the
        // selection, i.e. just after the colon). The current token's
        // cursor_position_for_token equals cursor exactly, so it does NOT
        // satisfy cursor_position_for_token < cursor and is excluded from the
        // backward search -- we land on the genuinely previous token.
        if let Some(end) = input.selection_end.take() {
            if forward {
                input.cursor = end;
            }
            let new_ast = parse_query_ast(&input.value);
            self.update(&new_ast, input.cursor);
            Self::jump_token_boundary(input, &new_ast, forward);
            return;
        }

        match &self.context {
            CompletionContext::StemKey { prefix } => {
                if self.candidates.is_empty() {
                    Self::jump_token_boundary(input, ast, forward);
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
                    Some(Token::PartialStem { key_span, .. } | Token::Stem { key_span, .. }) => {
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
                Self::jump_token_boundary(input, ast, forward);
            }
        }
    }

    /// Accept the currently highlighted completion candidate (Ctrl+Y).
    /// Replaces the prefix with the selected candidate and positions cursor
    /// after the inserted text. Returns `true` if a completion was applied.
    pub fn accept_completion(&mut self, input: &mut TextInput, _ast: &QueryAst) -> bool {
        match &self.context {
            CompletionContext::StemKey { prefix } => {
                if self.candidates.is_empty() {
                    return false;
                }
                let candidate = self.candidates[self.selected].clone();
                let (replace_start, replace_end) = match &self.active_token {
                    Some(Token::PartialStem { key_span, .. } | Token::Stem { key_span, .. }) => {
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
    /// When landing on a Stem token that has a non-empty value, set
    /// `input.selection_end` to the end of the token span so that the value is
    /// "selected" and typing immediately replaces it.  `PartialStem` tokens
    /// (empty value) do NOT set a selection.
    fn jump_token_boundary(input: &mut TextInput, ast: &QueryAst, forward: bool) {
        if ast.tokens.is_empty() {
            return;
        }

        let cursor = input.cursor;

        if forward {
            // Find the first token that starts strictly after the current cursor.
            let next = ast.tokens.iter().find(|t| span_bounds(t).0 > cursor);
            if let Some(t) = next {
                input.cursor = cursor_position_for_token(t);
                input.selection_end = selection_end_for_token(t);
            }
        } else {
            // Shift-Tab: jump to prev token.  We exclude the token the cursor
            // is already sitting at by comparing against cursor_position_for_token
            // rather than the raw token start.  This prevents getting stuck when
            // the cursor is exactly at cursor_position_for_token(t): the old
            // span_bounds(t).0 < cursor check would still match the current token
            // (its start is before the cursor), so cursor_position_for_token would
            // return the same position and the jump would be a no-op.
            let prev = ast
                .tokens
                .iter()
                .rfind(|t| cursor_position_for_token(t) < cursor);
            if let Some(t) = prev {
                input.cursor = cursor_position_for_token(t);
                input.selection_end = selection_end_for_token(t);
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
        Token::Stem { span, .. } | Token::PartialStem { span, .. } | Token::Word { span, .. } => {
            (span.start, span.end)
        }
    }
}

/// Return the cursor position to land on when Tab-jumping to a token.
/// For Stem and `PartialStem` tokens, position after the colon (at the value
/// portion). For Word, position at the start of the token.
fn cursor_position_for_token(token: &Token) -> usize {
    match token {
        Token::Stem { key_span, .. } | Token::PartialStem { key_span, .. } => key_span.end + 1,
        Token::Word { span, .. } => span.start,
    }
}

/// Return the `selection_end` value to set when Tab-jumping to a token.
/// For a Stem token with a non-empty value span, returns Some(span.end) so
/// that the value is "selected" and typing replaces it immediately.
/// `PartialStem` tokens (empty or invalid value) return None -- there is
/// nothing to select.
/// Word tokens also return None.
fn selection_end_for_token(token: &Token) -> Option<usize> {
    match token {
        Token::Stem { span, val_span, .. } => {
            if val_span.start < val_span.end {
                Some(span.end)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Return the list of stem-key candidates that case-insensitively start with
/// `prefix`.  The colon is included in each candidate string.
fn stem_key_candidates(prefix: &str) -> Vec<String> {
    let lower = prefix.to_lowercase();
    STEM_KEY_STRINGS
        .iter()
        .filter(|s| s.to_lowercase().starts_with(lower.as_str()))
        .map(std::string::ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use lt_runtime::query::{IssueQuery, SortField};
    use lt_runtime::search_query::{
        SortDir, StemKind, Token, args_to_ast, parse_query_ast, render_filter_context,
    };

    use super::*;
    use crate::text_input::TextInput;

    // Completer tests (bd-35l)
    // -----------------------------------------------------------------------

    #[test]
    fn completer_new_is_gap() {
        let c = Completer::new();
        assert_eq!(c.context, CompletionContext::Gap);
        assert!(c.candidates.is_empty());
        assert_eq!(c.selected, 0);
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
            other => panic!("expected StemKey, got {other:?}"),
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
            other => panic!("expected StemKey, got {other:?}"),
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
            other => panic!("expected StemKey with empty prefix, got {other:?}"),
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
            other => panic!("expected StemKey with empty prefix, got {other:?}"),
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
            other => panic!("expected StemKey, got {other:?}"),
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
        let args = IssueQuery::default();
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
            other => panic!("expected Stem(Sort), got {other:?}"),
        }
    }

    #[test]
    fn args_to_ast_team_and_assignee() {
        let args = IssueQuery {
            team: Some("eng".to_string()),
            assignee: Some("me".to_string()),
            ..IssueQuery::default()
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
            other => panic!("expected Stem(Team), got {other:?}"),
        }
        match &ast.tokens[1] {
            Token::Stem {
                kind: StemKind::Assignee { value },
                ..
            } => assert_eq!(value, "me"),
            other => panic!("expected Stem(Assignee), got {other:?}"),
        }
    }

    #[test]
    fn args_to_ast_asc_sort() {
        let args = IssueQuery {
            sort: SortField::Priority,
            desc: false,
            ..IssueQuery::default()
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
            other => panic!("expected Stem(Sort), got {other:?}"),
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
        let args = IssueQuery {
            team: Some("eng".to_string()),
            assignee: Some("me".to_string()),
            ..IssueQuery::default()
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
    //   - '[text]' immediately after '|' marks the selected text (cursor..sel_end)
    //   - '(text)' at the very end is ghost text from hint_suffix()
    //
    // Examples:
    //   "sort:updated-|"          -- cursor at end, no ghost text
    //   "sort:|updated-"          -- cursor after colon
    //   "sort:updated- |(sort:)"  -- cursor after space, ghost text "sort:"
    //   "priority:|[high]"        -- cursor after colon, "high" selected
    // -----------------------------------------------------------------------

    use crossterm::event::{KeyCode, KeyModifiers};

    struct Harness {
        input: TextInput,
        completer: Completer,
    }

    impl Harness {
        /// Parse a snapshot string to extract initial text, cursor position,
        /// and optional `selection_end`, then build the AST and initialise
        /// the completer.
        ///
        /// The trailing '(...)' ghost-text annotation is stripped and ignored
        /// (the completer will compute it fresh from the AST).
        /// A '[...]' immediately after '|' encodes the selected text; the
        /// brackets themselves are not part of the value.
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
                .unwrap_or_else(|| panic!("snapshot missing '|': {snapshot:?}"));
            let before = &bare[..pipe];
            let rest = &bare[pipe + 1..];

            // Check for optional selection '[...]' immediately after '|'.
            let (sel_text, after) = if rest.starts_with('[') {
                if let Some(close) = rest.find(']') {
                    (&rest[1..close], &rest[close + 1..])
                } else {
                    ("", rest)
                }
            } else {
                ("", rest)
            };

            let text = format!("{before}{sel_text}{after}");
            let cursor = before.len();
            let selection_end = if sel_text.is_empty() {
                None
            } else {
                Some(cursor + sel_text.len())
            };

            let mut input = TextInput::new();
            input.value = text;
            input.cursor = cursor;
            input.selection_end = selection_end;

            let mut completer = Completer::new();
            let ast = parse_query_ast(&input.value);
            completer.update(&ast, input.cursor);

            Harness { input, completer }
        }

        /// Render the current state as a snapshot string.
        fn snapshot(&self) -> String {
            let before = &self.input.value[..self.input.cursor];
            let sel_end = self
                .input
                .selection_end
                .unwrap_or(self.input.cursor)
                .min(self.input.value.len());
            let sel_text = &self.input.value[self.input.cursor..sel_end];
            let after = &self.input.value[sel_end..];
            let mut s = if sel_text.is_empty() {
                format!("{before}|{after}")
            } else {
                format!("{before}|[{sel_text}]{after}")
            };
            if let Some(ghost) = self.completer.hint_suffix() {
                s.push('(');
                s.push_str(ghost);
                s.push(')');
            }
            s
        }

        /// Assert the current state matches the expected snapshot.
        /// Panics with a clear diff on mismatch.
        fn assert_snapshot(&self, expected: &str) {
            let actual = self.snapshot();
            assert_eq!(
                actual, expected,
                "\nsnapshot mismatch:\n  actual:   {actual:?}\n  expected: {expected:?}"
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
    // Test 2: Shift-Tab at end of single-token query jumps into value portion
    // and selects the existing value.
    //
    // cursor=13 is in value portion (Word context). Shift-Tab calls
    // jump_token_boundary backward. The only token is sort:updated- with
    // start=0 < 13, so it qualifies. cursor_position_for_token = 5 (after
    // colon), selection_end = span.end = 13 (selects "updated-").
    // -----------------------------------------------------------------------
    #[test]
    fn shift_tab_from_end_of_default_query() {
        let mut h = Harness::new("sort:updated-|");
        h.shift_tab();
        // Cursor jumps to right after the colon; "updated-" is selected.
        h.assert_snapshot("sort:|[updated-]");
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
    //   sort:updated-  [0,13)  key_span=[0,4)   cursor_position=5  val_span=[5,13)
    //   assignee:will  [14,27) key_span=[14,22)  cursor_position=23 val_span=[23,27)
    //   priority:high  [28,41) key_span=[28,36)  cursor_position=37 val_span=[37,41)
    //
    // Shift-Tab 1: cursor=41, no selection. Jump backward to priority:high
    //   (cursor_position=37 < 41) -> cursor=37, selection_end=41.
    //
    // Shift-Tab 2: selection active (37..41). Keep cursor=37, clear sel.
    //   Jump backward from 37: priority:high excluded (cursor_position=37
    //   NOT < 37). Next prev is assignee:will (cursor_position=23 < 37).
    //   cursor=23, selection_end=27 (selects "will").
    //
    // Shift-Tab 3: selection active (23..27). Keep cursor=23, clear sel.
    //   Jump backward from 23: assignee:will excluded (23 NOT < 23).
    //   Next prev is sort:updated- (cursor_position=5 < 23).
    //   cursor=5, selection_end=13 (selects "updated-").
    // -----------------------------------------------------------------------
    #[test]
    fn shift_tab_through_multi_token_query() {
        let mut h = Harness::new("sort:updated- assignee:will priority:high|");
        // Shift-Tab 1: from end, lands after priority: colon with "high" selected.
        h.shift_tab();
        h.assert_snapshot("sort:updated- assignee:will priority:|[high]");
        // Shift-Tab 2: selection active -- accepts (cursor stays at 37) then
        // jumps backward past priority:high to assignee:will, selects "will".
        h.shift_tab();
        h.assert_snapshot("sort:updated- assignee:|[will] priority:high");
        // Shift-Tab 3: selection active -- accepts (cursor stays at 23) then
        // jumps backward past assignee:will to sort:updated-, selects "updated-".
        h.shift_tab();
        h.assert_snapshot("sort:|[updated-] assignee:will priority:high");
    }

    // -----------------------------------------------------------------------
    // Test 8: Tab forward through a multi-token query.
    //
    // Query: "sort:updated- assignee:will"
    // Token spans:
    //   sort:updated-  [0,13)  key_span=[0,4)   cursor_position=5  val_span=[5,13)
    //   assignee:will  [14,27) key_span=[14,22)  cursor_position=23 val_span=[23,27)
    //
    // Tab 1: cursor=0 inside sort:updated- key portion. StemKey{prefix:""}.
    //   Applies candidates[0]="sort:", replaces [0,5) with "sort:", cursor=5.
    //   Text unchanged. selected advances to 1.  (No selection: StemKey branch.)
    //
    // Tab 2: cursor=5, Word context (value portion of sort:updated-).
    //   Forward jump to assignee:will -> cursor=23, selection_end=27 ("will").
    //
    // Tab 3: selection active (23..27). Accepts it -> cursor=27, cleared.
    //   No further jump.
    //
    // Tab 4: cursor=27, Word context. No token starts after 27. No-op.
    // -----------------------------------------------------------------------
    #[test]
    fn tab_forward_through_multi_token_query() {
        let mut h = Harness::new("|sort:updated- assignee:will");
        // cursor=0, StemKey{prefix:""} inside sort: key. candidates=9.
        // Tab: applies "sort:" over [0,5), cursor=5. Text unchanged.
        h.tab();
        h.assert_snapshot("sort:|updated- assignee:will");
        // cursor=5, Word context. Tab jumps to assignee:, selects "will".
        h.tab();
        h.assert_snapshot("sort:updated- assignee:|[will]");
        // selection active -- Tab accepts: cursor=27, selection cleared.
        h.tab();
        h.assert_snapshot("sort:updated- assignee:will|");
        // cursor=27, Word context. No next token. Tab is a no-op.
        h.tab();
        h.assert_snapshot("sort:updated- assignee:will|");
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
    // sort:updated- at [6,19), key_span=[6,10), val_span=[11,19).
    // cursor_position_for_token = 10+1 = 11 (right after the colon).
    // selection_end = span.end = 19 (selects "updated-").
    // -----------------------------------------------------------------------
    #[test]
    fn tab_in_word_jumps_to_next_token() {
        let mut h = Harness::new("hello| sort:updated-");
        // cursor=5, Word context ("hello"). Tab -> jump to next token.
        h.tab();
        // cursor jumps to right after "sort:" colon; "updated-" is selected.
        h.assert_snapshot("hello sort:|[updated-]");
    }

    // -----------------------------------------------------------------------
    // bd-1wn: New tests for Tab/Shift-Tab stem-value selection feature.
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Test 14: Shift-Tab into a Stem token sets selection_end (value selected).
    //
    // "priority:high" is a Stem token. Shift-Tab from the end lands after
    // the colon (cursor=9) and sets selection_end=13 (end of "high").
    // -----------------------------------------------------------------------
    #[test]
    fn shift_tab_into_stem_sets_selection() {
        let mut h = Harness::new("priority:high|");
        h.shift_tab();
        // Cursor lands after colon; "high" is selected.
        h.assert_snapshot("priority:|[high]");
    }

    // -----------------------------------------------------------------------
    // Test 15: Typing a char while selection is active replaces the value.
    //
    // Starting from "priority:|[high]" (selection active), type "u" ->
    // "high" is deleted, "u" inserted at cursor=9, cursor=10, no selection.
    // -----------------------------------------------------------------------
    #[test]
    fn typing_replaces_selection() {
        let mut h = Harness::new("priority:|[high]");
        h.key('u');
        // "high" replaced by "u", cursor after "u".
        h.assert_snapshot("priority:u|");
    }

    // -----------------------------------------------------------------------
    // Test 16: Tab with active selection accepts it (moves cursor to
    // selection_end) without jumping further.
    //
    // Starting from "priority:|[high]" (selection active), Tab accepts ->
    // cursor=13 (end of "high"), selection cleared.  No additional jump.
    // -----------------------------------------------------------------------
    #[test]
    fn tab_with_active_selection_accepts_without_jumping() {
        let mut h = Harness::new("priority:|[high]");
        h.tab();
        // selection accepted: cursor at end of "high", no further jump.
        h.assert_snapshot("priority:high|");
    }

    // -----------------------------------------------------------------------
    // Test 17: Tab from the end with no next token is a no-op.
    //
    // "priority:high" with cursor at the end (13). Context=Word (value
    // portion). Tab -> jump forward -> no token starts after 13 -> no-op.
    // -----------------------------------------------------------------------
    #[test]
    fn tab_from_end_no_next_token_is_noop() {
        let mut h = Harness::new("priority:high|");
        h.tab();
        // No next token; cursor stays at end.
        h.assert_snapshot("priority:high|");
    }

    // -----------------------------------------------------------------------
    // Test 18: PartialStem (no value yet) does NOT set a selection.
    //
    // "priority:" is a PartialStem (empty value). Shift-Tab from the end
    // lands after the colon (cursor=9) but does NOT set selection_end
    // because there is nothing to select.
    // -----------------------------------------------------------------------
    #[test]
    fn shift_tab_into_partial_stem_no_selection() {
        let mut h = Harness::new("priority:|");
        // Already at cursor_position_for_token=9. No previous token with
        // cursor_position < 9.  Shift-Tab is a no-op.
        h.shift_tab();
        h.assert_snapshot("priority:|");
    }

    // -----------------------------------------------------------------------
    // Test 19: PartialStem in a multi-token query: Shift-Tab lands after
    // colon without setting selection, then a further Shift-Tab goes to
    // the previous token.
    //
    // "sort:updated- priority:" -- sort: is a Stem (val_span non-empty),
    // priority: is a PartialStem (empty val_span).
    // cursor starts at end (23).
    // Shift-Tab 1: from cursor=23 (in PartialStem value), jump to prev ->
    //   last token with cursor_position < 23 = sort: (cursor_position=5).
    //   cursor=5, selection_end=13 (selects "updated-").
    // -----------------------------------------------------------------------
    #[test]
    fn shift_tab_partial_stem_then_previous_stem() {
        let mut h = Harness::new("sort:updated- priority:|");
        // cursor=23, Word context inside PartialStem value (no selection set
        // because PartialStem landing -- but we ARE jumping FROM here, not TO
        // it). jump_token_boundary looks for the prev token with
        // cursor_position_for_token < 23.  That is sort: (5 < 23).
        h.shift_tab();
        // Lands on sort:updated- value portion; "updated-" selected.
        h.assert_snapshot("sort:|[updated-] priority:");
    }

    // -----------------------------------------------------------------------
    // Test 20: Backspace with active selection deletes the selection.
    //
    // "priority:|[high]" -- Backspace deletes "high", cursor stays at 9.
    // -----------------------------------------------------------------------
    #[test]
    fn backspace_with_active_selection_deletes_it() {
        let mut h = Harness::new("priority:|[high]");
        h.backspace();
        // "high" is deleted; cursor stays at position 9.
        h.assert_snapshot("priority:|");
    }
}
