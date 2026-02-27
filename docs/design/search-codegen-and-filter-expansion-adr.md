# Search Code Generation Overhaul and Filter Expansion (ADR)

## Status

Proposed

## Context

`build.rs` generates `search_stems.rs` from a TOML allowlist
(`build/search_filter_fields.toml`) validated against the Linear GraphQL
schema. The generated file is consumed via `include!` in
`src/tui/search_query.rs` and provides:

- `StemKey` enum (one variant per TOML field plus hard-coded `Sort`)
- `StemKind` enum (`Sort` with struct fields; others carry `{ value: String }`)
- `parse_query_ast_impl()` -- a Chumsky 0.9 parser with per-field match arms
- `impl From<&QueryAst> for ParsedQuery` -- AST-to-SQL-ready conversion

Two problems motivate this ADR:

1. **The code generation technique is fragile.** The ~580-line `build.rs` uses
   raw `push_str` / `format!` string concatenation to emit Rust source. Every
   `{` and `}` in the generated code must be escaped as `{{` / `}}`. The
   `gen_parser_fn` function alone is ~250 lines of interleaved string fragments
   and format arguments. The result is difficult to read, review, or modify
   with confidence.

2. **Only 5 of ~50 IssueFilter fields are exposed.** The current allowlist
   covers `assignee`, `priority`, `state`, `team`, and `label`. The Linear
   GraphQL schema offers many more fields that would be practically useful for
   search.

Additionally, the TOML fields `sql_col`, `sql_op`, and `sql_lower` are
annotated `#[allow(dead_code)]` in the build script's deserialization structs
and are **never consumed by code generation**. The actual SQL query building in
`run_query()` (search_query.rs:332-478) is entirely hand-written per field.
These TOML fields create a false impression that they drive behavior.

---

## Decision 1: Code Generation Technique -- `quote` + `prettyplease` in build.rs

### Options Considered

| Option | Readability | Schema validation | Build caching | Compile cost | Error quality | Handles complex fn |
|---|---|---|---|---|---|---|
| `push_str` (current) | Poor | Yes | Good | Minimal | OK | Yes |
| `quote` + `prettyplease` | Good | Yes | Good | Low (+3 deps) | Better | Yes |
| Proc macro (separate crate) | Good | Awkward | None | Medium | Worse | Yes |
| `macro_rules!` | Poor | No | N/A | None | N/A | No |
| `codegen` crate (builder API) | OK | Yes | Good | Low | OK | No |
| Template engine (askama/tera) | OK | Yes | Good | Low | Worse | Awkward |

### Analysis

**`quote` + `proc-macro2` + `prettyplease` in the existing build.rs** is the
clear winner. This is the idiomatic Rust approach for structured code
generation in build scripts.

`quote` (by dtolnay) is the foundation of virtually every proc macro in the
Rust ecosystem. It works perfectly fine outside of proc macros via
`proc-macro2`. The `format_ident!` macro constructs identifiers from runtime
strings. Repetition syntax (`#( #variants ),*`) replaces manual loops that
append match arms.

Example of what the transformation looks like:

**Before (push_str):**

```rust
fn gen_stem_key_enum(fields: &[FieldSpec]) -> String {
    let mut s = String::new();
    s.push_str("#[derive(Debug, Clone, PartialEq)]\n");
    s.push_str("pub enum StemKey {\n");
    s.push_str("    Sort,\n");
    for f in fields {
        s.push_str(&format!("    {},\n", to_pascal_case(&f.key)));
    }
    s.push_str("}\n");
    s
}
```

**After (quote):**

```rust
fn gen_stem_key_enum(fields: &[FieldSpec]) -> TokenStream {
    let variants: Vec<Ident> = fields
        .iter()
        .map(|f| format_ident!("{}", to_pascal_case(&f.key)))
        .collect();

    quote! {
        #[derive(Debug, Clone, PartialEq)]
        pub enum StemKey {
            Sort,
            #( #variants, )*
        }
    }
}
```

The final output pipeline pipes all `TokenStream` fragments through
`prettyplease` before writing, so `OUT_DIR/search_stems.rs` becomes
human-readable for debugging.

