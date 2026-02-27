# Search Parser v2: Build-Time Grammar Generation (ADR)

## Status

Proposed

## Context

The existing search parser (`src/tui/search_query.rs`, designed in
`search-query-parser-adr.md`) uses a hand-rolled byte scanner with five
hard-coded stem keys: `sort`, `assignee`, `priority`, `state`, `team`. It
serves the happy path well but has two limitations that motivate a second
design iteration:

1. **Limited filter coverage.** The Linear `IssueFilter` GraphQL type exposes
   ~30 filterable fields. Expanding the stem key set by hand is a maintenance
   burden that drifts out of sync with the API over time.

2. **No error feedback.** Misspelled keys (`priorty:high`) silently fall
   through to FTS. Users get no indication that a stem was not recognized.

The goals of this iteration are:

- Expand supported filter stems to a curated subset of `IssueFilter` fields.
- Keep the stem key list in sync with the Linear API schema automatically.
- Provide "did you mean?" error recovery for misspelled stem keys.
- Preserve all existing AST, span, and tab-completion behavior.

Value completion is explicitly out of scope for this iteration.

---

## Decision 1: Parser -- Chumsky

### Options Considered

| Option                | Pros                                                        | Cons                                                              |
|-----------------------|-------------------------------------------------------------|-------------------------------------------------------------------|
| Keep hand-rolled      | Zero new deps, already works                                | No error recovery; ~30 keys adds repetitive match arms            |
| pest (PEG)            | Declarative grammar file, easy to generate from build.rs    | Error messages need manual formatting; no built-in suggestions    |
| chumsky combinators   | First-class error recovery; Rich errors with suggestions;   | New dependency                                                    |
|                       | composable per-key value parsers                            |                                                                   |

### Decision

Use **chumsky 0.9.x** (current stable). The primary motivation is its `Rich`
error type, which supports edit-distance spelling suggestions and is the right
foundation for "did you mean?" feedback without significant manual formatting
work. Pest was considered but achieving the same error quality requires more
effort.

The query grammar remains simple; Chumsky does not add disproportionate
complexity for this grammar size.

---

## Decision 2: Code Generation via build.rs

### Rationale

`build.rs` reads `docs/reference/linear-schema-definition.graphql` at compile
time, validates that every key in the allowlist corresponds to a real
`IssueFilter` field, and generates the Rust source for the parser. If a field
in the allowlist is removed from the upstream schema, the build fails loudly,
preventing silent drift.

### Build Dependencies

```toml
[build-dependencies]
graphql-parser = "0.4"   # lightweight pure-Rust GraphQL schema parser
```

Code generation uses simple string templating (no `quote` / `proc-macro2`
needed given the small output size).

### Generated Artifacts

`build.rs` writes one file to `$OUT_DIR`:

```
$OUT_DIR/search_stems.rs    -- StemKey enum, StemKind enum, Chumsky parser fn
```

Included from `src/tui/search_query.rs` via:

```rust
include!(concat!(env!("OUT_DIR"), "/search_stems.rs"));
```

### Build Pipeline

```
docs/reference/linear-schema-definition.graphql
build/search_filter_fields.toml
              |
              v
          build.rs
              | 1. parse schema, extract IssueFilter fields
              | 2. validate allowlist entries against schema
              | 3. generate Rust source
              v
    $OUT_DIR/search_stems.rs
              |
              v
    src/tui/search_query.rs
    (includes generated file)
```

---

## Decision 3: Nested Filter Type Flattening

Each `IssueFilter` field has a GraphQL type that is itself a complex input
object. For example, `NullableUserFilter` has ~15 sub-fields (`name`, `email`,
`displayName`, `isMe`, `assignedIssues`, ...).

The search bar grammar does **not** expose the nested structure. Every filter
field is flattened to a single `key:value` token where the value is a bare
string. The semantic interpretation of that string -- which sub-field to match
and how -- is specified in the allowlist config, not derived from the schema.

The schema is consulted only to:

1. Confirm the field name exists in `IssueFilter`.
2. Verify the declared GraphQL type matches the expected type in the config
   (acts as a guard against API changes).

All value semantics are hand-specified per allowlist entry.

---

## Decision 4: Allowlist Config Format

The allowlist lives at `build/search_filter_fields.toml`. Each `[[field]]`
entry specifies one generated stem key:

```toml
[[field]]
key        = "assignee"               # stem key name in the search bar
gql_field  = "assignee"               # field name in IssueFilter (schema-validated)
gql_type   = "NullableUserFilter"     # expected GraphQL type (schema-validated)
value_hint = "<name>"                 # used in error messages
sql_col    = "assignee_name"          # SQLite column to match against
sql_op     = "LIKE"                   # LIKE or =
sql_lower  = true                     # whether to LOWER() both sides

[[field]]
key        = "priority"
gql_field  = "priority"
gql_type   = "NullableNumberComparator"
value_hint = "urgent|high|normal|low|none"
sql_col    = "priority_label"
sql_op     = "="
sql_lower  = false

[[field]]
key        = "state"
gql_field  = "state"
gql_type   = "WorkflowStateFilter"
value_hint = "<name>"
sql_col    = "state_name"
sql_op     = "LIKE"
sql_lower  = true

[[field]]
key        = "team"
gql_field  = "team"
gql_type   = "TeamFilter"
value_hint = "<name>"
sql_col    = "team_name"
sql_op     = "LIKE"
sql_lower  = true

[[field]]
key        = "label"
gql_field  = "labels"
gql_type   = "IssueLabelCollectionFilter"
value_hint = "<name>"
sql_col    = "label_names"
sql_op     = "LIKE"
sql_lower  = true
```

