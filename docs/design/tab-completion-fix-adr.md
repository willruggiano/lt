# Tab Completion Bug Fixes and Test Harness (bd-zsj)

## Status

Proposed

## Context

The tab-completion system in the search bar (`src/tui/search_query.rs`) has
several bugs reported in bd-zsj. They all trace back to how the colon separator
in stem tokens (e.g. `sort:`, `assignee:`) is handled by two functions:
`cursor_position_for_token()` and `apply_tab()`.

### Observed Symptoms

Reproduced from the bd-zsj bug report (notation: `|` = cursor, `(text)` =
ghost text):

```
Initial:   sort:updated-|
Tab:       sort|:updated-          <-- cursor BEFORE the colon (wrong)
```

```
Initial:   sort:updated- |(sort:)
Tab:       sort:updated- sort:|    <-- correct insertion
Tab:       sort|:updated- sort:    <-- wrapped around, cursor before colon
Tab:       sort::updated- sort:    <-- double colon!
```

```
Initial:   sort:updated- assignee:will priority:high|
Shift-Tab: sort:updated- assignee:will priority|:high  <-- before colon
Shift-Tab: sort:updated- assignee:will priority:|:high <-- double colon
```

---

## Root Cause Analysis

### Span Semantics

`key_span` on Stem/PartialStem tokens is half-open `[start, end)`, covering
only the key text. For `sort:updated-`:

```
byte:       0  1  2  3  4  5  6  ...  12
char:       s  o  r  t  :  u  p  ...  -
            ^-----------^
            key_span = {0, 4}
                        ^
                        colon is at byte 4 = key_span.end
```

### Bug 1: `cursor_position_for_token()` off-by-one

```rust
Token::Stem { key_span, .. } => key_span.end,  // returns 4 = colon position
```

Cursor at byte 4 is visually *before* the colon. The intent is to land the
cursor in the value portion (after the colon). Should return `key_span.end + 1`.

### Bug 2: `apply_tab()` replacement does not include the colon

```rust
let replace_end = input.cursor;  // e.g. 4 (before the colon)
```

When cursor is at `key_span.end`, the replacement range `key_span.start..cursor`
covers only the key text, not the colon. The candidate string (e.g. `"sort:"`)
already contains a colon, so the original colon is left behind -- producing
`sort::`.

### Bug 3: Wraparound in `jump_token_boundary()`

When Tab reaches the last token, it wraps to the first token. The user
explicitly stated: "im not sure i like the wraparound behavior." Wrapping also
compounds Bug 1 by landing cursor at the wrong position on a distant token,
leaving stale completions behind.

---

## Decision 1: Fix `cursor_position_for_token()`

Return `key_span.end + 1` for Stem and PartialStem tokens, placing cursor after
the colon.

This also changes the completer context at the landed position: cursor is now in
the value portion (> key_span.end), so `update()` sets context to Word (for
Stem) or StemValue (for PartialStem) instead of StemKey. This is correct -- the
user landed on the value and can edit it; Tab will jump to the next token rather
than attempting key replacement.

## Decision 2: Fix `apply_tab()` replacement range

When `active_token` is Stem or PartialStem, set `replace_end` to
`(key_span.end + 1).min(input.value.len())` so the colon is included in the
replacement range. The candidate string already contains a colon, so the
replacement is 1:1.

When `active_token` is None (gap insertion), keep `replace_end = input.cursor`
so that insertion semantics are preserved.

## Decision 3: Remove wraparound from `jump_token_boundary()`

When there is no next token (forward) or no previous token (backward), do
nothing -- return early. The cursor stays where it is. This avoids the
confusion of landing on a distant token and leaving stale completions behind.

## Decision 4: Initialize completer in `SearchOverlay::new()`

Call `completer.update(&ast, query.cursor)` in the SearchOverlay constructor so
the completer has correct context from the first frame. Currently
`Completer::new()` sets context = Gap with no candidates, so the first Tab
press uses stale state.

## Decision 5: Snapshot-based completion test harness

Build a test harness that mirrors the bug report notation:

```
|        cursor position
(text)   ghost text (hint_suffix) at end of line
```

Harness API:

```rust
struct Harness { input: TextInput, completer: Completer }

Harness::new("sort:updated-|")      // parse initial state from snapshot
h.tab()                              // simulate Tab key
h.shift_tab()                        // simulate Shift+Tab
h.key('x')                           // simulate typing a character
h.backspace()                        // simulate Backspace
h.assert("sort:|updated-")           // assert snapshot matches
```

Internally, each action:
1. Applies the operation to `TextInput` / `Completer`
2. Re-parses the query text into a fresh AST
3. Calls `completer.update()` to keep context in sync

This simulates the real event loop where the debounce has already fired.

## Decision 6: Fix stale `completer_update_gap_between_tokens` test

The existing test asserts `CompletionContext::Gap` but the `None` branch in
`update()` sets `StemKey { prefix: "" }`. The test predates a change that made
the Gap branch offer all stem candidates. Update the test assertion to match
the current code.

---

## Files Modified

| File | Change |
|------|--------|
| `src/tui/search_query.rs` | Fix `cursor_position_for_token`, fix `apply_tab`, remove wrap, add test harness |
| `src/tui/mod.rs` | Initialize completer in `SearchOverlay::new()` |

## Verification

```bash
cargo test --lib search_query::tests
```
