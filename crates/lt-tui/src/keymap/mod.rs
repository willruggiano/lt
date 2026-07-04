//! The keymap: a normalized [`Key`], an [`Action`] enum, and static
//! `(Binding, Action)` tables resolved through layered [`KeyContext`]s.
//! Phase 2 of `docs/design/keybinds.md` adds the text/form contexts
//! (`Search`, `Help`, `NewIssuePicker`, `NewIssueText`, `CommentInput`)
//! alongside phase 1's List/Detail/Popup set and the shared GLOBAL
//! navigation layer.

mod action;
mod key;

pub(crate) use action::Action;
use crossterm::event::KeyCode;
pub(crate) use key::Key;

/// Where a key is resolved: the focused view's own context, then GLOBAL --
/// skipped by the text contexts (`is_text`), which forward instead of
/// cascading (`docs/design/keybinds.md`, "Contexts and layering").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KeyContext {
    // Full keymap contexts: their own table, then GLOBAL.
    List,
    Detail,
    Popup,
    NewIssuePicker,
    // Text contexts: their own table only; an unbound key forwards to the
    // context's editor widget instead of cascading.
    Search,
    Help,
    NewIssueText,
    CommentInput,
}

impl KeyContext {
    /// Text contexts skip GLOBAL, never start chords, and forward an
    /// unbound key to their editor widget instead of cascading (except
    /// `esc`, which always passes to the floor -- handled by the dispatch
    /// seam, not here).
    pub(crate) fn is_text(self) -> bool {
        matches!(
            self,
            KeyContext::Search
                | KeyContext::Help
                | KeyContext::NewIssueText
                | KeyContext::CommentInput
        )
    }
}

/// A single- or two-key binding. Linear's chords are exactly two keys;
/// `Chord(Key, Key)` over `Vec<Key>` makes deeper nesting unrepresentable
/// rather than untested.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Binding {
    Single(Key),
    Chord(Key, Key),
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

impl KeyContext {
    fn table(self) -> &'static [(Binding, Action)] {
        match self {
            KeyContext::List => LIST,
            KeyContext::Detail => DETAIL,
            KeyContext::Popup => POPUP,
            KeyContext::NewIssuePicker => NEW_ISSUE_PICKER,
            KeyContext::Search => SEARCH,
            KeyContext::Help => HELP,
            KeyContext::NewIssueText => NEW_ISSUE_TEXT,
            KeyContext::CommentInput => COMMENT_INPUT,
        }
    }

    /// This context's effective resolution layers: its own table, then any
    /// shared layers, in precedence order. `NewIssuePicker` additionally
    /// layers `FORM_NAV` (`NewIssueText`'s own table already *is* `FORM_NAV`,
    /// so it needs no second copy). Every non-text context also picks up
    /// GLOBAL; a text context skips it so a navigation letter (`j`, `g`,
    /// ...) never steals a character from the editor it forwards to.
    fn layers(self) -> Vec<&'static [(Binding, Action)]> {
        let mut layers = vec![self.table()];
        if matches!(self, KeyContext::NewIssuePicker) {
            layers.push(FORM_NAV);
        }
        if !self.is_text() {
            layers.push(GLOBAL);
        }
        layers
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
    resolve_layers(&ctx.layers(), pending, key)
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
        ctx.layers().into_iter().flatten().copied().collect()
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
        for (context, layer) in [
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
        ] {
            for (binding, action) in layer {
                let binding_str = match binding {
                    Binding::Single(k) => k.to_string(),
                    Binding::Chord(a, b) => format!("{a} {b}"),
                };
                lines.push(format!(
                    "{context:<6} {binding_str:<10} -> {}",
                    action.label()
                ));
            }
        }
        insta::assert_snapshot!(lines.join("\n"));
    }
}