**Why not proc macros?** Despite Rust's reputation for proc macros, they are
the wrong tool here. Proc macros transform Rust source tokens. Our input is
TOML + GraphQL files. Specific issues:

- Proc macros must live in a separate crate (`proc-macro = true`).
- File I/O inside proc macros is problematic -- paths resolve relative to the
  compiler's working directory, and cargo does not track these files for
  `rerun-if-changed`.
- Proc macro expansion re-runs on every compilation, unlike build.rs which
  respects `rerun-if-changed`.
- Error messages for file I/O failures surface as proc macro panics with
  limited span information.

**Why not `macro_rules!`?** Declarative macros cannot read files, cannot do
string transformations (e.g. snake_case to PascalCase), and cannot use captured
fragments in match arm patterns (rust-lang/rust#64400). The complex parser
function with Chumsky integration would be extremely difficult to express.

**Why not a template engine?** No syntax checking of template content at
compile time. The complex parser function would still be painful to template
correctly. Mixing templates and string generation would be worse than using
`quote!` consistently.

### New build dependencies

```toml
[build-dependencies]
quote = "1"
proc-macro2 = "1"
syn = { version = "2", default-features = false, features = ["full", "parsing"] }
prettyplease = "0.2"
# existing: graphql-parser, toml, serde
```

All by dtolnay, all widely used, all compile quickly and cached after first
build.

### Migration path

Incremental, one generator function at a time:

1. `gen_stem_key_enum` (simplest, ~15 lines)
2. `gen_stem_kind_enum` (similar structure)
3. `gen_from_ast` (match arms with interpolation)
4. `gen_parser_fn` (most complex -- Chumsky boilerplate with interpolated arms)

Verify generated output is equivalent at each step.

### Decision

**Use `quote` + `prettyplease` in build.rs.** Keep the build.rs + `include!`
architecture; replace only the string concatenation technique.

---

## Decision 2: Clean Up Dead TOML Metadata

### Problem

The TOML allowlist contains `sql_col`, `sql_op`, `sql_lower`, and
`value_hint` fields that are deserialized but never used by code generation.
They are annotated `#[allow(dead_code)]` in build.rs. Meanwhile, the actual
SQL construction in `run_query()` is hand-written with per-field logic (e.g.
`team` matches against both `team_name` and `team_key` with an OR).

### Options

1. **Remove the dead fields.** Keep the TOML as pure "what stems exist" config.
   SQL logic stays hand-written.
2. **Use the fields to generate `run_query()` too.** Make the TOML fully
   declarative and generate the SQL WHERE clauses.
3. **Keep as-is.** Document them as "planned for future use."

### Analysis

Option 2 is appealing in theory but breaks down for field-specific SQL logic.
The `team` filter ORs across two columns. The `priority` filter requires
`normalise_priority()`. The `assignee` filter has `resolve_me()` special
handling. Encoding all of this in TOML metadata would require a mini-DSL that
is harder to maintain than the hand-written Rust.

Option 1 is the cleanest. If the fields are not driving behavior, they create
a false contract. Developers reading the TOML expect `sql_col = "team_name"`
to mean something, but changing it has no effect.

### Decision

**Remove `sql_col`, `sql_op`, `sql_lower`, and `value_hint` from the TOML.**
The allowlist should declare stem keys, their GraphQL mapping (for schema
validation), and nothing else. Document the SQL mapping in comments in
`run_query()` where it actually lives.

### Future: Declarative SQL Generation from TOML

The dead metadata fields were presumably intended to drive SQL generation, but
the schema is too simplistic. Here is a side-by-side of what the TOML declares
versus what `run_query()` actually does:

```
TOML declares                  run_query() actually does
-----------------------------  -------------------------------------------
assignee:
  sql_col   = assignee_name   LOWER(COALESCE(assignee_name,'')) LIKE ?
  sql_op    = LIKE             + special "me" branch with = 'me'
  sql_lower = true             bind: "%{value}%"

priority:
  sql_col   = priority_label   priority_label = ?
  sql_op    = =                + normalise_priority() value transform
  sql_lower = false            bind: normalised label (skip if None)

state:
  sql_col   = state_name       LOWER(state_name) LIKE ?
  sql_op    = LIKE             bind: "%{value}%"
  sql_lower = true             (only clean match in the set)

team:
  sql_col   = team_name        (LOWER(team_name) LIKE ?
  sql_op    = LIKE               OR LOWER(COALESCE(team_key,'')) LIKE ?)
  sql_lower = true             bind: "%{value}%" (twice -- two columns!)

label:
  sql_col   = label_names      LOWER(COALESCE(labels,'')) LIKE ?
  sql_op    = LIKE             bind: "%{value}%"
  sql_lower = true             (TOML says "label_names", actual col is "labels")
```

Three of five fields need capabilities the metadata cannot express:
multi-column OR (team), value transforms (priority), special branches
(assignee), COALESCE wrapping (assignee, team, label). Only `state` is a clean
match.

A richer TOML schema could support generated SQL. Sketch:

```toml
[[field]]
key       = "team"
gql_field = "team"
gql_type  = "TeamFilter"

# Multiple sql_match entries are ORed together.
[[field.sql_match]]
col      = "team_name"
op       = "LIKE"
lower    = true
coalesce = false
bind     = "%{value}%"

[[field.sql_match]]
col      = "team_key"
op       = "LIKE"
lower    = true
coalesce = true
bind     = "%{value}%"
```

```toml
[[field]]
key       = "priority"
gql_field = "priority"
gql_type  = "NullableNumberComparator"

[field.sql_match]
col                          = "priority_label"
op                           = "="
lower                        = false
coalesce                     = false
bind                         = "{value}"
transform                    = "normalise_priority"
skip_if_transform_returns_none = true
```

This becomes worthwhile at ~15+ filter stems where the hand-written approach
gets repetitive. The `quote` rewrite (Decision 1) is a prerequisite since
generating SQL-building Rust via `push_str` would be even worse than what
exists today. Not in scope for this sprint -- captured here for future
reference.

---

## Decision 3: Filter Expansion -- Additional Search Stems

### Current Allowlist

| Key | gql_field | gql_type |
|---|---|---|
| `assignee` | `assignee` | `NullableUserFilter` |
| `priority` | `priority` | `NullableNumberComparator` |
| `state` | `state` | `WorkflowStateFilter` |
| `team` | `team` | `TeamFilter` |
| `label` | `labels` | `IssueLabelCollectionFilter` |

Plus hard-coded `sort`.

### Candidate Fields from IssueFilter

The `IssueFilter` input type has ~50 fields. These are organized by practical
value for a developer searching issues locally.

#### Tier 1 -- High value, straightforward to add

These are simple entity filters with the same `key:value` pattern as existing
stems. They map to columns likely already present in the local SQLite database
(or easily added during sync).

| Key | gql_field | gql_type | Rationale |
|---|---|---|---|
| `project` | `project` | `NullableProjectFilter` | Very common workflow filter |
| `cycle` | `cycle` | `NullableCycleFilter` | Filter by sprint/cycle |
| `creator` | `creator` | `NullableUserFilter` | "Who filed this?" |

#### Tier 2 -- Useful for power users

These are relationally useful but may require schema changes to the local
SQLite tables or more complex SQL.

| Key | gql_field | gql_type | Rationale |
|---|---|---|---|
| `milestone` | `projectMilestone` | `NullableProjectMilestoneFilter` | Filter by project milestone |
| `parent` | `parent` | `NullableIssueFilter` | Find sub-issues of a parent |
| `blocked` | `hasBlockedByRelations` | `RelationExistsComparator` | Find blocked issues |
| `blocking` | `hasBlockingRelations` | `RelationExistsComparator` | Find issues blocking others |
| `sla` | `slaStatus` | `SlaStatusComparator` | Find SLA-breached issues |
| `subscriber` | `subscribers` | `UserCollectionFilter` | "Am I watching this?" |

#### Tier 3 -- Date/number comparators (requires parser extension)

These fields use date or number comparators. The current parser only supports
`key:value` (string equality/LIKE). Supporting these would require extending
the grammar to handle operators like `due:>2024-01-01` or `estimate:>=3`.

| Key | gql_field | gql_type | Rationale |
|---|---|---|---|
| `due` | `dueDate` | `NullableTimelessDateComparator` | Find overdue/upcoming issues |
| `estimate` | `estimate` | `EstimateComparator` | Filter by story points |
| `created` | `createdAt` | `DateComparator` | Date range filtering |
| `updated` | `updatedAt` | `DateComparator` | Recently touched issues |
| `completed` | `completedAt` | `NullableDateComparator` | Completion date filtering |

#### Tier 3 -- Niche

| Key | gql_field | gql_type | Rationale |
|---|---|---|---|
| `archived` | `archivedAt` | `NullableDateComparator` | Find archived issues |
| `description` | `description` | `NullableStringComparator` | Text search on body |
| `number` | `number` | `NumberComparator` | Filter by issue number |
| `delegate` | `delegate` | `NullableUserFilter` | AI agent assignments |

### Dependencies

Adding Tier 1 stems requires:

- A TOML entry per new field (trivial once codegen is `quote`-based).
- A corresponding column in the local SQLite `issues` table (check sync).
- A SQL clause in `run_query()` (hand-written, per Decision 2).
- Tab completion picks up new stems automatically from the generated
  `STEM_KEY_STRINGS` array.

Adding Tier 3 date/number stems requires:

- Parser grammar extension (new `StemKind` variants carrying structured
  values instead of raw strings).
- Operator-aware SQL generation.
- This is a separate body of work and should be its own ADR.

### Decision

**Add Tier 1 fields (`project`, `cycle`, `creator`) in the same change as
the codegen overhaul.** Defer Tier 2 and Tier 3 fields until the local
database schema supports them or until operator syntax is designed.

---

## Current System Architecture Reference

For context, here is the full data flow from user input to SQL execution:

```
User types in search bar
  |
  v
TextInput::handle_key()
  |
  v  (on Tab)                    (on other keys)
apply_tab()                      normal insertion
  |                                |
  v                                v
parse_query_ast(raw) -----> QueryAst { tokens, errors, raw }
  |                                |
  |    +---------------------------+
  |    |
  v    v
Completer::update(&ast, cursor)     ParsedQuery::from(&ast)
  |                                       |
  v                                       v
Ghost text / Tab cycling             run_query(&conn, &parsed, limit)
                                          |
                                          v
                                     SQL with WHERE clauses:
                                       - FTS5 MATCH for free text
                                       - LIKE/= per stem filter
                                       - ORDER BY from sort stem
                                          |
                                          v
                                     Vec<Issue> results
```

### Token Types

```rust
enum Token {
    Stem { span, key_span, val_span, kind: StemKind },  // Fully valid
    PartialStem { span, key_span, val_span, known_key: Option<StemKey> },  // Known key, bad value
    Word { span, text: String },   // Free-text word -> FTS
    Unknown { span, raw: String }, // Fallback -> FTS
}
```

### Completion Contexts

```rust
enum CompletionContext {
    StemKey { prefix: String },                    // Completing "so" -> "sort:"
    StemValue { key: StemKey, prefix: String },    // Phase 2 stub (not implemented)
    Word,                                          // No completion
    Gap,                                           // Cursor in whitespace
}
```

### Key Files

| File | Role |
|---|---|
| `build.rs` | Validate TOML against schema, generate search_stems.rs |
| `build/search_filter_fields.toml` | Allowlist of exposed filter stems |
| `src/tui/search_query.rs` | Parsing, AST, ParsedQuery, Completer, run_query |
| `src/tui/mod.rs` | SearchOverlay, Tab key handler, debounce |
| `src/tui/ui.rs` | Ghost text rendering |
| `docs/reference/linear-schema-definition.graphql` | Schema for build-time validation |

### Phase 2 Stubs (Not Yet Implemented)

- **Stem value completion:** `StemValue` context returns empty candidates.
  `candidates_pending` flag is defined but unused.
- **Async candidate loading:** Stubbed for future DB-backed value suggestions.

---

## Summary of Decisions

1. **Replace `push_str` code generation with `quote` + `prettyplease`** in the
   existing build.rs. Keep the build.rs + `include!` architecture. Do not use
   proc macros.

2. **Remove dead TOML metadata** (`sql_col`, `sql_op`, `sql_lower`,
   `value_hint`). Keep SQL logic hand-written in `run_query()`.

3. **Add `project`, `cycle`, `creator` stems** alongside the codegen overhaul.
   Defer date/number operator syntax to a future ADR.
