//! Tab-completion for the search query bar. The query parser and SQL
//! builder live in `lt_runtime::search_query`; this module drives a
//! [`TextInput`] against that parser's tokens.

use lt_runtime::search_query::{QueryAst, StemKey, Token, parse_query_ast};

use crate::text_input::TextInput;

// ---------------------------------------------------------------------------
// Completer
// ---------------------------------------------------------------------------

/// The completion context derived from the cursor position in the query.
#[derive(Debug, Clone, PartialEq)]
pub enum CompletionContext {
    /// Cursor is inside the key portion of a partial stem (or at an empty
    /// input with no characters typed yet).
    StemKey { prefix: String },
    /// Cursor is inside the value portion of a known stem; not yet completed.
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
        // The token the cursor is inside (inclusive: start <= cursor <= end).
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
                if cursor <= key_span.end {
                    let prefix = ast.raw[key_span.start..cursor].to_string();
                    self.candidates = stem_key_candidates(&prefix);
                    self.context = CompletionContext::StemKey { prefix };
                } else if let Some(key) = known_key {
                    // Value portion: not yet completed.
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
                // Whitespace, past the end, or an empty input: offer all
                // stem-key candidates so Tab can cycle through them.
                self.candidates = stem_key_candidates("");
                self.context = CompletionContext::StemKey {
                    prefix: String::new(),
                };
            }
        }
    }

    /// The untyped suffix of `candidates[selected]`, for ghost-text
    /// rendering; `None` if there are no candidates or none match the
    /// prefix.
    pub fn hint_suffix(&self) -> Option<&str> {
        if self.candidates.is_empty() {
            return None;
        }
        let candidate = self.candidates.get(self.selected)?;
        let prefix = match &self.context {
            CompletionContext::StemKey { prefix } => prefix.as_str(),
            _ => return None,
        };
        // Case-insensitive match; return the suffix in the candidate's own casing.
        if candidate.to_lowercase().starts_with(&prefix.to_lowercase()) {
            Some(&candidate[prefix.len()..])
        } else {
            None
        }
    }

    /// Apply one Tab press (`forward = false` for Shift-Tab): accept any
    /// active selection first, then cycle/insert a `StemKey` candidate or
    /// jump to the next/previous token boundary.
    pub fn apply_tab(&mut self, input: &mut TextInput, ast: &QueryAst, forward: bool) {
        // Accept any active selection, then jump. Forward advances past it
        // first; backward doesn't need to -- the current token's cursor
        // position already excludes it from the backward search.
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

                // Insert first, then advance the index so the next Tab differs.
                let candidate = self.candidates[self.selected].clone();
                let n = self.candidates.len();
                if forward {
                    self.selected = (self.selected + 1) % n;
                } else {
                    self.selected = (self.selected + n - 1) % n;
                }

                // Replace through the colon (not just the key) since the
                // candidate string already includes it.
                let (replace_start, replace_end) = match &self.active_token {
                    Some(Token::PartialStem { key_span, .. } | Token::Stem { key_span, .. }) => {
                        (key_span.start, (key_span.end + 1).min(input.value.len()))
                    }
                    _ => {
                        // Fallback: find start by subtracting prefix length.
                        (input.cursor.saturating_sub(prefix.len()), input.cursor)
                    }
                };

                let mut new_value = input.value[..replace_start].to_string();
                new_value.push_str(&candidate);
                new_value.push_str(&input.value[replace_end..]);
                input.value = new_value;

                // The colon is always the candidate's last char, so cursor
                // lands right after it.
                input.cursor = replace_start + candidate.len();

                // Keep `context`'s prefix in sync so `hint_suffix` stays
                // correct until the next `update()`.
                self.context = CompletionContext::StemKey {
                    prefix: candidate[..candidate.len().saturating_sub(1)].to_string(),
                };
            }
            _ => {
                Self::jump_token_boundary(input, ast, forward);
            }
        }
    }

    /// Replace the prefix with the selected candidate and position the
    /// cursor after it. Returns `true` if a completion was applied.
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

    /// Jump the cursor to the next/previous token boundary, landing after a
    /// stem's colon. Sets `input.selection_end` to select an existing
    /// (non-empty) value; `PartialStem`s have nothing to select.
    fn jump_token_boundary(input: &mut TextInput, ast: &QueryAst, forward: bool) {
        if ast.tokens.is_empty() {
            return;
        }

        let cursor = input.cursor;

        if forward {
            let next = ast.tokens.iter().find(|t| span_bounds(t).0 > cursor);
            if let Some(t) = next {
                input.cursor = cursor_position_for_token(t);
                input.selection_end = selection_end_for_token(t);
            }
        } else {
            // Compare against `cursor_position_for_token`, not the raw
            // start, so we don't get stuck re-landing on the current token.
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

/// `Some(span.end)` for a `Stem` with a non-empty value (so it's selected
/// and typing replaces it); `None` otherwise.
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
    use lt_runtime::query::{SortDirection, SortField};
    use lt_runtime::search_query::{
        StemKind, Token, args_to_ast, parse_query_ast, render_filter_context,
    };
    use lt_types::issues::{AssigneeFilter, IssueFilter};

    use super::*;
    use crate::text_input::TextInput;

    // -- Completer tests ------------------------------------------------------

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
        let ast = parse_query_ast("s");
        let mut c = Completer::new();
        c.update(&ast, 1);
        assert_eq!(c.context, CompletionContext::Word);
        assert!(c.candidates.is_empty());
    }

    #[test]
    fn completer_update_partial_stem_key_cursor_in_key() {
        let ast = parse_query_ast("so:");
        let mut c = Completer::new();
        c.update(&ast, 2);
        match &c.context {
            CompletionContext::StemKey { prefix } => {
                assert_eq!(prefix, "so");
            }
            other => panic!("expected StemKey, got {other:?}"),
        }
        assert_eq!(c.candidates, vec!["sort:"]);
    }

    #[test]
    fn completer_update_partial_stem_key_empty_prefix() {
        let ast = parse_query_ast("a:");
        let mut c = Completer::new();
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
        c.update(&ast, 3);
        assert_eq!(c.context, CompletionContext::Word);
        assert!(c.candidates.is_empty());
    }

    #[test]
    fn completer_update_gap_past_end() {
        let ast = parse_query_ast("foo");
        let mut c = Completer::new();
        c.update(&ast, 5);
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
        c.update(&ast, 2);
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
        let ast = parse_query_ast("sort:");
        let mut c = Completer::new();
        c.update(&ast, 4);
        match &c.context {
            CompletionContext::StemKey { prefix } => {
                assert_eq!(prefix, "sort");
            }
            other => panic!("expected StemKey, got {other:?}"),
        }
        assert_eq!(c.candidates, vec!["sort:"]);
        assert_eq!(c.hint_suffix(), Some(":"));
    }

    #[test]
    fn hint_suffix_case_insensitive_prefix() {
        let ast = parse_query_ast("SO:");
        let mut c = Completer::new();
        c.update(&ast, 2);
        assert_eq!(c.candidates, vec!["sort:"]);
        assert_eq!(c.hint_suffix(), Some("rt:"));
    }

    #[test]
    fn completer_update_resets_selected_to_zero() {
        let ast = parse_query_ast("s:");
        let mut c = Completer::new();
        c.update(&ast, 1);
        c.selected = 3;
        c.update(&ast, 1);
        assert_eq!(c.selected, 0);
    }

    // -- args_to_ast tests ------------------------------------------------------

    #[test]
    fn args_to_ast_default_produces_sort_updated_desc() {
        let ast = args_to_ast(
            &IssueFilter::default(),
            &SortField::Updated,
            SortDirection::Descending,
        );
        assert_eq!(ast.raw, "sort:updated-");
        assert_eq!(ast.tokens.len(), 1);
        match &ast.tokens[0] {
            Token::Stem {
                kind: StemKind::Sort { field, dir },
                ..
            } => {
                assert!(matches!(field, SortField::Updated));
                assert_eq!(*dir, SortDirection::Descending);
            }
            other => panic!("expected Stem(Sort), got {other:?}"),
        }
    }

    #[test]
    fn args_to_ast_team_and_assignee() {
        let filter = IssueFilter {
            team: Some("eng".to_string()),
            assignee: Some(AssigneeFilter::Contains("me".to_string())),
            ..Default::default()
        };
        let ast = args_to_ast(&filter, &SortField::Updated, SortDirection::Descending);
        assert!(ast.raw.contains("team:eng"));
        assert!(ast.raw.contains("assignee:me"));
        assert!(ast.raw.contains("sort:"));
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
        let ast = args_to_ast(
            &IssueFilter::default(),
            &SortField::Priority,
            SortDirection::Ascending,
        );
        assert!(ast.raw.ends_with("sort:priority+"));
        match &ast.tokens[0] {
            Token::Stem {
                kind: StemKind::Sort { field, dir },
                ..
            } => {
                assert!(matches!(field, SortField::Priority));
                assert_eq!(*dir, SortDirection::Ascending);
            }
            other => panic!("expected Stem(Sort), got {other:?}"),
        }
    }

    // -- render_filter_context tests ---------------------------------------------

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
        // "sort:" is a PartialStem (empty value); skipped.
        let ast = parse_query_ast("sort:");
        let s = render_filter_context(&ast);
        assert_eq!(s, "");
    }

    #[test]
    fn render_filter_context_round_trip() {
        let filter = IssueFilter {
            team: Some("eng".to_string()),
            assignee: Some(AssigneeFilter::Contains("me".to_string())),
            ..Default::default()
        };
        let ast = args_to_ast(&filter, &SortField::Updated, SortDirection::Descending);
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

    // -- Snapshot-based completion test harness ----------------------------------
    //
    // '|' marks the cursor; '[text]' immediately after it marks a selection
    // (cursor..selection_end); a trailing '(text)' is ghost text from
    // `hint_suffix()`.

    use crossterm::event::{KeyCode, KeyModifiers};

    struct Harness {
        input: TextInput,
        completer: Completer,
    }

    impl Harness {
        /// Parse a snapshot string into text/cursor/selection, build the
        /// AST, and initialize the completer.
        fn new(snapshot: &str) -> Self {
            let bare = if snapshot.ends_with(')') {
                if let Some(paren) = snapshot.rfind('(') {
                    &snapshot[..paren]
                } else {
                    snapshot
                }
            } else {
                snapshot
            };

            let pipe = bare
                .find('|')
                .unwrap_or_else(|| panic!("snapshot missing '|': {snapshot:?}"));
            let before = &bare[..pipe];
            let rest = &bare[pipe + 1..];

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

        fn assert_snapshot(&self, expected: &str) {
            let actual = self.snapshot();
            assert_eq!(
                actual, expected,
                "\nsnapshot mismatch:\n  actual:   {actual:?}\n  expected: {expected:?}"
            );
        }

        fn tab(&mut self) {
            let ast = parse_query_ast(&self.input.value);
            self.completer.apply_tab(&mut self.input, &ast, true);
            let ast = parse_query_ast(&self.input.value);
            self.completer.update(&ast, self.input.cursor);
        }

        fn shift_tab(&mut self) {
            let ast = parse_query_ast(&self.input.value);
            self.completer.apply_tab(&mut self.input, &ast, false);
            let ast = parse_query_ast(&self.input.value);
            self.completer.update(&ast, self.input.cursor);
        }

        fn key(&mut self, c: char) {
            self.input.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
            let ast = parse_query_ast(&self.input.value);
            self.completer.update(&ast, self.input.cursor);
        }

        fn backspace(&mut self) {
            self.input
                .handle_key(KeyCode::Backspace, KeyModifiers::NONE);
            let ast = parse_query_ast(&self.input.value);
            self.completer.update(&ast, self.input.cursor);
        }

        fn ctrl_y(&mut self) {
            let ast = parse_query_ast(&self.input.value);
            if self.completer.accept_completion(&mut self.input, &ast) {
                let ast = parse_query_ast(&self.input.value);
                self.completer.update(&ast, self.input.cursor);
            }
        }

        /// Does not re-parse the AST; only changes the selected index.
        fn ctrl_n(&mut self) {
            self.completer.cycle_next();
        }

        /// Does not re-parse the AST; only changes the selected index.
        fn ctrl_p(&mut self) {
            self.completer.cycle_prev();
        }
    }

    #[test]
    fn tab_from_end_of_default_query() {
        let mut h = Harness::new("sort:updated-|");
        h.tab();
        h.assert_snapshot("sort:updated-|");
    }

    #[test]
    fn shift_tab_from_end_of_default_query() {
        let mut h = Harness::new("sort:updated-|");
        h.shift_tab();
        h.assert_snapshot("sort:|[updated-]");
    }

    #[test]
    fn space_then_tab_inserts_completion() {
        let mut h = Harness::new("sort:updated-|");
        h.key(' ');
        h.assert_snapshot("sort:updated- |(sort:)");
        h.tab();
        h.assert_snapshot("sort:updated- sort:|");
    }

    #[test]
    fn tab_no_wrap_at_end() {
        let mut h = Harness::new("sort:updated- sort:|");
        h.tab();
        h.assert_snapshot("sort:updated- sort:|");
    }

    #[test]
    fn shift_tab_from_start_of_stemkey_applies_candidate() {
        let mut h = Harness::new("|sort:updated-");
        h.shift_tab();
        h.assert_snapshot("sort:|updated-");
    }

    #[test]
    fn tab_partial_key_completes_without_double_colon() {
        let mut h = Harness::new("so|:");
        h.tab();
        h.assert_snapshot("sort:|");
    }

    #[test]
    fn shift_tab_through_multi_token_query() {
        let mut h = Harness::new("sort:updated- assignee:will priority:high|");
        h.shift_tab();
        h.assert_snapshot("sort:updated- assignee:will priority:|[high]");
        h.shift_tab();
        h.assert_snapshot("sort:updated- assignee:|[will] priority:high");
        h.shift_tab();
        h.assert_snapshot("sort:|[updated-] assignee:will priority:high");
    }

    #[test]
    fn tab_forward_through_multi_token_query() {
        let mut h = Harness::new("|sort:updated- assignee:will");
        h.tab();
        h.assert_snapshot("sort:|updated- assignee:will");
        h.tab();
        h.assert_snapshot("sort:updated- assignee:|[will]");
        h.tab();
        h.assert_snapshot("sort:updated- assignee:will|");
        h.tab();
        h.assert_snapshot("sort:updated- assignee:will|");
    }

    #[test]
    fn tab_cycles_candidates_in_gap() {
        let mut h = Harness::new("sort:updated- |");
        h.assert_snapshot("sort:updated- |(sort:)");
        h.tab();
        h.assert_snapshot("sort:updated- sort:|");
    }

    #[test]
    fn backspace_and_retype() {
        let mut h = Harness::new("sort:|updated-");
        h.key('x');
        h.assert_snapshot("sort:x|updated-");
        h.backspace();
        h.assert_snapshot("sort:|updated-");
    }

    #[test]
    fn ctrl_y_accepts_completion() {
        let mut h = Harness::new("so|:");
        h.ctrl_y();
        h.assert_snapshot("sort:|");
    }

    #[test]
    fn ctrl_n_ctrl_p_cycle() {
        let mut h = Harness::new("|");
        h.assert_snapshot("|(sort:)");
        h.ctrl_n();
        h.assert_snapshot("|(assignee:)");
        h.ctrl_n();
        h.assert_snapshot("|(priority:)");
        h.ctrl_p();
        h.assert_snapshot("|(assignee:)");
    }

    #[test]
    fn tab_in_word_jumps_to_next_token() {
        let mut h = Harness::new("hello| sort:updated-");
        h.tab();
        h.assert_snapshot("hello sort:|[updated-]");
    }

    #[test]
    fn shift_tab_into_stem_sets_selection() {
        let mut h = Harness::new("priority:high|");
        h.shift_tab();
        h.assert_snapshot("priority:|[high]");
    }

    #[test]
    fn typing_replaces_selection() {
        let mut h = Harness::new("priority:|[high]");
        h.key('u');
        h.assert_snapshot("priority:u|");
    }

    #[test]
    fn tab_with_active_selection_accepts_without_jumping() {
        let mut h = Harness::new("priority:|[high]");
        h.tab();
        h.assert_snapshot("priority:high|");
    }

    #[test]
    fn tab_from_end_no_next_token_is_noop() {
        let mut h = Harness::new("priority:high|");
        h.tab();
        h.assert_snapshot("priority:high|");
    }

    #[test]
    fn shift_tab_into_partial_stem_no_selection() {
        let mut h = Harness::new("priority:|");
        h.shift_tab();
        h.assert_snapshot("priority:|");
    }

    #[test]
    fn shift_tab_partial_stem_then_previous_stem() {
        let mut h = Harness::new("sort:updated- priority:|");
        h.shift_tab();
        h.assert_snapshot("sort:|[updated-] priority:");
    }

    #[test]
    fn backspace_with_active_selection_deletes_it() {
        let mut h = Harness::new("priority:|[high]");
        h.backspace();
        h.assert_snapshot("priority:|");
    }
}
