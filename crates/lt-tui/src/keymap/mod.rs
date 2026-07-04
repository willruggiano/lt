//! The keymap: a normalized [`Key`], an [`Action`] enum, and static
//! `(Binding, Action)` tables resolved through layered [`KeyContext`]s.
//! Phase 2 of `docs/design/keybinds.md` adds the text/form contexts
//! (`Search`, `Help`, `NewIssuePicker`, `NewIssueText`, `CommentInput`)
//! alongside phase 1's List/Detail/Popup set and the shared GLOBAL
//! navigation layer.

mod action;
mod key;

use std::fmt;

pub(crate) use action::Action;
use crossterm::event::KeyCode;
pub(crate) use key::Key;

/// Where a key is resolved: the focused view's own context, then GLOBAL --
/// skipped by the text contexts, which forward instead of cascading
/// (`docs/design/keybinds.md`, "Contexts and layering").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KeyContext {
    // Full keymap contexts: their own table, then GLOBAL.
    List,
    Detail,
    Popup,
    NewIssuePicker,
    // Text contexts: their own table only (`layers()` never adds GLOBAL);
    // an unbound key forwards to the context's editor widget instead of
    // cascading (except `esc`, which always passes to the floor -- handled
    // by the dispatch seam, not here).
    Search,
    Help,
    NewIssueText,
    CommentInput,
}

/// A single- or two-key binding. Linear's chords are exactly two keys;
/// `Chord(Key, Key)` over `Vec<Key>` makes deeper nesting unrepresentable
/// rather than untested.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Binding {
    Single(Key),
    Chord(Key, Key),
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Binding::Single(k) => write!(f, "{k}"),
            Binding::Chord(a, b) => write!(f, "{a} {b}"),
        }
    }
}

/// The outcome of resolving a key against a context.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Resolved {
    Act(Action),
    Pending(Key),
    Unbound(Key),
}

// ---------------------------------------------------------------------------
// Tables (docs/design/keybinds.md, "Default binding tables")
// ---------------------------------------------------------------------------

/// The shared navigation vocabulary, layered under every phase-1 context.
static GLOBAL: &[(Binding, Action)] = &[
    (Binding::Single(Key::char('j')), Action::MoveDown),
    (Binding::Single(Key::plain(KeyCode::Down)), Action::MoveDown),
    (Binding::Single(Key::char('k')), Action::MoveUp),
    (Binding::Single(Key::plain(KeyCode::Up)), Action::MoveUp),
    (
        Binding::Chord(Key::char('g'), Key::char('g')),
        Action::MoveTop,
    ),
    (Binding::Single(Key::char('G')), Action::MoveBottom),
    (Binding::Single(Key::ctrl('d')), Action::HalfPageDown),
    (Binding::Single(Key::ctrl('u')), Action::HalfPageUp),
    (
        Binding::Single(Key::plain(KeyCode::PageDown)),
        Action::PageDown,
    ),
    (Binding::Single(Key::plain(KeyCode::PageUp)), Action::PageUp),
];

static LIST: &[(Binding, Action)] = &[
    (
        Binding::Single(Key::plain(KeyCode::Enter)),
        Action::OpenDetail,
    ),
    (Binding::Single(Key::char(' ')), Action::OpenDetail),
    (Binding::Single(Key::char('/')), Action::OpenSearch),
    (Binding::Single(Key::ctrl('/')), Action::OpenHelp),
    // Legacy terminals send Ctrl+/ as 0x1F, which crossterm decodes as
    // ctrl+'7'; kitty-enhanced terminals deliver a true ctrl+/. Both bound.
    (Binding::Single(Key::ctrl('7')), Action::OpenHelp),
    (Binding::Single(Key::char('c')), Action::CreateIssue),
    (Binding::Single(Key::char('s')), Action::SetStatus),
    (Binding::Single(Key::char('p')), Action::SetPriority),
    (Binding::Single(Key::char('a')), Action::SetAssignee),
    (Binding::Single(Key::ctrl('r')), Action::Refresh),
    (Binding::Single(Key::char('d')), Action::ToggleSortDirection),
    (
        Binding::Chord(Key::char('o'), Key::char('b')),
        Action::OpenInBrowser,
    ),
    (Binding::Single(Key::ctrl('n')), Action::NextPage),
    (Binding::Single(Key::ctrl('p')), Action::PrevPage),
    (Binding::Single(Key::char('L')), Action::Login),
];