The `sort:` stem is a UI concept with no `IssueFilter` backing. It is
hard-coded in `build.rs` and is not represented in the TOML.

### Field Spec Semantics

| Field      | Required | Description                                                     |
|------------|----------|-----------------------------------------------------------------|
| key        | yes      | Stem key string as typed by the user                            |
| gql_field  | yes      | Field name in `IssueFilter`; validated against parsed schema    |
| gql_type   | yes      | Expected GraphQL type name; validated against parsed schema     |
| value_hint | yes      | Human-readable value placeholder for error messages             |
| sql_col    | yes      | SQLite column in the `issues` table                             |
| sql_op     | yes      | `LIKE` (substring) or `=` (exact after normalization)           |
| sql_lower  | yes      | If true, wraps both sides in `LOWER()`                          |

---

## Decision 5: Error Recovery and "Did You Mean?"

Chumsky's `recover_with(skip_then_retry_until(...))` combinator allows the
parser to skip an unrecognized token and continue, always producing a complete
AST.

For unknown stem keys, the parser collects candidates from the generated key
list and picks the closest match by edit distance:

```
user types:   priorty:high
parse error:  unknown filter key 'priorty' -- did you mean 'priority'?

user types:   assignee:
parse error:  expected <name> after 'assignee:'
```

Errors are stored on `QueryAst` (see Decision 6) and rendered in the TUI
below the search bar. The result list continues to update on every keystroke
because the parser always emits a best-effort AST regardless of errors.

---

## Decision 6: AST and Span Design

The `QueryAst`, `Token`, `Span`, and `Completer` types from the previous design
(bd-1v9) are preserved without structural change. Chumsky provides span
information via `.map_with_span(...)`, replacing the manual offset counter. The
`Token` variants remain:

- `Token::Stem` -- recognized key with a valid value
- `Token::PartialStem` -- key recognized, value absent or incomplete
- `Token::Word` -- bare FTS word
- `Token::Unknown` -- unrecognized, falls through to FTS

One new field is added to `QueryAst` to carry structured parse errors for
display:

```rust
pub struct QueryAst {
    pub raw: String,
    pub tokens: Vec<Token>,
    pub errors: Vec<ParseError>,   // new
}

pub struct ParseError {
    pub span: Span,
    pub message: String,   // e.g. "unknown key 'priorty', did you mean 'priority'?"
}
```

The `StemKey` and `StemKind` enums are **generated** by `build.rs` from the
TOML allowlist instead of being hand-written. Their shape is otherwise
identical to the previous design.

---

## Decision 7: SQL Translation

`ParsedQuery` and `run_query` are preserved. `From<&QueryAst> for ParsedQuery`
is regenerated from the TOML config: each field entry emits one match arm.
The `sort:` arm remains hand-written.

Generated translation for a `LIKE` field looks like:

```rust
// generated for: key=assignee, sql_col=assignee_name, sql_op=LIKE, sql_lower=true
StemKind::Assignee { value } => {
    conditions.push("LOWER(COALESCE(assignee_name,'')) LIKE ?".to_string());
    bind.push(Box::new(format!("%{}%", value)));
}
```

The `priority` stem retains the existing `normalise_priority` mapping
(user strings to DB labels) as a special case, since that mapping is semantic
and cannot be derived from the schema.

---

## Rejected Alternatives

### Fully auto-deriving everything from the schema (no allowlist)

`IssueFilter` has ~30 fields, many of which are internal (`[Internal]`
doc annotation), specialized (`slaStatus`, `sharedWith`), or have opaque
nested types with no obvious single-column SQL translation. Auto-generating
all of them would expose incomplete SQL support and confusing key names. The
allowlist is the right control point for curation.

### Keeping the hand-rolled scanner with more match arms

This works but provides no error recovery or "did you mean?", which are the
primary UX goals of this iteration. It also does not scale cleanly as the
key count grows.

### Pest with a generated grammar file

Pest error messages require significant manual formatting work to reach the
quality that `chumsky::error::Rich` provides out of the box.

### Embedding the allowlist as a Rust literal inside build.rs

Reduces file count but makes the config harder to read and diff independently
of the build script logic. A separate TOML file is preferred for clarity.

---

## Open Questions

1. **Error display location.** Where in the TUI does the parse error appear?
   Options: a status line below the search bar; an inline annotation span
   next to the offending token. TBD in implementation.

2. **`sort:` stem maintenance.** The `sort:` stem is hard-coded in `build.rs`.
   If the set of sortable fields should also be config-driven in the future, a
   second `[[sort_field]]` config section would be needed.

3. **Priority normalization.** The `priority` stem maps user strings
   (`urgent`, `high`, ...) to DB labels (`Urgent`, `High`, ...). This mapping
   is currently a hand-written function. It could be moved into the TOML config
   as a `[field.value_map]` table if other stems need similar normalization.

---

## Follow-up Beads

- Implementation: write `build.rs` with `graphql-parser` + TOML allowlist
- Implementation: replace hand-rolled scanner with generated Chumsky parser
- Implementation: add `QueryAst::errors` and render parse errors in the TUI
- Follow-up design: async stem-value completion (deferred from bd-1v9)
