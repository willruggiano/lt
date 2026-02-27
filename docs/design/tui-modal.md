# TUI Modal Redesign (bd-qei)

## Overview

This document describes a vim-style modal design for the `lt` TUI.
The current TUI is already partially modal (it has `Mode::List`,
`Mode::Detail`, `Mode::Popup`, `Mode::NewIssue`, `Mode::Help`, and
`Mode::Search`), but there is no explicit concept of a "normal" vs.
"insert/filter" mode presented to the user.  The goal is to make the
modal contract explicit and consistent, and to add a clearly labelled
filter mode entered via `/`.

---

## Mode Taxonomy

```
+---------------------------------------------------------------------+
|  NORMAL mode  (default)                                             |
|    Navigation, selection, and action triggers.                      |
|    No text is typed "into" the list itself.                         |
|                                                                     |
|    /       --> FILTER mode                                          |
|    <space> --> DETAIL mode (read-only overlay)                      |
|    s/p/a   --> POPUP mode  (inline field editor)                    |
|    n       --> FORM mode   (new-issue modal)                        |
|    ?       --> HELP mode   (searchable keybinding list)             |
+---------------------------------------------------------------------+
       |           |            |           |            |
       v           v            v           v            v
  FILTER mode  DETAIL mode  POPUP mode   FORM mode   HELP mode
  (/ query)    (<space>)    (s/p/a)      (n)         (?)
       |           |            |           |            |
  Esc/Enter   Esc/q        Esc/Enter    Esc/Ctrl+   Esc/q
       |           |            |       Enter         |
       +------->   NORMAL   <---+--------+------------+
```

### Normal Mode

The only mode in which no text input is accepted from printable keys
(except single-key commands).  Bindings:

| Key        | Action                                    |
|------------|-------------------------------------------|
| j / Down   | move selection down                       |
| k / Up     | move selection up                         |
| g          | go to top                                 |
| G          | go to bottom                              |
| Ctrl+d     | half page down                            |
| Ctrl+u     | half page up                              |
| PgDn       | page down                                 |
| PgUp       | page up                                   |
| Ctrl+n     | next page (pagination)                    |
| Ctrl+p     | previous page (pagination)                |
| Space      | open detail pane                          |
| /          | enter filter mode                         |
| s          | set state (inline popup)                  |
| p          | set priority (inline popup)               |
| a          | set assignee (inline popup)               |
| n          | new issue form                            |
| o          | open issue in browser                     |
| r          | refresh from cache                        |
| S          | cycle sort field                          |
| d          | toggle sort direction                     |
| ?          | help popup                                |
| q / Esc    | quit (Esc: reset list to first page)      |

### Filter Mode

Entered by pressing `/` from Normal mode.  The status bar changes to
show the current query with a block cursor (already implemented via
`SearchOverlay` and `Mode::Search`).

Bindings while in Filter mode:

| Key        | Action                                    |
|------------|-------------------------------------------|
| (text)     | append to query; debounced FTS search     |
| Backspace  | delete char before cursor                 |
| Ctrl+w     | delete word before cursor                 |
| Ctrl+u     | clear query                               |
| Left/Right | move cursor in query                      |
| j / Down   | move selection in result list             |
| k / Up     | move selection in result list             |
| Enter      | confirm: results become the issue list    |
| Esc        | cancel: return to full list               |

This is already implemented as `Mode::Search`.  The rename from
"Search" to "Filter" is a UX label change only -- the underlying type
can stay `Mode::Search` for now, or be renamed in a follow-up.

### Detail Mode

A read-only overlay split showing issue body and comments.

| Key        | Action                                    |
|------------|-------------------------------------------|
| j / Down   | scroll down                               |
| k / Up     | scroll up                                 |
| o          | open in browser                           |
| Esc / q    | close detail, return to Normal            |

### Popup Mode

Inline field picker (state / priority / assignee).  Already
implemented.  No text input -- pure navigation.

| Key        | Action                                    |
|------------|-------------------------------------------|
| j / Down   | move selection down                       |
| k / Up     | move selection up                         |
| Enter      | confirm selection                         |
| Esc        | cancel, return to Normal                  |

### Form Mode (New Issue)

Full-screen modal form.  Tab / Shift-Tab navigate fields.  Each text
field uses the `TextInput` widget with vim line-editing bindings.

| Key           | Action                                 |
|---------------|----------------------------------------|
| Tab           | next field                             |
| Shift-Tab     | previous field                         |
| Ctrl+Enter    | submit form                            |
| j / k         | move within picker fields              |
| Esc           | cancel, return to Normal               |

### Help Mode

Searchable keybinding list popup.  Text typed goes to the search
bar.  Already implemented.

---

## Status Bar / Mode Indicator

The bottom status bar should show the current mode name on the
right-hand side so the user always knows which mode is active.

```
Proposed layout (Normal):
  [left]  q quit  / filter  j/k nav  <space> detail ...
  [right] NORMAL   synced 2 min ago  [1]

Proposed layout (Filter):
  [left]  / <query with cursor>
  [right] FILTER   synced 2 min ago  [1]
```

The mode indicator should use bold or reversed styling so it stands
out.

---

## Implementation Notes

### What Already Exists

The TUI already implements all six modes.  The main gaps relative to
this design are:

1. No explicit mode indicator in the status bar.
2. The "filter" mode is labelled "Search" internally and in user-
   visible strings.  Renaming it to "filter" (triggered by `/`) aligns
   with the description in the bead.
3. `Esc` in Normal mode currently calls `do_fetch(true)` (resets the
   list).  Consider whether `Esc` should instead be a no-op when no
   filter is active, or confirm as-is.

### Proposed Changes (non-breaking)

1. Add a `mode_label(mode: &Mode) -> &'static str` helper that returns
   `"NORMAL"`, `"FILTER"`, `"DETAIL"`, `"POPUP"`, `"FORM"`, `"HELP"`.

2. Update `render_footer` to include the mode label on the right side,
   between the sync status and the page number.

3. Optionally rename `Mode::Search` to `Mode::Filter` (search-and-
   replace; requires updating all match arms).

4. Update the help entry for `/` from `"filter by title"` to
   `"enter filter mode"` (or similar).

No key-handler logic needs to change; all mode transitions already
work as described above.

---

## Open Questions

1. Should `Esc` in Normal mode be a no-op when already on page 1 with
   no active filter, or should it continue to reset the list?

2. Should the mode indicator always be visible (preferred), or only
   when not in Normal mode?

3. Is a "VISUAL" or "MULTI-SELECT" mode desired in the future?  (Not
   in scope for this bead, but worth noting as a possible extension.)

4. Should filter mode use the FTS full-text index only, or also allow
   filtering by field (eg. `state:In Progress`)?  The current
   implementation is FTS-only.

---

## Summary

The current TUI is already close to a vim-style modal design.  The
primary deliverable of this bead is the formal specification above.
Implementation work is small: add a mode label to the status bar and
optionally rename `Mode::Search` to `Mode::Filter`.  Larger features
(multi-select, field-qualified filter syntax) are future work.