static DETAIL: &[(Binding, Action)] = &[
    (Binding::Single(Key::char('c')), Action::Comment),
    (
        Binding::Chord(Key::char('o'), Key::char('b')),
        Action::OpenInBrowser,
    ),
];

static POPUP: &[(Binding, Action)] =
    &[(Binding::Single(Key::plain(KeyCode::Enter)), Action::Confirm)];

/// Shared by `NewIssuePicker`/`NewIssueText`: the submit chord plus
/// Tab/Shift+Tab field navigation (`docs/design/keybinds.md`, "New issue --
/// picker fields"/"-- text fields"). `NewIssueText`'s own table *is* this
/// layer (everything else forwards to the focused field's editor);
/// `NewIssuePicker` layers it alongside its own Confirm/PickMe rows.
static FORM_NAV: &[(Binding, Action)] = &[
    (
        Binding::Single(Key::ctrl_code(KeyCode::Enter)),
        Action::Submit,
    ),
    (Binding::Single(Key::alt(KeyCode::Enter)), Action::Submit),
    (Binding::Single(Key::plain(KeyCode::Tab)), Action::NextField),
    (Binding::Single(Key::shift_tab()), Action::PrevField),
];

/// New-issue modal, picker fields (Team/Priority/State/Assignee): `FORM_NAV`
/// (layered on in `KeyContext::layers`) plus GLOBAL's `j`/`k`/`down`/`up`,
/// which move the focused picker's selection (`View::scroll`'s `NewIssue`
/// override); `enter` advances like `Tab` (leaving Team swaps the watched
/// scope).
static NEW_ISSUE_PICKER: &[(Binding, Action)] = &[
    (Binding::Single(Key::plain(KeyCode::Enter)), Action::Confirm),
    (Binding::Single(Key::char('m')), Action::PickMe),
];

/// New-issue modal, text fields (Title/Description): everything but
/// `FORM_NAV`'s rows forwards to the focused field's editor (`enter` inserts
/// a newline in Description).
static NEW_ISSUE_TEXT: &[(Binding, Action)] = FORM_NAV;

/// The detail pane's comment box: the one context that binds `esc` --
/// narrower than the floor's pop (cancels the draft, keeps the pane open).
static COMMENT_INPUT: &[(Binding, Action)] = &[
    (
        Binding::Single(Key::ctrl_code(KeyCode::Enter)),
        Action::Submit,
    ),
    (Binding::Single(Key::alt(KeyCode::Enter)), Action::Submit),
    (Binding::Single(Key::plain(KeyCode::Esc)), Action::Back),
];

/// The FTS search overlay. Plain `j`/`k` are deliberately unbound (typeable
/// filter text); `tab`/`shift+tab` drive stem-key completion and must not
/// reach the query bar.
static SEARCH: &[(Binding, Action)] = &[
    (Binding::Single(Key::plain(KeyCode::Enter)), Action::Confirm),
    (Binding::Single(Key::ctrl('c')), Action::ClearQuery),
    (Binding::Single(Key::plain(KeyCode::Down)), Action::MoveDown),
    (Binding::Single(Key::plain(KeyCode::Up)), Action::MoveUp),
    (Binding::Single(Key::ctrl('n')), Action::CompleteNext),
    (Binding::Single(Key::ctrl('p')), Action::CompletePrev),
    (Binding::Single(Key::ctrl('y')), Action::CompleteAccept),
    (
        Binding::Single(Key::plain(KeyCode::Tab)),
        Action::CompleteForward,
    ),
    (Binding::Single(Key::shift_tab()), Action::CompleteBackward),
];

/// The keyboard-shortcuts help popup. `j`/`k` stay untypeable in the filter
/// bar -- an existing limitation, carried forward deliberately.
static HELP: &[(Binding, Action)] = &[
    (Binding::Single(Key::plain(KeyCode::Down)), Action::MoveDown),
    (Binding::Single(Key::char('j')), Action::MoveDown),
    (Binding::Single(Key::plain(KeyCode::Up)), Action::MoveUp),
    (Binding::Single(Key::char('k')), Action::MoveUp),
];

