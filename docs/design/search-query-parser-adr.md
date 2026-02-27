# Search Query Parser, AST, and Tab-Completion Design (bd-1v9)

## Status

Proposed

## Context

The current `src/tui/search_query.rs` uses `str::split_whitespace` plus
`split_once(':')` to detect stems and values. This works for the happy path
but has several limitations:

- No span information: the parser does not know *where* in the input string
  each token lives, so cursor navigation cannot jump between token boundaries.
- No error recovery model: partial input (e.g. `sort:`) silently falls through
  to FTS words; the behaviour is accidental rather than designed.
- No completion hook: nothing exposes what token the cursor is currently inside.
- Hard to extend: adding a new stem or a new value type requires editing
  multiple match arms scattered through the function.

This document records the decisions for the next iteration of the parser and
the tab-completion subsystem.

---

## Goals (recap from bead)

1. Parser / AST / Grammar with token spans.
2. Token-aware cursor navigation (Tab jumps between token boundaries).
3. Stem auto-complete (Tab cycles through known stem names at a partial stem).
4. Stem-value auto-complete (follow-up, out-of-scope for the implementation
   bead that follows this design).

---

## Decision 1: Parser Approach -- Hand-Rolled Recursive Descent

### Options Considered

| Option               | Pros                                    | Cons                                          |
|----------------------|-----------------------------------------|-----------------------------------------------|
| pest (PEG)           | Declarative grammar file, fast          | Extra build dep, grammar file not in .rs,     |
|                      |                                         | error recovery requires custom handling       |
| nom combinators      | Composable, in-tree, good span support  | Verbose for simple grammars, steep learning   |
|                      |                                         | curve for contributors unfamiliar with nom    |
| peg crate (macro)    | Compact grammar in a proc-macro         | Older API, less maintained, no span support   |
|                      |                                         | out of the box                                |
| Hand-rolled descent  | Zero new deps, trivial span tracking,   | More lines of code; grammar must be kept in   |
|                      | full control over error recovery,       | sync with docs manually                       |
|                      | easy to add incremental / partial rules |                                               |

### Decision

Use a **hand-rolled recursive descent parser** with explicit byte-span
tracking. The query grammar is deliberately simple (tokens separated by ASCII
whitespace; each token is either `key:value` or a bare word). A PEG or nom
parser would add complexity without a proportional benefit for this grammar
size. The span tracking cost is just carrying an offset counter, which is
trivial in a hand-rolled approach.

---

## Decision 2: AST Design

The AST represents one fully-parsed query. It is designed to be
always-constructible from any input string, including partial / malformed
input (error recovery via the `Unknown` token variant).

### Span

```rust
/// Byte span [start, end) within the original input string.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}
```

### Token

```rust
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
    Word {
        span: Span,
        text: String,
    },
    /// Anything that could not be classified (e.g. empty string, stray colon).
    Unknown {
        span: Span,
        raw: String,
    },
}
```

### StemKey and StemKind

```rust
/// The key side of a stem token (used for completion context).
#[derive(Debug, Clone, PartialEq)]
pub enum StemKey {
    Sort,
    Assignee,
    Priority,
    State,
    Team,
}

/// The fully-parsed meaning of a recognised stem.
#[derive(Debug, Clone, PartialEq)]
pub enum StemKind {
    Sort { field: SortField, dir: SortDir },
    Assignee { value: String },
    Priority { value: String },
    State { value: String },
    Team { value: String },
}
```

### QueryAst

```rust
pub struct QueryAst {
    /// Original input string (owned).
    pub raw: String,
    /// Ordered list of tokens (whitespace gaps are not represented).
    pub tokens: Vec<Token>,
}
```

### Derived ParsedQuery

The existing `ParsedQuery` struct and `run_query` function are kept unchanged.
A `From<&QueryAst> for ParsedQuery` impl derives the SQL-ready form from the
AST, replacing the current `parse_query` free function.

---

## Decision 3: Parser Algorithm

The parser is a single-pass, left-to-right byte scanner over the raw `&str`.

