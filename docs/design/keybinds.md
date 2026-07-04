# Keybinds: Linear Parity and Keymap Machinery

## Context

The TUI should use the same (or as similar as possible) keybinds as the
linear.app web client (ENG-26). Linear's core interaction patterns — two-key
chord navigation (`g` then `i`, `o` then `w`) and single-letter contextual
actions on the focused issue — cannot be expressed by the current input
machinery, which is a per-view `match KeyCode` with no key→action indirection,
no chord support, and a hand-maintained help table that has already drifted from
the real handlers.

This document inventories Linear's shortcuts, assesses the current machinery,
specifies its replacement, and lays out the implementation plan. It supersedes
the binding tables in [[tui-modal.md]]; the mode taxonomy described there is
replaced by the delivered view stack of [[tui-app-event-queue-adr.md]], which
this design targets: key presses arrive on the unified event queue as
`AppEvent::Key`, cascade down the stack of live views, and land on the Esc/q
floor (Decision 6).

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
`TextInput`'s line-editing bindings (`crates/lt-tui/src/text_input.rs:3-17`) are
the editing vocabulary.

### Platform note

Linear notates modifiers as `Cmd/Ctrl` and `Option/Alt`. In a terminal there is
no Cmd; everything collapses to `ctrl`/`alt`.

## Current machinery assessment

[[tui-app-event-queue-adr.md]] (delivered) already fixed the routing shell.
`App::apply` hands `AppEvent::Key` to `dispatch_key`
(`crates/lt-tui/src/lib.rs:1120`), which checks four layers in order (Decision
6): the focused view's own handler; the shared scroll defaults
(`ScrollMotion::from_key`, lib.rs:179, resolved at the focused view only); the
cascade toward the base for anything still unconsumed; and the Esc/q floor —
Back (pop) above the base, double-esc reset and quit at it. What remains inside
each layer-one handler is a hand-written `match KeyCode`:

```rust
// crates/lt-tui/src/lib.rs:1147
fn handle_view_key(&mut self, i: usize, key: KeyEvent) -> KeyFlow {
    let handler: KeyHandler = match &self.views[i] {
        View::List(_) => handle_list_key,
        View::Detail(_) => detail::handle_key,
        View::Popup(_) => popup::handle_key,
        View::NewIssue(_) => new_issue::handle_key,
        View::Search(_) => popup::handle_search_key,
        View::Help(_) => popup::handle_help_key,
    };
    handler(self, i, key)
}
```

(`handle_list_key` lib.rs:1454, `detail::handle_key` detail.rs:184,
`popup::handle_key` popup.rs:507, `popup::handle_help_key` popup.rs:519,
`popup::handle_search_key` popup.rs:556, `new_issue::handle_key`
new_issue.rs:398), with sub-focus handled by inline gates
(`d.comment_input.is_some()` detail.rs:189, `modal.focused_field`
new_issue.rs:423). Deficiencies:

- **No indirection.** Keys map straight to `App` method calls; there is nothing
  to enumerate, rebind, or reuse.
- **No chords.** `g i`-style sequences are inexpressible.
- **Help drifts.** `ALL_KEYBINDINGS` (lib.rs:533-634) is a parallel static table
  with no link to the handlers. It already lies twice about `q` alone: its `q`
  entry says "quit", but since the floor landed, `q` above the base is Back
  (lib.rs:1137-1141); the help popup's own title says "Esc/q to close"
  (`ui/help.rs:23`) but `q` is typed into the filter (popup.rs:543-548).
- **Cross-layer shadowing by hand.** List's `d` (toggle sort direction) needs a
  `!ctrl` guard (lib.rs:1481) so it does not swallow the Ctrl-d scroll default
  that resolves one layer later; nothing enforces such guards, and modifier
  decoding is re-derived per handler.
- **Duplication.** `o` (open in browser) is implemented twice (lib.rs:1468-1475,
  detail.rs:205-211).