/// Every table, named, in source declaration order: GLOBAL first, then each
/// context table. Pins the internal tables (including the shared `form_nav`
/// layer) for `binding_snapshot`; the help overlay reads `HELP_CONTEXTS`
/// instead, which names real, user-facing contexts.
#[cfg(test)]
static TABLES: &[(&str, &[(Binding, Action)])] = &[
    ("global", GLOBAL),
    ("list", LIST),
    ("detail", DETAIL),
    ("popup", POPUP),
    ("form_nav", FORM_NAV),
    ("new_issue_picker", NEW_ISSUE_PICKER),
    ("new_issue_text", NEW_ISSUE_TEXT),
    ("comment_input", COMMENT_INPUT),
    ("search", SEARCH),
    ("help", HELP),
];

/// Every context's effective resolution layers, precomputed at compile time
/// (`docs/design/keybinds.md`, "Contexts and layering"): its own table, then
/// any shared layers, in precedence order. `NewIssuePicker` additionally
/// layers `FORM_NAV` (`NewIssueText`'s own table already *is* `FORM_NAV`, so
/// it needs no second copy). Every non-text context also picks up GLOBAL; a
/// text context skips it so a navigation letter (`j`, `g`, ...) never steals
/// a character from the editor it forwards to. `static`, not built per
/// `resolve()` call: a keypress is on the hot path and every layer set is
/// known at compile time.
static LIST_LAYERS: &[&[(Binding, Action)]] = &[LIST, GLOBAL];
static DETAIL_LAYERS: &[&[(Binding, Action)]] = &[DETAIL, GLOBAL];
static POPUP_LAYERS: &[&[(Binding, Action)]] = &[POPUP, GLOBAL];
static NEW_ISSUE_PICKER_LAYERS: &[&[(Binding, Action)]] = &[NEW_ISSUE_PICKER, FORM_NAV, GLOBAL];
static SEARCH_LAYERS: &[&[(Binding, Action)]] = &[SEARCH];
static HELP_LAYERS: &[&[(Binding, Action)]] = &[HELP];
static NEW_ISSUE_TEXT_LAYERS: &[&[(Binding, Action)]] = &[NEW_ISSUE_TEXT];
static COMMENT_INPUT_LAYERS: &[&[(Binding, Action)]] = &[COMMENT_INPUT];

impl KeyContext {
    /// This context's effective resolution layers -- see `LIST_LAYERS` et al.
    fn layers(self) -> &'static [&'static [(Binding, Action)]] {
        match self {
            KeyContext::List => LIST_LAYERS,
            KeyContext::Detail => DETAIL_LAYERS,
            KeyContext::Popup => POPUP_LAYERS,
            KeyContext::NewIssuePicker => NEW_ISSUE_PICKER_LAYERS,
            KeyContext::Search => SEARCH_LAYERS,
            KeyContext::Help => HELP_LAYERS,
            KeyContext::NewIssueText => NEW_ISSUE_TEXT_LAYERS,
            KeyContext::CommentInput => COMMENT_INPUT_LAYERS,
        }
    }
}

// ---------------------------------------------------------------------------
// Resolution (docs/design/keybinds.md, "Resolution and chords")
// ---------------------------------------------------------------------------

fn lookup_chord(table: &[(Binding, Action)], prefix: Key, key: Key) -> Option<Action> {
    table.iter().find_map(|(binding, action)| match binding {
        Binding::Chord(a, b) if *a == prefix && *b == key => Some(*action),
        _ => None,
    })
}

fn is_chord_prefix(table: &[(Binding, Action)], key: Key) -> bool {
    table
        .iter()
        .any(|(binding, _)| matches!(binding, Binding::Chord(a, _) if *a == key))
}

fn lookup_single(table: &[(Binding, Action)], key: Key) -> Option<Action> {
    table.iter().find_map(|(binding, action)| match binding {
        Binding::Single(k) if *k == key => Some(*action),
        _ => None,
    })
}

