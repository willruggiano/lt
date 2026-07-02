# Keybinds: Linear Parity and Keymap Machinery

## Context

The TUI should use the same (or as similar as possible) keybinds as the
linear.app web client (ENG-26). Linear's core interaction patterns — two-key
chord navigation (`g` then `i`, `o` then `w`) and single-letter contextual
actions on the focused issue — cannot be expressed by the current input
machinery, which is a per-mode `match KeyCode` with no key→action indirection,
no chord support, and a hand-maintained help table that has already drifted from
the real handlers.

This document inventories Linear's shortcuts, assesses the current machinery,
specifies its replacement, and lays out the implementation plan. It supersedes
the binding tables in [[tui-modal.md]]; the mode taxonomy described there is
unchanged.

## Goals

- Match Linear's web bindings wherever the underlying feature exists in `lt`;
  reserve Linear's keys for features that don't exist yet (e.g. ENG-41 workspace
  switching = `o w`) so they slot in without redesign.
- Replace ad-hoc dispatch with a declarative keymap: every binding is a row of
  data, resolvable to an `Action`, enumerable for the help overlay.
- Support two-key chords with visible pending state.
- Keep vim navigation vocabulary (`j`/`k`, `g g`/`G`, `ctrl+d`/`ctrl+u`) — it
  coexists with Linear's bindings.

## Non-goals

- **User-configurable keybinds.** Linear itself has no remapping. Bindings are
  hardcoded; the `Key` type is string-parsable (`FromStr`/`Display`) so a config
  layer can be layered on later without rework, but no config file, loader, or
  serde dependency ships now.
- **Ctrl+K command palette.** Linear's universal fallback is out of scope. The
  machinery makes it cheap later: a palette enumerates `(Binding, Action)` rows
  plus `Action::label()`.
- Mouse bindings, multi-select (`x`), sequences longer than two keys, vim count
  prefixes (`3j`).
- The features behind reserved bindings (inbox, triage, boards, workspaces).

## Linear web client inventory

Sourced from Linear's official documentation only. There is no single shortcuts
page — the authoritative complete list is the in-app shortcuts panel; the docs
document shortcuts per feature. Each table cites its pages.

Status column: **bound** (mapped in this design), **reserved** (key left unbound
until the feature exists), **out of scope** (browser- or mouse-specific, or a
non-goal).

### Navigation chords