Verdict: replace the handler bodies, keep the shell. The dispatch walk, the
scroll seam, the cascade, and the floor stand as delivered; the keymap replaces
what each view does with a key — the per-view `match KeyCode` — with table
resolution. The view variants and `NewIssueField` carry the context information
a keymap needs.

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
  mod.rs      Binding, Resolved, resolve(), the shared GLOBAL table,
              help_rows(), invariant + snapshot tests
  key.rs      Key: From<KeyEvent> normalization, Display, FromStr, const ctors
  action.rs   Action enum + label()
```

The keymap module is vocabulary and resolution machinery only. Each view
declares its own binding tables and a `Keymap` — resolution layers, apply
function, unbound policy — next to its state and handlers (lib.rs for the list,
detail.rs, popup.rs, new_issue.rs), and `View::keymap()` selects the declaration
from the view's own state (ENG-46).

Deleted: `ALL_KEYBINDINGS`/`HelpEntry` (lib.rs:527-634),
`ScrollMotion::from_key` (lib.rs:179-192, subsumed by the `GLOBAL` table), and
the `match KeyCode` bodies of the six handlers. No new dependencies.

### Key flow

```text
crossterm KeyEvent (input thread, Press-filtered)
        |
        v
  AppEvent::Key(KeyEvent) on the app event queue
        |                  ([[tui-app-event-queue-adr.md]] Decision 10)
        v
  Key::from(KeyEvent)      normalize: strip kitty state bits;
        |                  BackTab -> tab+shift; ctrl+char lowercased;
        v                  SHIFT folded into char case ('G', not shift+g)
  dispatch_key(app, key)   the AppEvent::Key arm of App::apply
        |            esc while a chord is pending? -> cancel, done
        v
  walk the view stack, top down; per view:
    view.keymap()          the view's declared Keymap; sub-focus selects
        |                  (Detail's open comment input, NewIssue's field)
        v
    resolve(keymap.layers, pending, key)
        |
        +-- Act(action)  -> navigation via View::scroll; anything else
        |                   via the keymap's apply fn; consumed
        +-- Pending(key) -> app.pending_key = Some(key); status row
        |                   shows it; consumed
        +-- Unbound(key) -> esc: pass to the floor (never forwarded);
        |                   else per the keymap's unbound policy:
        |                   Forward -> the view's editor widget; consumed
        |                   Swallow -> consumed (a form swallows strays)
        |                   Cascade -> pass to the view beneath
        v (no view consumed the key)
  the Esc/q floor           delivered (lib.rs:1137-1143): esc/q pop above
                            the base; at the base, esc = double-esc reset,
                            q = quit
```

### Types

```rust
/// A normalized key press. Canonical form:
/// - Char keys carry case in the char itself; SHIFT is always cleared
///   for Char (shift+p arrives as Char('P'), stored as "P").
/// - ctrl+letter is stored lowercase ("ctrl+d", never "ctrl+D").
/// - BackTab is normalized to Tab + SHIFT.
/// - Esc always clears every modifier: esc is esc, regardless of what
///   shift/alt/ctrl the terminal tacked onto it.
/// - SHIFT is cleared for every other code except Tab (whose SHIFT bit is
///   what distinguishes it from shift+tab); Char already folds SHIFT into
///   case above -- this is what keeps shift+enter/ctrl+shift+enter and
///   shift+arrow/pgdn matching their unshifted bindings.
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
    Back, OpenHelp, OpenSearch, OpenDetail, CreateIssue,
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
in Detail, item-selection in Popup — which keeps the enum small. The scroll
family (`MoveUp` through `PageUp`) maps 1:1 onto the delivered `ScrollMotion`
(lib.rs:165-177) and applies through the delivered `View::scroll` overrides
(lib.rs:144-157), which already implement exactly those per-view semantics.
There is no `Quit` variant: quit is the floor's, not a binding's. A reserved
Linear binding becomes one new variant + one table row + one apply arm when its
feature lands; that is the entire extensibility story.

### View-declared keymaps and layering

```rust
/// Declared once per view context, next to the view's state and handlers.
pub struct Keymap {
    pub layers: Layers,                             // own table, shared layers, GLOBAL
    pub apply: Option<fn(&mut App, usize, Action)>, // non-navigation actions
    pub unbound: Unbound,
}

pub enum Unbound {
    Cascade,                                // pass to the view beneath
    Swallow,                                // consumed; a form swallows strays
    Forward(fn(&mut App, usize, KeyEvent)), // to the view's editor widget
}
```