/// The resolution algorithm, parameterized over the layer set so tests can
/// exercise layer precedence directly with synthetic layers; [`resolve`] is
/// the `KeyContext`-facing entry point. A slice rather than a fixed array:
/// text contexts have one layer (their own table), non-text contexts two
/// (table, then GLOBAL).
fn resolve_layers(layers: &[&[(Binding, Action)]], pending: Option<Key>, key: Key) -> Resolved {
    if let Some(prefix) = pending {
        for layer in layers {
            if let Some(action) = lookup_chord(layer, prefix, key) {
                return Resolved::Act(action);
            }
        }
        // Chord miss: drop the prefix and resolve `key` fresh (atuin
        // behavior) rather than treating it as unbound.
        return resolve_layers(layers, None, key);
    }
    for layer in layers {
        if is_chord_prefix(layer, key) {
            return Resolved::Pending(key);
        }
    }
    for layer in layers {
        if let Some(action) = lookup_single(layer, key) {
            return Resolved::Act(action);
        }
    }
    Resolved::Unbound(key)
}

/// Resolve `key` against `ctx`'s effective layers (its own table, then
/// GLOBAL unless `ctx` is a text context), given the pending chord prefix
/// `App::dispatch_key` took once at entry.
pub(crate) fn resolve(ctx: KeyContext, pending: Option<Key>, key: Key) -> Resolved {
    resolve_layers(ctx.layers(), pending, key)
}

// ---------------------------------------------------------------------------
// Help overlay (docs/design/keybinds.md, "Help overlay from the keymap")
// ---------------------------------------------------------------------------

/// One help-panel row: one or more equivalent bindings for the same
/// (context, action) -- e.g. `j`/`down`, both `MoveDown` in `global` -- grouped
/// so the panel shows them on one line. Rows are immutable once `help_rows()`
/// returns, so `binding_form`/`haystack` are precomputed there rather than
/// rebuilt every render frame or filter keystroke.
pub(crate) struct HelpRow {
    pub(crate) bindings: Vec<Binding>,
    pub(crate) label: &'static str,
    pub(crate) context: &'static str,
    /// The bindings' `Display` forms joined with `" / "`, e.g. `"j / down"`,
    /// `"ctrl+enter / alt+enter"` -- the rendered key column.
    pub(crate) binding_form: String,
    /// Lowercase `"{binding_form} {label} {context}"`, matched against the
    /// help popup's filter query.
    pub(crate) haystack: String,
}

impl HelpRow {
    /// A row with its grouped `bindings` still open to appending (`help_rows`
    /// merges consecutive same-action rows); `binding_form`/`haystack` are
    /// filled in by `finalize` once grouping is done.
    fn open(bindings: Vec<Binding>, label: &'static str, context: &'static str) -> Self {
        Self {
            bindings,
            label,
            context,
            binding_form: String::new(),
            haystack: String::new(),
        }
    }

    /// Fill `binding_form`/`haystack` from the now-final `bindings`. Called
    /// once per row after `help_rows` finishes grouping.
    fn finalize(&mut self) {
        self.binding_form = self
            .bindings
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" / ");
        self.haystack = format!(
            "{} {} {}",
            self.binding_form.to_lowercase(),
            self.label.to_lowercase(),
            self.context.to_lowercase()
        );
    }
}

/// The Esc/q floor's dispatch behavior (`docs/design/keybinds.md`, "Contexts
/// and layering"): not table bindings, so `help_rows` appends them by hand.
/// `esc`/`q` close an overlay; at the base list, `esc` refreshes (twice within
/// 500ms resets sort/filter/search, `handle_list_esc`) and `q` quits.
fn floor_rows() -> Vec<HelpRow> {
    vec![
        HelpRow::open(
            vec![Binding::Single(Key::plain(KeyCode::Esc))],
            "close, back to the view beneath",
            "overlay",
        ),
        // Unlike esc, q is not floor-owned in text contexts: the search/help
        // filter bars type it, so its "close" meaning only applies above the
        // base outside a text context.
        HelpRow::open(
            vec![Binding::Single(Key::char('q'))],
            "close, back to the view beneath (typed in text inputs)",
            "overlay",
        ),
        HelpRow::open(
            vec![Binding::Single(Key::plain(KeyCode::Esc))],
            "refresh (press twice to reset sort/filter/search)",
            "list",
        ),
        HelpRow::open(vec![Binding::Single(Key::char('q'))], "quit", "list"),
    ]
}