Sources: [inbox](https://linear.app/docs/inbox),
[my-issues](https://linear.app/docs/my-issues),
[triage](https://linear.app/docs/triage),
[search](https://linear.app/docs/search),
[favorites](https://linear.app/docs/favorites).

| Keys  | Action                   | Status            |
| ----- | ------------------------ | ----------------- |
| `g i` | Go to Inbox              | reserved          |
| `g m` | Go to My Issues          | reserved          |
| `g t` | Go to Triage             | reserved          |
| `o i` | Open issue by ID/title   | reserved          |
| `o p` | Open project             | reserved          |
| `o v` | Open view                | reserved          |
| `o f` | Open favorite            | reserved          |
| `o d` | Open document            | reserved          |
| `o u` | Open workspace user list | reserved          |
| `o w` | Switch workspace         | reserved (ENG-41) |
| `o q` | Open customer request    | reserved          |
| `m m` | Mark as duplicate        | reserved          |
| `w o` | Work on issue menu       | reserved          |

### Global and list navigation

Sources: [select-issues](https://linear.app/docs/select-issues),
[search](https://linear.app/docs/search).

| Keys               | Action             | Status                        |
| ------------------ | ------------------ | ----------------------------- |
| `j` / `k`, arrows  | Move highlight     | bound                         |
| `enter`            | Open issue         | bound (`space` kept as alias) |
| `/`                | Search             | bound                         |
| `?`                | Open help panel    | reserved (no help panel)      |
| `ctrl+/`           | Keyboard shortcuts | bound                         |
| `V`                | Display options    | reserved                      |
| `esc`              | Context-dependent  | bound                         |
| `cmd/ctrl+k`       | Command menu       | out of scope (non-goal)       |
| `cmd/ctrl+f`       | Find in view       | reserved                      |
| `cmd/ctrl+[` / `]` | History back/fwd   | out of scope (browser)        |

The `ctrl+/` (keyboard shortcuts) and `V` (display options — Linear's home for
ordering) rows are not documented on the cited pages; they come from the app
itself.

### Issue actions (focused or selected issue)

Sources: [assigning-issues](https://linear.app/docs/assigning-issues),
[priority](https://linear.app/docs/priority),
[labels](https://linear.app/docs/labels),
[due-dates](https://linear.app/docs/due-dates),
[estimates](https://linear.app/docs/estimates),
[creating-issues](https://linear.app/docs/creating-issues),
[select-issues](https://linear.app/docs/select-issues).

| Keys             | Action               | Status                       |
| ---------------- | -------------------- | ---------------------------- |
| `s`              | Change status        | bound (already matches)      |
| `a`              | Assign               | bound (already matches)      |
| `p`              | Set priority         | bound (already matches)      |
| `c`              | Create issue         | bound (replaces `n`)         |
| `i`              | Assign to me         | reserved (cheap follow-up)   |
| `l`              | Edit labels          | reserved                     |
| `P`              | Set project          | reserved                     |
| `D`              | Set due date         | reserved                     |
| `E`              | Set estimate         | reserved                     |
| `S`              | Subscribe            | reserved                     |
| `h`              | Snooze               | reserved                     |
| `x`              | Select (multi)       | out of scope (non-goal)      |
| `v`              | Create full-screen   | out of scope (no equivalent) |
| `alt+c`          | Create from template | reserved                     |
| `cmd/ctrl+enter` | Submit form          | bound                        |

### Inbox, triage, boards

Sources: [inbox](https://linear.app/docs/inbox),
[triage](https://linear.app/docs/triage),
[board-layout](https://linear.app/docs/board-layout).

| Keys                                       | Action                 | Status               |
| ------------------------------------------ | ---------------------- | -------------------- |
| `u`, `alt+u`                               | Mark (all) read/unread | reserved (inbox)     |
| `backspace`                                | Delete notification    | reserved (inbox)     |
| `1` / `2` / `3`                            | Accept / dup / decline | reserved (triage)    |
| `cmd/ctrl+b`                               | Toggle board layout    | reserved (boards)    |
| `t`                                        | Collapse swimlane      | reserved (boards)    |
| hover `space`, `shift`+scroll, right-click | —                      | out of scope (mouse) |

Linear's rich-text editor shortcuts ([editor](https://linear.app/docs/editor))
are out of scope: `lt`'s text fields are line editors, not a rich-text surface.
`TextInput`'s emacs-style bindings (`crates/lt-tui/src/text_input.rs:3-17`) are
the editing vocabulary.

### Platform note

Linear notates modifiers as `Cmd/Ctrl` and `Option/Alt`. In a terminal there is
no Cmd; everything collapses to `ctrl`/`alt`.

## Current machinery assessment

Everything funnels through one router:

```rust
// crates/lt-tui/src/lib.rs:844
match app.mode {
    Mode::Popup(_) => handle_popup_key(app, key.code),
    Mode::Detail => handle_detail_key(app, key.code, key.modifiers),
    Mode::NewIssue => handle_new_issue_key(app, key.code, key.modifiers),
    Mode::Help => handle_help_key(app, key.code, key.modifiers),
    Mode::Search => handle_search_key(app, key.code, key.modifiers),
    Mode::List => handle_normal_key(app, key.code, key.modifiers),
}
```

Each arm is a hand-written `match KeyCode` (`handle_normal_key` lib.rs:858,
`handle_popup_key` popup.rs:441, `handle_help_key` popup.rs:453,
`handle_search_key` popup.rs:487, `handle_detail_key` detail.rs:248,
`handle_new_issue_key` new_issue.rs:410), with sub-focus handled by inline gates
(`app.comment_input.is_some()` detail.rs:250, `modal.focused_field`
new_issue.rs:433). Deficiencies:

- **No indirection.** Keys map straight to `App` method calls; there is nothing
  to enumerate, rebind, or reuse.
- **No chords.** `g i`-style sequences are inexpressible.
- **Help drifts.** `ALL_KEYBINDINGS` (lib.rs:146-247) is a parallel static table
  with no link to the handlers. It already lies: `ui/help.rs:23` says "Esc/q to
  close" but `handle_help_key` closes only on `esc` (`q` is typed into the
  filter), and the `c` entry describes a detail-only binding with no context
  marker.
- **Order-dependent matching.** `Char('d') if ctrl` (lib.rs:889) works only
  because it precedes `Char('d')` (lib.rs:904). Modifier handling is re-derived
  per handler.
- **Duplication.** `o` (open in browser) is implemented twice (lib.rs:895,
  detail.rs:272); `esc` semantics are re-implemented per mode.

Verdict: replace. The `match app.mode` router is the seam; `Mode` and
`NewIssueField` already carry the context information a keymap needs.

## Prior art

Primary evidence: official ratatui documentation and the source of mature
ratatui applications.

- The
  [ratatui Elm architecture guide](https://ratatui.rs/concepts/application-patterns/the-elm-architecture/)
  recommends mapping key events to a message/action enum consumed by an update
  function. The official
  [ratatui/templates](https://github.com/ratatui/templates) component template
  implements `HashMap<Mode, HashMap<Vec<KeyEvent>, Action>>`
  (`component/template/src/config.rs`) with a tick-flushed key sequence buffer
  (`component/template/src/app.rs`).
- [atuin](https://github.com/atuinsh/atuin)
  (`crates/atuin/src/command/client/search/keybindings/key.rs`) normalizes
  crossterm events into a canonical key type — uppercase char absorbs SHIFT,
  `BackTab` → `Tab`+shift — with a test pinning that `parse` and `from_event`
  agree. Its `interactive.rs` resolves `g g`-style chords with a pending key and
  **no timer**: the chord resolves on the next keystroke or falls back to the
  single-key binding.
- [television](https://github.com/alexpasmantier/television)
  (`television/keymap.rs`) layers contextual keymaps over a global fallback and
  builds a reverse action→key index to render key hints, so help never drifts.
- [gitui](https://github.com/gitui-org/gitui) (`src/keys/key_list.rs`) uses a
  named-field struct per binding — lowest indirection, but no map to iterate for
  help and no chord support; not a fit here.
- Crates assessed: [crokey](https://docs.rs/crokey) (combination parsing +
  `key!` macro, no sequences) and [keymap](https://docs.rs/keymap) (serde key
  strings, no sequences, 0.x). Neither covers sequences + contextual layers +
  help enumeration; every mature app above rolls a thin owned layer, and so do
  we.

Design adopted: **action enum + normalized key type + layered contextual
tables + no-timer pending prefix + help generated from the tables**.

## Architecture

### Module layout

```text
crates/lt-tui/src/keymap/
  mod.rs      KeyContext, Binding, Resolved, resolve(), static binding
              tables, help_rows(), invariant + snapshot tests
  key.rs      Key: From<KeyEvent> normalization, Display, FromStr, const ctors
  action.rs   Action enum + label()
```

Deleted: `ALL_KEYBINDINGS`/`HelpEntry` (lib.rs:139-247) and the dispatch layers
of the six handlers. No new dependencies.

### Key flow

```text
crossterm KeyEvent (EventSource::next_key, Press-filtered)
        |
        v
  Key::from(KeyEvent)      normalize: strip kitty state bits;
        |                  BackTab -> tab+shift; ctrl+char lowercased;
        v                  SHIFT folded into char case ('G', not shift+g)
  dispatch_key(app, key)
        |            esc while a chord is pending? -> cancel, done
        v
  key_context(app)         List | Detail | Popup | NewIssuePicker
        |                  | Search | Help | NewIssueText | CommentInput
        v
  resolve(ctx, pending, key)
        |
        +-- Act(action)  -> apply_<context>(app, action)
        +-- Pending(key) -> app.pending_key = Some(key); status row shows it
        +-- Unbound(key) -> text contexts: forward to editor widget
                            other contexts: ignore
```

### Types

```rust
/// A normalized key press. Canonical form:
/// - Char keys carry case in the char itself; SHIFT is always cleared
///   for Char (shift+p arrives as Char('P'), stored as "P").
/// - ctrl+letter is stored lowercase ("ctrl+d", never "ctrl+D").
/// - BackTab is normalized to Tab + SHIFT.
/// - Only CONTROL | ALT | SHIFT modifier bits are retained.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Key { pub code: KeyCode, pub mods: KeyModifiers }

impl From<KeyEvent> for Key {
    fn from(ev: KeyEvent) -> Self;            // sole entry from crossterm
}

impl Key {
    pub const fn char(c: char) -> Self;       // table-building ctors
    pub const fn ctrl(c: char) -> Self;
    pub const fn plain(code: KeyCode) -> Self;
    pub const fn alt(code: KeyCode) -> Self;
}
// Display: "g", "G", "ctrl+d", "shift+tab", "enter", "esc", "?" ...
// FromStr: inverse; accepts "shift+p" and folds it to 'P'. Round-trip
// is test-enforced. This is the future config surface: serde via
// #[serde(try_from = "String")] needs no shape change.

pub enum Binding {
    Single(Key),
    Chord(Key, Key),   // "g g", "g i", future "o w"
}
```

Linear's chords are exactly two keys; `Chord(Key, Key)` over `Vec<Key>` makes
deeper nesting unrepresentable rather than untested.

The SHIFT-fold is load-bearing: terminals variously deliver `G` as
`Char('G')+SHIFT` or `Char('G')` alone. Without normalization a `G` binding
would be terminal-dependent (this is why atuin and the ratatui template both do
it).

```rust
pub enum Action {
    // navigation -- interpreted by the active context
    MoveUp, MoveDown, MoveTop, MoveBottom,
    HalfPageUp, HalfPageDown, PageUp, PageDown, NextPage, PrevPage,
    // app-level
    Quit, Back, OpenHelp, OpenSearch, OpenDetail, CreateIssue,
    Refresh, Login, OpenInBrowser,
    // issue fields
    SetStatus, SetPriority, SetAssignee, ToggleSortDirection, Comment,
    // forms
    Submit, Confirm, NextField, PrevField, PickMe,
    // search overlay
    ClearQuery, CompleteNext, CompletePrev, CompleteAccept,
    CompleteForward, CompleteBackward,
}

impl Action {
    /// Display name for the help overlay (and a future palette).
    pub fn label(self) -> &'static str;
}
```

Navigation actions are semantic — `MoveDown` is list-selection in List, scroll
in Detail, item-selection in Popup — which keeps the enum small. A reserved
Linear binding becomes one new variant + one table row + one apply arm when its
feature lands; that is the entire extensibility story.

### Contexts and layering

```rust
pub enum KeyContext {
    List, Detail, Popup, NewIssuePicker,           // full keymap contexts
    Search, Help, NewIssueText, CommentInput,      // text contexts
}
```

`key_context(app)` derives the context from `App` state, absorbing the inline
gates at detail.rs:250 (comment input) and new_issue.rs:433 (focused field:
Title/Description → `NewIssueText`, pickers → `NewIssuePicker`).

Two layers, resolved context-first:

- The context's own table.
- `GLOBAL` — **navigation vocabulary only** (`j`/`k`/arrows, `g g`, `G`,
  `ctrl+d`/`ctrl+u`, page keys). Action keys (`q`, `c`, `ctrl+/`, `/`) are
  per-context. This eliminates shadowing surprises (a global `q` would quit the
  app from inside a popup) at the cost of repeating `esc → Back` in each table.

Text contexts (a) skip the `GLOBAL` layer — a `j` binding must not steal the
letter from the query bar, (b) never start chords, (c) forward unbound keys to
their editor widget (`TextInput::handle_key`, the description editor, the
comment buffer), preserving the widgets' existing behavior including the search
debounce touch (popup.rs:548-553) and `enter`-as-newline in multiline fields
(detail.rs:304, new_issue.rs:513).

### Resolution and chords

```rust
pub enum Resolved { Act(Action), Pending(Key), Unbound(Key) }

pub fn resolve(ctx: KeyContext, pending: Option<Key>, key: Key) -> Resolved {
    // layers: context table, then GLOBAL unless ctx.is_text()
    // 1. pending chord? try Chord(pending, key) in each layer; on miss,
    //    drop the prefix and resolve `key` fresh (atuin behavior)
    // 2. key is a chord prefix in a layer (and !ctx.is_text())?
    //    -> Pending(key)
    // 3. Single(key) in a layer? -> Act
    // 4. -> Unbound
}
```

- **No timers.** A pending prefix waits indefinitely, like vim's `g`. The 100 ms
  poll loop (lib.rs:843) and `EventSource` are untouched.
- `esc` during a pending chord cancels it and does nothing else (handled in
  `dispatch_key` before `resolve`, so it never triggers the context's `Back`).
- Invariant, enforced by test: within any context's effective layers, no key is
  both `Single`-bound and a chord prefix — so prefix-vs-action ambiguity (the
  reason other systems need timeouts) is structurally impossible.
- Pending state is one field, `App.pending_key: Option<Key>`, rendered in
  `render_status_row` (ui/mod.rs:79-101) as e.g. `g …`, taking priority over the
  plain footer.

Tables are `static` slices of `(Binding, Action)` built with the `const`
constructors — no HashMap, no lazy init; the largest table is ~16 rows and a
linear scan per keypress is irrelevant.

### Dispatch

The router at lib.rs:844-851 becomes:

```rust
if let Some(ev) = events.next_key(Duration::from_millis(100))? {
    dispatch_key(app, Key::from(ev));
}
```

The six handlers become per-context `apply_*(app: &mut App, action: Action)`
functions in their current files, bodies unchanged, arms keyed on `Action`
instead of `KeyCode` + modifier booleans:

| Today                                   | Becomes                          |
| --------------------------------------- | -------------------------------- |
| `handle_normal_key` lib.rs:858          | `apply_list`                     |
| `handle_detail_key` detail.rs:248       | `apply_detail`                   |
| `handle_popup_key` popup.rs:441         | `apply_popup`                    |
| `handle_help_key` popup.rs:453          | `apply_help` + forward to filter |
| `handle_search_key` popup.rs:487        | `apply_search` + forward to bar  |
| `handle_new_issue_key` new_issue.rs:410 | `apply_new_issue` + forward      |

Behavior that moves intact: double-esc reset (lib.rs:862-882) into
`apply_list`'s `Back` arm; the team-field background load (new_issue.rs:453-495)
into the `Confirm`/`NextField` arms. `EventSource` and `ScriptedEvents` keep
`KeyEvent` as the wire type; normalization happens once at the dispatch
boundary.

## Default binding tables

### Conflict resolutions

| Conflict                                  | Resolution                                                                                                                                 |
| ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `lt` `g` = top vs Linear `g` chord prefix | `g g` = top (vim), `G` = bottom unchanged. `g` becomes a prefix; `g i`/`g m`/`g t` slot in when those features exist.                      |
| `lt` `o` = browser vs Linear `o` prefix   | Open-in-browser moves to `ctrl+o`. `o` is left unbound (reserved) — an empty prefix would be dead weight; chords register per feature.     |
| `lt` `n` = new issue vs Linear `c`        | `c` = create issue in List; `n` unbound. `c` in Detail stays comment (context tables shadow nothing — create is List-scoped).              |
| `lt` `space` = open vs Linear `enter`     | `enter` opens detail; `space` kept as alias. Revisit if multi-select ever wants `space`.                                                   |
| `lt` `S` = cycle sort vs Linear `S`       | Cycle-sort functionality is removed (sort remains expressible via `/` `sort:` stems). `S` reserved for subscribe, `V` for display options. |
| `lt` `?` = keybind help vs Linear `?`     | Linear's `?` opens the help panel, which `lt` lacks; the keybinds panel mirrors Linear's `ctrl+/`. `?` reserved for a future help panel.   |
| Non-conflicts after normalization         | `d` (sort dir) vs `D` (due date), `L` (login) vs `l` (labels), pagination `ctrl+n`/`ctrl+p` vs completion `ctrl+n`/`ctrl+p` (layering).    |

### GLOBAL (skipped by text contexts)

| Binding         | Action       |
| --------------- | ------------ |
| `j` / `down`    | MoveDown     |
| `k` / `up`      | MoveUp       |
| `g g`           | MoveTop      |
| `G`             | MoveBottom   |
| `ctrl+d`        | HalfPageDown |
| `ctrl+u`        | HalfPageUp   |
| `pgdn` / `pgup` | PageDown/Up  |

### List

| Binding             | Action            | Binding  | Action              |
| ------------------- | ----------------- | -------- | ------------------- |
| `enter` / `space`   | OpenDetail        | `ctrl+r` | Refresh             |
| `esc`               | Back (double-esc) | `d`      | ToggleSortDirection |
| `/`                 | OpenSearch        | `ctrl+o` | OpenInBrowser       |
| `ctrl+/` / `ctrl+7` | OpenHelp          | `ctrl+n` | NextPage            |
| `c`                 | CreateIssue       | `ctrl+p` | PrevPage            |
| `s`                 | SetStatus         | `L`      | Login               |
| `p`                 | SetPriority       | `q`      | Quit                |
| `a`                 | SetAssignee       |          |                     |

`ctrl+7` is the legacy alias for `ctrl+/`: without the kitty protocol, terminals
send Ctrl+/ as 0x1F, which crossterm decodes as ctrl+`'7'` (crossterm 0.29,
`src/event/sys/unix/parse.rs:110-113`); kitty-enhanced terminals deliver a true
`ctrl+/`. Both are bound, like the submit chords. Cycle-sort (`S` today) is
removed outright — `S` is reserved for subscribe — and its functionality remains
reachable via `/` `sort:` stems.

### Detail

`esc` / `q` → Back, `c` → Comment, `ctrl+o` → OpenInBrowser, plus GLOBAL
(navigation = scrolling; `g g` replaces today's `g` for scroll-to-top).

### Popup

`enter` → Confirm, `esc` → Back, plus GLOBAL (MoveTop/MoveBottom clamp to
first/last item).

### New issue — picker fields

`ctrl+enter` / `alt+enter` → Submit, `esc` → Back, `tab` → NextField,
`shift+tab` → PrevField, `enter` → Confirm (advance; Team triggers the
background load), `m` → PickMe, plus GLOBAL for `j`/`k`.

### New issue — text fields (text context)

`ctrl+enter` / `alt+enter` → Submit, `esc` → Back, `tab` → NextField,
`shift+tab` → PrevField. Everything else forwards to the editor (`enter` inserts
a newline in the description).

### Comment input (text context)

`ctrl+enter` / `alt+enter` → Submit, `esc` → Back. Everything else forwards.

### Search (text context)

`esc` → Back, `enter` → Confirm, `ctrl+c` → ClearQuery, `down`/`up` →
MoveDown/MoveUp, `ctrl+n`/`ctrl+p` → CompleteNext/Prev, `ctrl+y` →
CompleteAccept, `tab`/`shift+tab` → CompleteForward/Backward. Everything else
forwards to the query bar.

### Help (text context)

`esc` → Back, `down`/`j` → MoveDown, `up`/`k` → MoveUp. Everything else forwards
to the filter bar. (`j`/`k` remain untypeable in the filter — an existing
limitation, carried forward deliberately.)

## Help overlay from the keymap

`ALL_KEYBINDINGS` and `HelpEntry` are deleted. The keymap module provides:

```rust
pub struct HelpRow {
    pub bindings: Vec<Binding>, // e.g. [j, down] or [g g]
    pub label: &'static str,    // Action::label()
    pub context: &'static str,  // "global", "list", "detail", ...
}

/// Iterate GLOBAL then each context table in declaration order; group
/// consecutive rows by (context, action).
pub fn help_rows() -> Vec<HelpRow>;
```

`HelpPopup` (popup.rs:61-94) stores `rows: Vec<HelpRow>` built once; the
renderer joins the bindings' `Display` forms with `" / "` ("j / down",
"ctrl+enter / alt+enter"), and `update_filter` matches against that rendered
form, the label, and the context. `ui/help.rs` reads the rows and gains a
context column. Help can no longer drift because it _is_ the keymap; the
existing inaccuracies (the "Esc/q" title, the context-less `c` entry) disappear
as a class.

## Kitty keyboard protocol and the submit chord

The loop enables the kitty protocol when supported (lib.rs:778-787);
`Session.keyboard_enhanced` (lib.rs:270-277) exists because plain terminals
cannot encode `ctrl+enter`. Today's handlers accept ctrl **or** alt
unconditionally (new_issue.rs:417, detail.rs:290) and `keyboard_enhanced` only
selects the hint string. The keymap replicates this exactly: both `ctrl+enter`
and `alt+enter` are statically bound to `Submit` — no runtime capability
switching. Without kitty, `ctrl+enter` arrives as plain `enter` (newline in
multiline fields, today's behavior) and `alt+enter` is the escape hatch. The
same both-bound pattern covers `ctrl+/`/`ctrl+7` for the shortcuts panel. The
`From<KeyEvent>` conversion strips kitty's extra state bits so enhanced and
legacy terminals produce identical `Key` values for every table entry.

## Testing strategy

Inline `#[cfg(test)]` modules per [[testing.md]].

1. **Round-trip agreement** (key.rs): for representative `KeyEvent`s —
   `Char('G')+SHIFT`, `BackTab`, `Char('D')+CTRL+SHIFT`, kitty state bits —
   assert `Key::from(ev).to_string().parse() == Key::from(ev)`; `FromStr`
   leniency (`"shift+p"` == `"P"`); `Binding` round-trip (`"g g"`).
2. **Resolution units** (mod.rs): chord hit (`g`,`g` → MoveTop); chord miss
   falls through (`g`,`j` → MoveDown); text context ignores GLOBAL (`c` in
   Search → Unbound); layer precedence (`q` → Back in Detail, Quit in List).
3. **Invariants** (mod.rs), over every context's effective layers: no duplicate
   `Binding`; no key both `Single`-bound and a chord prefix; every table binding
   round-trips through Display/FromStr.
4. **Binding snapshot** (insta): render every context's `binding → label` lines
   and snapshot them. Any binding change becomes a reviewed snapshot diff — the
   drift guard, mirroring this document's tables. A second snapshot pins
   `help_rows()`.
5. **Loop tests** (existing `ScriptedEvents` harness, loop_tests.rs): `g`,`g`
   selects top; `g`,`j` moves down; `enter` opens detail; `c` opens the create
   modal; `esc` cancels a pending `g` without touching `last_esc_time`. The
   existing `run_app_dispatches_keys_and_quits` passes unmodified.
6. **Render test**: footer shows the pending-prefix indicator (render-test
   pattern with `pending_key = Some(...)`).

## Implementation plan

Each phase lands gate-green (`make check` / `make test`).

1. **Keymap core + non-text contexts.** Add `keymap/{mod,key,action}.rs` with
   tables and tests 1-4; add `App.pending_key`; replace the router with
   `dispatch_key`; convert `handle_normal_key`/`handle_detail_key`/
   `handle_popup_key` to `apply_*` (the comment-input gate temporarily keeps
   forwarding to the old comment handler). Binding changes land here: `g g`/`G`,
   `enter`+`space`, `c` create, `ctrl+o` browser, `ctrl+r` refresh,
   `ctrl+/`+`ctrl+7` help. Cycle-sort is removed outright (`App::cycle_sort`,
   its `S` binding, and its help entry). The dead `App.input_mode`/`input_buf`
   filter state (lib.rs:323-325) and the unreachable footer branch
   (ui/mod.rs:94-95) are deleted (approved in review). Pending indicator in the
   status row. Patch `ALL_KEYBINDINGS` strings minimally so help doesn't lie in
   the interim. Extend loop tests.
2. **Text contexts.** `key_context` grows the comment-input and `NewIssueField`
   derivations; Search/Help/NewIssue/CommentInput move onto the keymap with
   forward-to-editor; delete the dispatch layers of their old handlers (the
   editing widgets remain). Behavior-neutral; existing render tests are the
   guard.
3. **Help from the keymap.** `help_rows()`, `HelpPopup.rows`, rewrite
   `ui/help.rs`, delete `ALL_KEYBINDINGS`/`HelpEntry`, add the help snapshot,
   update the footer hints (ui/chrome.rs:92-99) and replace [[tui-modal.md]]'s
   binding tables with a link here.
4. **Parity follow-ups** (separate issues; each is one table row + one variant +
   one apply arm): `i` assign-to-me (viewer id is already fetched; action is one
   `enqueue_assignee_change`), `s`/`p`/`a` from Detail (blocked on popup
   return-mode, below), first real `g`/`o` chords as inbox/my-issues /workspaces
   land (ENG-41 = `o w`).

## Risks and flagged issues

- **Popup return-mode.** `popup_confirm`/`popup_cancel` hardcode `Mode::List`
  (popup.rs:341,346), as does `new_issue_submit` (new_issue.rs:243). Fine today
  (popups only open from List), but phase-4 "s/p/a from Detail" needs a
  return-mode field first. Not solved here; do not bind Detail `s` without it.
- **Muscle-memory breaks.** `g` → `g g`, `o` → `ctrl+o`, `n` → `c`, `r` →
  `ctrl+r`, `?` → `ctrl+/`, and the removal of `S` cycle-sort are deliberate
  breaking changes, acceptable in 0.1.x per [[posture.md]]; the help overlay and
  footer hints are updated in the same phases.