There is no context enum: a parallel context type would have to be kept in sync
with the view variants and their sub-focus (ENG-46). Instead each view declares
its `Keymap`(s) and `View::keymap()` reads the view's own state: `View::List` →
the list keymap; `View::Detail(d)` → the comment-input keymap iff
`d.comment_input.is_some()`, else the detail keymap; `View::NewIssue(m)` by
`m.focused_field` (Title/Description → the text keymap, pickers → the picker
keymap) — the sub-focus decisions live on `DetailView::keymap` and
`NewIssueModal::keymap`, absorbing the old inline gates.

Two layers, resolved own-table-first:

- The view's own table.
- `GLOBAL` — the navigation vocabulary (`j`/`k`/arrows, `g g`, `G`,
  `ctrl+d`/`ctrl+u`, page keys). This is the merge the ADR's reconciliation
  section calls for: GLOBAL and the delivered scroll-default layer deliver
  per-view semantics for the same keys, and GLOBAL wins as the resolution layer.
  The key set of `ScrollMotion::from_key` (lib.rs:179-192) becomes GLOBAL's rows
  (`g` → `g g` is the one change) and the function is deleted; the navigation
  actions apply through the delivered `View::scroll` seam, whose per-view
  overrides (selection movement in List/Popup, offset scrolling in Detail, no-op
  elsewhere) are untouched. Like the scroll defaults it replaces, GLOBAL
  resolves at the focused view only: the stack cascade delivers _keys_ downward;
  GLOBAL delivers per-context _semantics_ for the same key (`j` scrolls in
  Detail, moves the selection in List) — a resolution layer within each view,
  not a layer at the bottom of the stack.

