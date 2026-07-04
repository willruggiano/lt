//! The keymap: a normalized [`Key`], an [`Action`] enum, and static
//! `(Binding, Action)` tables resolved through layered [`KeyContext`]s.
//! Phase 1 of `docs/design/keybinds.md` -- the List/Detail/Popup contexts and
//! the shared GLOBAL navigation layer. Search/Help/`NewIssue*`/`CommentInput`
//! stay on their existing handlers until phase 2.

mod action;
mod key;

pub(crate) use action::Action;
use crossterm::event::KeyCode;
pub(crate) use key::Key;

/// Where a key is resolved: the focused view's own context, then GLOBAL.
/// Later phases add the text contexts (`Search`, `Help`, `NewIssuePicker`,
/// `NewIssueText`, `CommentInput`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KeyContext {
    List,
    Detail,
    Popup,
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

impl KeyContext {
    fn table(self) -> &'static [(Binding, Action)] {
        match self {
            KeyContext::List => LIST,
            KeyContext::Detail => DETAIL,
            KeyContext::Popup => POPUP,
        }
    }

    /// This context's effective resolution layers: its own table, then
    /// GLOBAL. Every phase-1 context is non-text, so all pick up GLOBAL;
    /// text contexts (phase 2) will skip it.
    fn layers(self) -> [&'static [(Binding, Action)]; 2] {
        [self.table(), GLOBAL]
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
/// the `KeyContext`-facing entry point.
fn resolve_layers(layers: [&[(Binding, Action)]; 2], pending: Option<Key>, key: Key) -> Resolved {
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
/// GLOBAL), given the pending chord prefix `App::dispatch_key` took once at
/// entry.
pub(crate) fn resolve(ctx: KeyContext, pending: Option<Key>, key: Key) -> Resolved {
    resolve_layers(ctx.layers(), pending, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_CONTEXTS: [KeyContext; 3] = [KeyContext::List, KeyContext::Detail, KeyContext::Popup];

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
            resolve_layers([CONTEXT_LAYER, GLOBAL_STAND_IN], None, Key::char('d')),
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
    fn no_table_binds_q_or_esc() {
        for ctx in ALL_CONTEXTS {
            for key in context_keys(ctx) {
                assert!(
                    !matches!(key.code, KeyCode::Char('q') | KeyCode::Esc),
                    "{ctx:?}: table binds {key}"
                );
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
