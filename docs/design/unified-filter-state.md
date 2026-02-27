# Unified Filter State: QueryAst as Single Source of Truth

## Context

The TUI header shows stale/default filters after confirming a search because
filter state is fragmented across `IssueArgs`, `SearchOverlay.query`,
`ParsedQuery`, and `last_search_query`. When Enter confirms a search, only
the sort field transfers back to `app.args` -- other stems are silently
dropped.

The fix: store a `QueryAst` on App as the single source of truth. The header,
search bar, sort shortcuts, and double-esc all read/write from this AST.
The raw string shown to the user is just a rendered view of it.

## Why QueryAst, not a raw string

- `QueryAst` is already the structured representation: typed `Token` variants
  with `StemKind` enums (`Sort`, `Assignee`, `Team`, etc.)
- `ParsedQuery` (used for SQL execution) is derived from it via
  `From<&QueryAst>` -- no re-parsing needed
- The header can be rendered by iterating tokens -- no string parsing
- The AST always carries its `.raw` string, so populating the search bar
  on `/` is just `active_filter.raw.clone()`
- Consistency is guaranteed: every `QueryAst` is produced by
  `parse_query_ast()`, so `.raw` and `.tokens` are always in sync

## Scope (3 files, ~70 lines added, ~50 removed)

| File | Changes |
|------|---------|
| `src/tui/search_query.rs` | Add `args_to_ast()`, `render_filter_context()` |
| `src/tui/mod.rs` | Add `active_filter`/`initial_filter` to App; update Enter, `/`, double-esc, cycle_sort, toggle_desc; remove `last_search_query` |
| `src/tui/ui.rs` | Replace `filter_context(&app.args)` call; delete old `filter_context()` |

---

## Implementation

### Step 1: Add `args_to_ast()` to `search_query.rs`

Converts CLI `IssueArgs` into a `QueryAst` at startup. Goes through the
string path so the AST is always structurally valid (produced by the Chumsky
parser).

```rust
pub fn args_to_ast(args: &IssueArgs) -> QueryAst {
    let mut parts: Vec<String> = Vec::new();
    if let Some(ref t) = args.team { parts.push(format!("team:{}", t)); }
    if let Some(ref a) = args.assignee { parts.push(format!("assignee:{}", a)); }
    if let Some(ref s) = args.state { parts.push(format!("state:{}", s)); }
    if let Some(ref p) = args.priority { parts.push(format!("priority:{}", p)); }
    let dir = if args.desc { "-" } else { "+" };
    parts.push(format!("sort:{}{}", args.sort.label(), dir));
    parse_query_ast(&parts.join(" "))
}
```

### Step 2: Add `render_filter_context()` to `search_query.rs`

Replaces `filter_context(&IssueArgs)` in `ui.rs`. Iterates AST tokens
directly -- no parsing, no string splitting.

```rust
pub fn render_filter_context(ast: &QueryAst) -> String {
    let mut parts = Vec::new();
    for token in &ast.tokens {
        match token {
            Token::Stem { kind, .. } => match kind {
                StemKind::Sort { field, dir } => {
                    let d = match dir { SortDir::Desc => "-", SortDir::Asc => "+" };
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
```

### Step 3: Add `active_filter` and `initial_filter` to App

In `App` struct (`mod.rs:693`):

```rust
pub active_filter: search_query::QueryAst,   // single source of truth
pub initial_filter: search_query::QueryAst,   // for double-esc reset
```

Remove `last_search_query: Option<String>` -- subsumed by
`active_filter.raw`.

In `App::new()`: initialize both via `search_query::args_to_ast(&args)`.

Keep `args: IssueArgs` and `initial_args: IssueArgs` -- still needed for
`do_fetch()` (sort, limit, offset) and operational params. Sort/desc are
kept in sync via `sync_args_from_filter()`.

### Step 4: Add two helpers on App impl

**`sync_args_from_filter()`** -- keeps `app.args.sort`/`desc` in sync with
the AST (needed for table column sort marker and `do_fetch`):

```rust
fn sync_args_from_filter(&mut self) {
    let parsed = search_query::ParsedQuery::from(&self.active_filter);
    if let Some((field, dir)) = parsed.sort {
        self.args.sort = field;
        self.args.desc = dir == search_query::SortDir::Desc;
    }
}
```