/// A `(Binding, Action)` table, aliased so `HELP_CONTEXTS`'s type stays
/// simple (`clippy::type_complexity`).
type Table = &'static [(Binding, Action)];

/// The help overlay's real, user-facing contexts -- distinct from `TABLES`,
/// which names the internal tables (including the shared `form_nav` layer)
/// for `binding_snapshot`. The two new-issue contexts (`NewIssuePicker`/
/// `NewIssueText`) collapse into one displayed context, "new issue":
/// `NewIssueText`'s own table *is* `FORM_NAV`, so `FORM_NAV` plus
/// `NEW_ISSUE_PICKER`'s own rows is their union, with no duplicates.
static HELP_CONTEXTS: &[(&str, &[Table])] = &[
    ("global", &[GLOBAL]),
    ("list", &[LIST]),
    ("detail", &[DETAIL]),
    ("popup", &[POPUP]),
    ("new issue", &[FORM_NAV, NEW_ISSUE_PICKER]),
    ("comment", &[COMMENT_INPUT]),
    ("search", &[SEARCH]),
    ("help", &[HELP]),
];

/// Accumulates `help_rows()`'s output, grouping consecutive rows for the
/// same `(context, action)` into one [`HelpRow`] as they're pushed.
#[derive(Default)]
struct HelpRowBuilder {
    rows: Vec<HelpRow>,
    last: Option<(&'static str, Action)>,
}

impl HelpRowBuilder {
    fn push(&mut self, context: &'static str, binding: Binding, action: Action) {
        if self.last == Some((context, action)) {
            if let Some(row) = self.rows.last_mut() {
                row.bindings.push(binding);
            }
            return;
        }
        self.rows
            .push(HelpRow::open(vec![binding], action.label(), context));
        self.last = Some((context, action));
    }
}

/// `HELP_CONTEXTS` in declaration order, grouping consecutive rows for the
/// same `(context, action)` into one [`HelpRow`], plus the floor's static
/// rows, each finalized (`HelpRow::finalize`) once its `bindings` group is
/// complete.
pub(crate) fn help_rows() -> Vec<HelpRow> {
    let mut builder = HelpRowBuilder::default();
    for &(context, tables) in HELP_CONTEXTS {
        for &(binding, action) in tables.iter().flat_map(|table| table.iter()) {
            builder.push(context, binding, action);
        }
    }
    let mut rows = builder.rows;
    rows.extend(floor_rows());
    for row in &mut rows {
        row.finalize();
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_CONTEXTS: [KeyContext; 8] = [
        KeyContext::List,
        KeyContext::Detail,
        KeyContext::Popup,
        KeyContext::NewIssuePicker,
        KeyContext::Search,
        KeyContext::Help,
        KeyContext::NewIssueText,
        KeyContext::CommentInput,
    ];

    fn binding_keys(binding: &Binding) -> Vec<Key> {
        match binding {
            Binding::Single(k) => vec![*k],
            Binding::Chord(a, b) => vec![*a, *b],
        }
    }

    /// Every `(Binding, Action)` row across `ctx`'s effective layers
    /// (its own table, then GLOBAL), flattened so the invariant tests below
    /// stay a single loop level instead of nesting through layers/rows.
    fn context_bindings(ctx: KeyContext) -> Vec<(Binding, Action)> {
        ctx.layers()
            .iter()
            .flat_map(|layer| layer.iter())
            .copied()
            .collect()
    }

    /// Every key named by any binding across `ctx`'s effective layers.
    fn context_keys(ctx: KeyContext) -> Vec<Key> {
        context_bindings(ctx)
            .iter()
            .flat_map(|(binding, _)| binding_keys(binding))
            .collect()
    }

    // -- Resolution units (Testing strategy, item 2) -------------------

    #[test]
    fn chord_hit_g_g_selects_top() {
        let pending = match resolve(KeyContext::List, None, Key::char('g')) {
            Resolved::Pending(k) => k,
            other => unreachable!("expected Pending, got {other:?}"),
        };
        assert_eq!(
            resolve(KeyContext::List, Some(pending), Key::char('g')),
            Resolved::Act(Action::MoveTop)
        );
    }

    #[test]
    fn chord_miss_g_j_falls_through_to_move_down() {
        assert_eq!(
            resolve(KeyContext::List, Some(Key::char('g')), Key::char('j')),
            Resolved::Act(Action::MoveDown)
        );
    }

    #[test]
    fn layer_precedence_context_wins_over_global() {
        // No production table currently overrides a GLOBAL key (by design:
        // the invariant test below forbids ambiguity), so this exercises the
        // resolution mechanism directly with synthetic layers standing in
        // for "context table" and "GLOBAL".
        const CONTEXT_LAYER: &[(Binding, Action)] =
            &[(Binding::Single(Key::char('d')), Action::ToggleSortDirection)];
        const GLOBAL_STAND_IN: &[(Binding, Action)] =
            &[(Binding::Single(Key::char('d')), Action::MoveDown)];

        assert_eq!(
            resolve_layers(&[CONTEXT_LAYER, GLOBAL_STAND_IN], None, Key::char('d')),
            Resolved::Act(Action::ToggleSortDirection)
        );
    }

    // -- Invariants (Testing strategy, item 3) --------------------------

    #[test]
    fn no_context_duplicates_a_binding() {
        for ctx in ALL_CONTEXTS {
            let bindings: Vec<Binding> =
                context_bindings(ctx).into_iter().map(|(b, _)| b).collect();
            for binding in &bindings {
                let occurrences = bindings.iter().filter(|b| *b == binding).count();
                assert!(occurrences <= 1, "{ctx:?}: duplicate binding {binding:?}");
            }
        }
    }

    #[test]
    fn no_key_is_both_single_bound_and_a_chord_prefix() {
        for ctx in ALL_CONTEXTS {
            let bindings = context_bindings(ctx);
            let singles: Vec<Key> = bindings
                .iter()
                .filter_map(|(b, _)| match b {
                    Binding::Single(k) => Some(*k),
                    Binding::Chord(..) => None,
                })
                .collect();
            let prefixes = bindings.iter().filter_map(|(b, _)| match b {
                Binding::Chord(a, _) => Some(*a),
                Binding::Single(_) => None,
            });
            for prefix in prefixes {
                assert!(
                    !singles.contains(&prefix),
                    "{ctx:?}: {prefix} is both single-bound and a chord prefix"
                );
            }
        }
    }

    #[test]
    fn every_table_binding_round_trips_through_display_and_from_str() {
        for ctx in ALL_CONTEXTS {
            for key in context_keys(ctx) {
                assert_eq!(key.to_string().parse::<Key>(), Ok(key));
            }
        }
    }

    #[test]
    fn no_table_binds_q_and_only_comment_input_binds_esc() {
        for ctx in ALL_CONTEXTS {
            for key in context_keys(ctx) {
                assert!(
                    !matches!(key.code, KeyCode::Char('q')),
                    "{ctx:?}: table binds {key}"
                );
                if key.code == KeyCode::Esc {
                    assert_eq!(
                        ctx,
                        KeyContext::CommentInput,
                        "{ctx:?}: table binds esc (Back/quit are the floor's, except CommentInput's cancel)"
                    );
                }
            }
        }
    }

    // -- Binding snapshot (Testing strategy, item 4) --------------------

    #[test]
    fn binding_snapshot() {
        let mut lines = Vec::new();
        for &(context, table) in TABLES {
            for (binding, action) in table {
                let binding_str = binding.to_string();
                lines.push(format!(
                    "{context:<6} {binding_str:<10} -> {}",
                    action.label()
                ));
            }
        }
        insta::assert_snapshot!(lines.join("\n"));
    }

    /// The second snapshot (Testing strategy, item 4): `help_rows()`'s
    /// grouped, filterable output -- the exact data the help popup renders.
    #[test]
    fn help_rows_snapshot() {
        let lines: Vec<String> = help_rows()
            .iter()
            .map(|row| {
                format!(
                    "{:<8} {:<24} -> {}",
                    row.context, row.binding_form, row.label
                )
            })
            .collect();
        insta::assert_snapshot!(lines.join("\n"));
    }
}