```
fn parse_query_ast(raw: &str) -> QueryAst {
    let mut tokens = Vec::new();
    let mut pos = 0;

    loop {
        // Skip whitespace.
        while pos < raw.len() && raw.as_bytes()[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= raw.len() { break; }

        // Find end of token (next whitespace boundary).
        let start = pos;
        while pos < raw.len() && !raw.as_bytes()[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let slice = &raw[start..pos];

        let token = classify_token(raw, start, pos, slice);
        tokens.push(token);
    }

    QueryAst { raw: raw.to_string(), tokens }
}
```

`classify_token` checks whether the slice contains `:`:
- If yes, split at the first `:`. Check if key is a known `StemKey`.
  - If the key is known and the value is a valid value for that stem:
    produce `Token::Stem`.
  - If the key is known but the value is empty or not yet valid:
    produce `Token::PartialStem { known_key: Some(...) }`.
  - If the key is unrecognised:
    produce `Token::PartialStem { known_key: None }` (falls through to FTS in
    the derived `ParsedQuery`).
- If no `:`, produce `Token::Word`.

### Invariants

- Every non-whitespace byte in `raw` is covered by exactly one token.
- All `span` fields contain valid UTF-8 char boundaries.
- The parser never panics; any input string yields a valid `QueryAst`.

---

## Decision 4: Completion State -- New `Completer` Struct

Completion state is separated from `SearchOverlay` into a dedicated struct.

```rust
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
    pub candidates_pending: bool,
}

pub enum CompletionContext {
    /// Cursor is inside the key portion of a partial stem (or at start of
    /// input with no characters typed yet).
    StemKey { prefix: String },
    /// Cursor is inside the value portion of a known stem (Phase 2).
    StemValue { key: StemKey, prefix: String },
    /// Cursor is inside a bare word: no structured completion.
    Word,
    /// Cursor is in whitespace between tokens or past the end.
    Gap,
}
```

`Completer::update(ast: &QueryAst, cursor: usize)` recomputes `active_token`,
`context`, and `candidates`, and resets `selected` to 0.

### Stem Key Candidates (static)

```
sort:    assignee:    priority:    state:    team:
```

When the cursor is in a partial key with some characters already typed, the
candidates are filtered to those that start with the typed prefix
(case-insensitive).

### Stem Value Candidates (Phase 2)

- `sort:` -> static: `updated-  updated+  created-  created+  priority-
  priority+  title-  title+  assignee-  assignee+  state-  state+  team-  team+`
- `priority:` -> static: `urgent  high  normal  low  none`
- `state:`, `assignee:`, `team:` -> async DB query, results cached.

---

## Decision 5: Tab Behaviour

Tab has two roles depending on context:

```
+-----------------------------+-----------------------------------------+
| Context                     | Tab action                              |
+-----------------------------+-----------------------------------------+
| Cursor inside partial key   | Cycle through matching stem key         |
|                             | completions. Each press advances        |
|                             | selected by 1 (wraps). The current      |
|                             | candidate is inserted inline,           |
|                             | replacing the partial key text.         |
+-----------------------------+-----------------------------------------+
| Cursor inside stem value    | (Phase 2) cycle value candidates.       |
+-----------------------------+-----------------------------------------+
| Cursor inside a bare word   | Jump to start of next token boundary.   |
+-----------------------------+-----------------------------------------+
| Cursor in gap or at end     | Jump to start of next token boundary.   |
+-----------------------------+-----------------------------------------+
| No next token               | Wrap to start of first token.           |
+-----------------------------+-----------------------------------------+
```

Shift-Tab reverses the direction for both cycling and boundary jumping.

### Inline Insertion

When Tab completes a stem key, the text replacement is:
1. Remove characters from `key_span.start` to the cursor (the partial key).
2. Insert the full candidate string (e.g. `priority:`).
3. Move cursor to just after the inserted colon.

The rest of the query string beyond the cursor is left untouched.

### Completion UX: Inline Suffix Hint

Two sub-options were considered:

**Inline suffix hint** (ghost text): render the untyped suffix of the top
candidate in a dimmed style after the cursor. Pressing Tab accepts or cycles.

**Popup list**: a small floating box below the search bar lists all candidates.

Decision: use **inline suffix hint** for the initial implementation because:
- The ratatui rendering in `ui.rs` already produces custom span sequences for
  the cursor (see `append_text_input_spans`). Adding a dim suffix span is a
  one-line change.
- No additional overlay layout widget is needed.
- The candidate list for stem keys is short (5 items); cycling is fast.
- A popup can be added later without changing the `Completer` API.

The inline hint is produced by `Completer::hint_suffix() -> Option<&str>`,
which returns the untyped suffix of `candidates[selected]` relative to the
already-typed prefix.

---

## Decision 6: Where Completion State Lives

`Completer` and `QueryAst` are fields on `SearchOverlay`:

```rust
pub struct SearchOverlay {
    pub query: TextInput,
    pub results: Vec<Issue>,
    pub table_state: TableState,
    pub last_changed: Option<Instant>,
    pub fts_unavailable: bool,
    pub ast: QueryAst,         // replaces raw string re-parsing in run_search
    pub completer: Completer,  // new
}
```

`SearchOverlay::on_key` handles Tab / Shift-Tab by calling
`Completer::apply_tab(&mut self, input: &mut TextInput, forward: bool)`.

`SearchOverlay::run_search` is called after every keystroke (already
debounced). It re-parses the AST, calls `Completer::update`, and then derives
`ParsedQuery` from the AST for the SQL query.

---

## Data Flow

```
User keystroke
      |
      v
TextInput::handle_key()
      |
      v
  [text or cursor changed]
      |
      v
parse_query_ast(raw) --> QueryAst stored in overlay.ast
      |
      v
ParsedQuery::from(&ast)
      |
      v
run_query() --> overlay.results updated
      |
      v
Completer::update(&ast, cursor) --> overlay.completer updated
      |
      v
  [Tab pressed]
      |
      v
Completer::apply_tab(&mut input, forward)
      |         \
      |    [completion context]
      |         |
      |    inline text replacement + cursor move
      |
      v
  loop back to parse step
```

---

## Rejected Alternatives

### Retrofitting spans onto the existing split-based parser

`str::split_whitespace` does not expose byte offsets. `str::match_indices`
could be used but produces messier code than a dedicated parser with an explicit
`pos` counter.

### Using `nom`

nom is well-suited for binary protocols and more complex grammars. For a
grammar this small, the combinator overhead (type gymnastics, `IResult`,
lifetime parameters) is not justified.

### Storing completion state in `App`

`App` already has many responsibilities. `SearchOverlay` is the natural owner
of all search-related state, including completion.

---

## Open Questions

1. **Async DB completion for state/assignee/team values**: The `Completer` API
   includes a `candidates_pending` flag placeholder, but the mechanism for
   triggering and receiving async results is TBD. The existing `AppEvent`
   channel pattern (used for sync progress) is the most likely approach.

2. **Multi-value stems**: Should `priority:urgent,high` be supported? The
   current grammar only allows a single value per stem. This would require a
   grammar extension and is deferred.

3. **Quoted values**: Stems like `state:"in progress"` would allow multi-word
   state names. Deferred.

4. **Ctrl+Right / Ctrl+Left conflict**: These keys are currently used for
   word-level navigation inside `TextInput`. Token boundaries may differ from
   word boundaries (e.g. inside `sort:updated-`, the colon is not a word
   boundary but is a token-internal boundary). Resolution: keep
   Ctrl+Right/Left as word-level navigation; use Tab/Shift-Tab exclusively for
   token-level jumping and stem completion.

---

## Follow-up Beads

- Implementation: replace `parse_query` with `parse_query_ast`; add `Span`,
  `Token`, `QueryAst`, `Completer` types.
- Implementation: wire Tab/Shift-Tab in `SearchOverlay::on_key`.
- Implementation: render inline suffix hint in `append_text_input_spans`.
- Follow-up design: async stem-value completion (state, assignee, team).