`esc` and `q` are not bindings. The delivered floor — `dispatch_key`'s terminal
arm (lib.rs:1137-1143) — owns them: Back (`pop_view`) above the base; at the
base, the double-esc reset and quit. A table binds `esc` only where it means
something narrower than pop (the comment input's cancel); no table binds `q`
(text contexts forward it to their editor, so it stays typeable). The q-leak the
ADR disclosed is resolved structurally, as delivered: the floor consumes `q` as
Back before it could ever mean Quit from an overlay.

Keymaps split by their declared `Unbound` policy:

- **`Forward` — the text contexts** (Search, Help, the new-issue text fields,
  the comment input): (a) skip the `GLOBAL` layer — a `j` binding must not steal
  the letter from the query bar, (b) never start chords, (c) forward unbound
  keys to their editor widget (`TextInput::handle_key`, the description editor,
  the comment buffer), preserving the widgets' existing behavior including the
  search debounce touch (popup.rs:618-622) and `enter`-as-newline in multiline
  fields (detail.rs:277, new_issue.rs:500-502). Forwarding consumes: printable
  input never cascades. `esc` is the one key never forwarded: unbound `esc`
  passes to the floor instead, keeping overlay-close floor-owned — the keymap
  form of the explicit `Esc => Pass` arms the delivered handlers carry
  (popup.rs:527, popup.rs:564, new_issue.rs:412-419).
- **`Swallow` — the new-issue picker fields** consume unbound keys without
  forwarding: the modal is a form, and a stray letter acting on a view
  underneath it would be hostile. `esc` passes to the floor here too, which pops
  the modal (and unwatches its scopes, `pop_view` lib.rs:979).
- **`Cascade`** (`List`, `Detail`, `Popup`) lets an unbound key cascade to the
  view beneath, ending at `views[0]` and then the floor. This is what makes list
  bindings reachable from overlays.

### Resolution and chords

```rust
pub enum Resolved { Act(Action), Pending(Key), Unbound(Key) }

pub fn resolve(layers: Layers, pending: Option<Key>, key: Key) -> Resolved {
    // layers: the view's declared layer list -- its own table, then any
    // shared layers, then GLOBAL (text contexts declare a single layer,
    // their own table, so GLOBAL never steals a typeable letter)
    // 1. pending chord? try Chord(pending, key) in each layer; on miss,
    //    drop the prefix and resolve `key` fresh (atuin behavior)
    // 2. key is a chord prefix in a layer? -> Pending(key)
    // 3. Single(key) in a layer? -> Act
    // 4. -> Unbound
}
```

- **No timers.** A pending prefix waits indefinitely, like vim's `g`. The prefix
  is `App` state and survives any number of idle ticks of the event loop's
  `recv_timeout` wait ([[tui-app-event-queue-adr.md]] Decision 10); no
  event-loop cooperation is needed.
- **`pending` is `App`-level, not per-view.** `dispatch_key` takes the prefix
  once at entry and clears it; every view in the walk resolves against the same
  taken prefix. A chord miss drops the prefix and resolves the key fresh within
  that same context before the cascade continues.
- `esc` during a pending chord cancels it and does nothing else (handled in
  `dispatch_key` before `resolve`, so it never reaches the floor's Back).
- Invariant, enforced by test: within any context's effective layers, no key is
  both `Single`-bound and a chord prefix — so prefix-vs-action ambiguity (the
  reason other systems need timeouts) is structurally impossible.
- Pending state is one field, `App.pending_key: Option<Key>`, rendered in
  `render_status_row` (ui/mod.rs:78-99) as e.g. `g …`, taking priority over the
  plain footer.

Tables are `static` slices of `(Binding, Action)` built with the `const`
constructors — no HashMap, no lazy init; the largest table is ~16 rows and a
linear scan per keypress is irrelevant.

### Dispatch

The dispatch site is delivered: the `AppEvent::Key` arm of `App::apply`
(lib.rs:1106-1113) feeding `dispatch_key` (lib.rs:1120-1145). The queue's wire
type is the raw crossterm `KeyEvent`, not `keymap::Key`: normalization happens
exactly once, at the boundary between transport and keymap:

```rust
match event {
    AppEvent::Key(ev) => dispatch_key(self, Key::from(ev)),
    // the Runtime(_) arms: not keymap concerns
}
```

`dispatch_key` keeps its delivered four-layer shape with the first two layers
folded into keymap resolution: the per-view handler call and the separate
`ScrollMotion::from_key` check become one `resolve` against the focused view's
declared layers (GLOBAL now carries the scroll keys); on `Act` apply through the
keymap's apply fn and stop; on `Unbound` under a `Cascade` policy repeat against
the view beneath (`KeyFlow::Pass`, lib.rs:515-518); the Esc/q floor stays the
terminal arm, verbatim. `resolve` itself is stack-unaware; the cascade and floor
are dispatch-loop behavior above it.

The six handlers become per-view
`apply_*(app: &mut App, idx: usize, action: Action)` functions in their current
files, referenced from each view's `Keymap` declaration, bodies unchanged, arms
keyed on `Action` instead of `KeyCode` + modifier booleans (the `idx` re-fetches
the view, per the ADR's borrow rule: no view borrow crosses a `&mut App` call).
Help declares no apply fn (`apply: None`): its table is navigation-only, and
everything else forwards to its filter bar.

| Today                                    | Becomes                     |
| ---------------------------------------- | --------------------------- |
| `handle_list_key` lib.rs:1454            | `apply_list`                |
| `detail::handle_key` detail.rs:184       | `apply_detail`              |
| `popup::handle_key` popup.rs:507         | `apply_popup`               |
| `handle_help_key` popup.rs:519           | forward to filter           |
| `handle_search_key` popup.rs:556         | `apply_search` + forward    |
| `new_issue::handle_key` new_issue.rs:398 | `apply_new_issue` + forward |

Behavior that moves intact: the team watch swap (`new_issue_team_changed`,
new_issue.rs:254-268, called from the Tab/Enter arms) into the
`Confirm`/`NextField` arms. `Action::Back` survives in exactly one table — the
comment input's `esc`, which cancels the input without popping the Detail view
beneath it (detail.rs:262-267), narrower than the floor's pop. Every other
close/cancel path is the floor's: confirm/cancel pop the view and restore
whatever is beneath, and the base's double-esc reset stays `handle_list_esc`
(lib.rs:1061-1077), invoked by the floor, outside the keymap.

## Default binding tables

### Conflict resolutions

| Conflict                                  | Resolution                                                                                                                                                     |
| ----------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `lt` `g` = top vs Linear `g` chord prefix | `g g` = top (vim), `G` = bottom unchanged. `g` becomes a prefix; `g i`/`g m`/`g t` slot in when those features exist.                                          |
| `lt` `o` = browser vs Linear `o` prefix   | `o` becomes a prefix now: open-in-browser moves to `o b`, an `lt`-specific chord (Linear, being the browser, has none). `o i`/`o p`/`o w` slot in per feature. |
| `lt` `n` = new issue vs Linear `c`        | `c` = create issue in List; `n` unbound. `c` in Detail stays comment: a key bound in the focused view never cascades, so Detail shadows List's create.         |
| `lt` `space` = open vs Linear `enter`     | `enter` opens detail; `space` kept as alias. Revisit if multi-select ever wants `space`.                                                                       |
| `lt` `S` = cycle sort vs Linear `S`       | Cycle-sort functionality is removed (sort remains expressible via `/` `sort:` stems). `S` reserved for subscribe, `V` for display options.                     |
| `lt` `?` = keybind help vs Linear `?`     | Linear's `?` opens the help panel, which `lt` lacks; the keybinds panel mirrors Linear's `ctrl+/`. `?` reserved for a future help panel.                       |
| Non-conflicts after normalization         | `d` (sort dir) vs `D` (due date), `L` (login) vs `l` (labels), pagination `ctrl+n`/`ctrl+p` vs completion `ctrl+n`/`ctrl+p` (layering).                        |

`esc` and `q` appear in no table below (the comment input's `esc` excepted): the
delivered floor handles them — Back above the base; double-esc reset and quit at
it.

### GLOBAL (skipped by text contexts)

The delivered scroll-default key set (`ScrollMotion::from_key`), with `g`
becoming the `g g` chord:

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

| Binding             | Action      | Binding  | Action              |
| ------------------- | ----------- | -------- | ------------------- |
| `enter` / `space`   | OpenDetail  | `ctrl+r` | Refresh             |
| `/`                 | OpenSearch  | `d`      | ToggleSortDirection |
| `ctrl+/` / `ctrl+7` | OpenHelp    | `o b`    | OpenInBrowser       |
| `c`                 | CreateIssue | `ctrl+n` | NextPage            |
| `s`                 | SetStatus   | `ctrl+p` | PrevPage            |
| `p`                 | SetPriority | `L`      | Login               |
| `a`                 | SetAssignee |          |                     |

`ctrl+7` is the legacy alias for `ctrl+/`: without the kitty protocol, terminals
send Ctrl+/ as 0x1F, which crossterm decodes as ctrl+`'7'` (crossterm 0.29,
`src/event/sys/unix/parse.rs:110-113`); kitty-enhanced terminals deliver a true
`ctrl+/`. Both are bound, like the submit chords. Cycle-sort (`S` today,
lib.rs:1478 / `App::cycle_sort` lib.rs:1022) is removed outright — `S` is
reserved for subscribe — and its functionality remains reachable via `/` `sort:`
stems. The hand-written `!ctrl` guard on `d` (lib.rs:1481) disappears
structurally: after normalization, `d` and `ctrl+d` are distinct `Key` values
resolved in distinct layers.

### Detail

`c` → Comment, `o b` → OpenInBrowser, plus GLOBAL (navigation = the delivered
offset-scrolling override; `g g` replaces today's `g` for scroll-to-top).
`esc`/`q` close via the floor, as today. Pass-through: unbound keys cascade to
the base list (e.g. `/` opens Search, `s`/`p`/`a` act on the list selection
until phase 4 binds them here).

### Popup

`enter` → Confirm, plus GLOBAL (the delivered selection-movement override;
MoveTop/MoveBottom clamp to first/last item). `esc`/`q` close via the floor, as
today ([[tui-app-event-queue-adr.md]] behavior change 14). Pass-through.

### New issue — picker fields

`ctrl+enter` / `alt+enter` → Submit, `tab` → NextField, `shift+tab` → PrevField,
`enter` → Confirm (advance; leaving Team triggers the watch swap), `m` → PickMe,
plus GLOBAL for `j`/`k`. `esc` passes to the floor, which pops the modal.

### New issue — text fields (text context)

`ctrl+enter` / `alt+enter` → Submit, `tab` → NextField, `shift+tab` → PrevField.
Everything else forwards to the editor (`enter` inserts a newline in the
description); `esc` passes to the floor.

### Comment input (text context)

`ctrl+enter` / `alt+enter` → Submit, `esc` → Back (cancel the input, keeping the
Detail view — the one `esc` table row in the keymap). Everything else forwards.

### Search (text context)

`enter` → Confirm, `ctrl+c` → ClearQuery, `down`/`up` → MoveDown/MoveUp,
`ctrl+n`/`ctrl+p` → CompleteNext/Prev, `ctrl+y` → CompleteAccept,
`tab`/`shift+tab` → CompleteForward/Backward. Everything else forwards to the
query bar; `esc` passes to the floor.

### Help (text context)

`down`/`j` → MoveDown, `up`/`k` → MoveUp. Everything else forwards to the filter
bar; `esc` passes to the floor. (`j`/`k` remain untypeable in the filter — an
existing limitation, carried forward deliberately.)

## Help overlay from the keymap

`ALL_KEYBINDINGS` and `HelpEntry` are deleted. The keymap module provides:

```rust
pub struct HelpRow {
    pub bindings: Vec<Binding>, // e.g. [j, down] or [g g]
    pub label: &'static str,    // Action::label()
    pub context: &'static str,  // "global", "list", "detail", ...
}

/// Iterate the passed contexts in order; group consecutive rows by
/// (context, action). Appends static rows for the floor (esc/q: back; at
/// the list, reset/quit) -- the floor is dispatch behavior, not a binding,
/// but the panel must still show it.
pub fn help_rows(contexts: &[(&str, &[Table])]) -> Vec<HelpRow>;
```

The displayed-context registry (`HELP_CONTEXTS`) lives with the view stack: it
enumerates each view's declared tables under a user-facing display name, GLOBAL
first, and collapses the two new-issue contexts into one "new issue" entry (the
text context's table _is_ the shared form-nav layer, so form-nav plus the
picker's own rows is their union, with no duplicates).

`HelpPopup` (popup.rs:107-131) stores `rows: Vec<HelpRow>` built once; the
renderer joins the bindings' `Display` forms with `" / "` ("j / down",
"ctrl+enter / alt+enter"), and `update_filter` matches against that rendered
form, the label, and the context. `ui/help.rs` reads the rows and gains a
context column. Help can no longer drift because it _is_ the keymap; the
existing inaccuracies (the "Esc/q to close" title while `q` filters, the `q` =
"quit" entry that is wrong everywhere above the base) disappear as a class.

## Kitty keyboard protocol and the submit chord

The loop enables the kitty protocol when supported (lib.rs:1330-1356);
`Session.keyboard_enhanced` (lib.rs:703-710) exists because plain terminals
cannot encode `ctrl+enter`. Today's handlers accept ctrl **or** alt
unconditionally (new_issue.rs:405-410, detail.rs:256-260) and
`keyboard_enhanced` only selects the hint string (`submit_key_label`,
ui/new_issue.rs:13-18). The keymap replicates this exactly: both `ctrl+enter`
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
   Search → Unbound); layer precedence (a context row wins over a GLOBAL row for
   the same key). Dispatch units: an unbound key in a Popup resolves in the List
   beneath; `q` above the base pops via the floor, never Quit; a printable key
   in Search never cascades; `esc` in Search reaches the floor, not the query
   bar. (The delivered cascade/floor tests cover the floor itself; these pin the
   keymap's pass policies.)
3. **Invariants** (mod.rs), over every declared keymap's layers: no duplicate
   `Binding`; no key both `Single`-bound and a chord prefix; every table binding
   round-trips through Display/FromStr; no table binds `q`, and none binds `esc`
   except the comment input's (Back/quit are the floor's).
4. **Binding snapshot** (insta): render every context's `binding → label` lines
   and snapshot them. Any binding change becomes a reviewed snapshot diff — the
   drift guard, mirroring this document's tables. A second snapshot pins
   `help_rows()`.
5. **Loop tests** (the `EventPump::Scripted` harness of
   [[tui-app-event-queue-adr.md]] Decision 10, loop_tests.rs, scripting
   `AppEvent::Key(...)` entries): `g`,`g` selects top; `g`,`j` moves down;
   `enter` opens detail; `c` opens the create modal; `esc` cancels a pending `g`
   without touching `last_esc_time`. The existing
   `run_app_dispatches_keys_and_quits` assertions (loop_tests.rs:431) survive
   unchanged.
6. **Render test**: footer shows the pending-prefix indicator (render-test
   pattern with `pending_key = Some(...)`).

## Implementation plan

Each phase lands gate-green (`make check` / `make test`).

1. **Keymap core + non-text contexts.** Add `keymap/{mod,key,action}.rs` with
   tables and tests 1-4; add `App.pending_key`; wire keymap resolution into the
   delivered dispatch seam: `dispatch_key`'s per-view handler call and
   `ScrollMotion::from_key` layer (lib.rs:1120-1145) become one `resolve` per
   view, the cascade and floor arms unchanged. Convert
   `handle_list_key`/`detail::handle_key`/`popup::handle_key` to `apply_*` (the
   comment-input gate temporarily keeps forwarding to
   `handle_comment_input_key`, detail.rs:252). The pass-through policy (which
   keymaps `Pass` on `Unbound`) becomes per-view data — the delivered handlers
   already return `Pass` for unbound keys; the keymap encodes the same policy in
   each view's declared `Unbound`. Binding changes land here: `g g`/`G`,
   `enter`+`space`, `c` create, `o b` browser (the first `o` chord), `ctrl+r`
   refresh, `ctrl+/`+`ctrl+7` help. Cycle-sort is removed outright
   (`ListView::cycle_sort` lib.rs:364, `App::cycle_sort` lib.rs:1022, the `S`
   arm lib.rs:1478, and its help entry). Pending indicator in the status row.
   Patch `ALL_KEYBINDINGS` strings minimally so help doesn't lie in the interim.
   Extend loop tests.
2. **Text contexts.** `View::keymap` absorbs the comment-input and
   `NewIssueField` sub-focus gates (detail.rs:189 and new_issue.rs:423) via
   `DetailView::keymap`/`NewIssueModal::keymap`;
   Search/Help/NewIssue/CommentInput move onto the keymap with
   forward-to-editor, their explicit `Esc => Pass` arms replaced by the
   esc-is-never-forwarded rule; delete the dispatch layers of their old handlers
   (the editing widgets remain). Behavior-neutral; existing render tests are the
   guard.
3. **Help from the keymap.** `help_rows()` (tables + the static floor rows),
   `HelpPopup.rows`, rewrite `ui/help.rs`, delete `ALL_KEYBINDINGS`/`HelpEntry`,
   add the help snapshot, update the footer hints (ui/chrome.rs:83-90) and
   replace [[tui-modal.md]]'s binding tables with a link here.
4. **Parity follow-ups** (separate issues; each is one table row + one variant +
   one apply arm): `i` assign-to-me (viewer id is already fetched; action is one
   `IssueEdit::Assignee` through the service), `s`/`p`/`a` from Detail (the view
   stack is delivered, so this is structural: the binding pushes a `PopupView`
   built from the detail's own issue, and confirm/cancel pop back to the Detail
   beneath), first `g` chords and further `o` chords as
   inbox/my-issues/workspaces land (ENG-41 = `o w`).

## Risks and flagged issues

- **Muscle-memory breaks.** `g` → `g g`, `o` → `o b`, `n` → `c`, `r` → `ctrl+r`,
  `?` → `ctrl+/`, and the removal of `S` cycle-sort are deliberate breaking
  changes, acceptable in 0.1.x per [[posture.md]]; the help overlay and footer
  hints are updated in the same phases.