**`replace_sort_in_filter()`** -- produces a new QueryAst with the sort
token replaced (for `cycle_sort`/`toggle_desc`). Reconstructs via string
to keep AST consistent:

```rust
fn replace_sort_in_filter(&self) -> search_query::QueryAst {
    let dir = if self.args.desc { "-" } else { "+" };
    let new_sort = format!("sort:{}{}", self.args.sort.label(), dir);
    let mut parts: Vec<String> = self.active_filter.raw
        .split_whitespace()
        .filter(|t| !t.to_lowercase().starts_with("sort:"))
        .map(|s| s.to_string())
        .collect();
    parts.push(new_sort);
    search_query::parse_query_ast(&parts.join(" "))
}
```

### Step 5: Update Enter handler (`mod.rs:2494`)

Replace the sort-only transfer:

```rust
KeyCode::Enter => {
    if let Some(ref mut overlay) = app.search_overlay {
        let results = std::mem::take(&mut overlay.results);
        let selected = overlay.table_state.selected();
        // AST becomes the source of truth
        app.active_filter = overlay.ast.clone();
        app.sync_args_from_filter();
        app.issues = results;
        let n = app.issues.len();
        let sel = selected.unwrap_or(0).min(n.saturating_sub(1));
        app.table_state.select(if n > 0 { Some(sel) } else { None });
    }
    app.mode = Mode::List;
    app.search_overlay = None;
}
```

### Step 6: Update `/` handler (`mod.rs:2415`)

Restore from `active_filter.raw` instead of `last_search_query`:

```rust
KeyCode::Char('/') => {
    let mut overlay = SearchOverlay::new();
    if app.active_filter.raw != search_query::DEFAULT_QUERY {
        overlay.query = TextInput::from_string(app.active_filter.raw.clone());
        overlay.ast = app.active_filter.clone();
        overlay.last_changed = Some(Instant::now());
    }
    app.search_overlay = Some(overlay);
    app.mode = Mode::Search;
}
```

### Step 7: Update double-esc handler (`mod.rs:2379`)

Replace `app.last_search_query = None` with:

```rust
app.active_filter = app.initial_filter.clone();
```

### Step 8: Update `cycle_sort` and `toggle_desc` (`mod.rs:918, 925`)

After mutating `self.args.sort`/`self.args.desc`, rebuild the filter AST:

```rust
fn cycle_sort(&mut self) {
    self.args.sort = self.args.sort.next();
    self.active_filter = self.replace_sort_in_filter();
    // ... rest unchanged
}

fn toggle_desc(&mut self) {
    self.args.desc = !self.args.desc;
    self.active_filter = self.replace_sort_in_filter();
    // ... rest unchanged
}
```

### Step 9: Update header in `ui.rs`

- Line 30: change `filter_context(&app.args)` to
  `search_query::render_filter_context(&app.active_filter)`
- Delete the `filter_context()` function (lines 205-240) -- fully replaced

---

## Key types reference

| Type | File | Role |
|------|------|------|
| `QueryAst` | `search_query.rs:116` | Tokens + raw string + errors |
| `Token::Stem { kind: StemKind }` | `search_query.rs:89` | Typed filter stem |
| `StemKind` | generated `search_stems.rs` | Sort/Assignee/Team/State/... |
| `ParsedQuery` | `search_query.rs:160` | SQL-ready flat struct, derived via `From<&QueryAst>` |
| `IssueArgs` | `issues/mod.rs:48` | CLI args (clap), operational params |

## Verification

1. `cargo build` -- no new warnings
2. `cargo test` -- all 60 tests pass
3. Manual TUI testing:
   - Launch `lt tui` -- header shows `sort:updated-`
   - Launch `lt tui --team eng --assignee me` -- header shows
     `team:eng  assignee:me  sort:updated-`
   - Press `/`, type `assignee:will team:eng state:todo`, press Enter --
     header shows `team:eng  assignee:will  state:todo  sort:updated-`
   - Press `/` again -- search bar pre-populated with confirmed query
   - Press `S` to cycle sort -- header sort stem updates
   - Double-esc -- header resets to initial CLI filters

## Not in scope

- tui-modal.md changes (mode indicator, rename Search -> Filter) -- clean
  follow-up after this lands
- Date range stems in query syntax
- Unifying the two query backends (GraphQL list vs SQLite search)
