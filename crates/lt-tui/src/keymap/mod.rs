//! The keymap: a normalized [`Key`], an [`Action`] enum, [`Binding`] tables,
//! and the machinery that resolves a key through a view's declared layers
//! ([`resolve`]) into an [`Action`], plus help-row generation from those same
//! tables ([`help_rows`]). [`GLOBAL`] is the shared navigation vocabulary
//! layered under most views. Each view declares its own binding tables and a
//! [`crate::Keymap`] naming its layers, apply function, and unbound-key
//! policy; this module owns none of that -- only the vocabulary and the
//! resolution/help-row algorithms.

mod action;
mod key;

use std::fmt;

pub(crate) use action::Action;
use crossterm::event::KeyCode;
pub(crate) use key::Key;

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

/// A `(Binding, Action)` table: one view's declared bindings, or the shared
/// `GLOBAL` layer.
pub(crate) type Table = &'static [(Binding, Action)];

/// A view's effective resolution layers, in precedence order: its own table
/// first, then any shared layers.
pub(crate) type Layers = &'static [Table];

/// The outcome of resolving a key against a set of layers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Resolved {
    Act(Action),
    Pending(Key),
    Unbound(Key),
}

// ---------------------------------------------------------------------------
// Shared vocabulary (docs/design/keybinds.md, "Default binding tables")
// ---------------------------------------------------------------------------

/// The shared navigation vocabulary, layered under most views' own tables. A
/// view whose own table forwards to a text editor instead of cascading skips
/// this layer, so a navigation letter (`j`, `g`, ...) never steals a
/// character from the editor.
pub(crate) static GLOBAL: &[(Binding, Action)] = &[
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

/// Resolve `key` against `layers`, given the pending chord prefix
/// `App::dispatch_key` took once at entry. A slice rather than a fixed array:
/// text contexts have one layer (their own table), non-text contexts two or
/// three (own table, any shared layers, then GLOBAL).
pub(crate) fn resolve(layers: Layers, pending: Option<Key>, key: Key) -> Resolved {
    if let Some(prefix) = pending {
        for layer in layers {
            if let Some(action) = lookup_chord(layer, prefix, key) {
                return Resolved::Act(action);
            }
        }
        // Chord miss: drop the prefix and resolve `key` fresh (atuin
        // behavior) rather than treating it as unbound.
        return resolve(layers, None, key);
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

/// `contexts` in declaration order, grouping consecutive rows for the same
/// `(context, action)` into one [`HelpRow`], plus the floor's static rows,
/// each finalized (`HelpRow::finalize`) once its `bindings` group is
/// complete.
pub(crate) fn help_rows(contexts: &[(&'static str, &[Table])]) -> Vec<HelpRow> {
    let mut builder = HelpRowBuilder::default();
    for &(context, tables) in contexts {
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

    fn binding_keys(binding: &Binding) -> Vec<Key> {
        match binding {
            Binding::Single(k) => vec![*k],
            Binding::Chord(a, b) => vec![*a, *b],
        }
    }

    /// Every `(Binding, Action)` row across `layers`, flattened so the
    /// invariant tests below stay a single loop level instead of nesting
    /// through layers/rows.
    fn layer_bindings(layers: Layers) -> Vec<(Binding, Action)> {
        layers
            .iter()
            .flat_map(|layer| layer.iter())
            .copied()
            .collect()
    }

    /// Every key named by any binding across `layers`.
    fn layer_keys(layers: Layers) -> Vec<Key> {
        layer_bindings(layers)
            .iter()
            .flat_map(|(binding, _)| binding_keys(binding))
            .collect()
    }

    // -- Resolution units (Testing strategy, item 2) -------------------

    #[test]
    fn chord_hit_g_g_selects_top() {
        let pending = match resolve(crate::LIST_KEYMAP.layers, None, Key::char('g')) {
            Resolved::Pending(k) => k,
            other => unreachable!("expected Pending, got {other:?}"),
        };
        assert_eq!(
            resolve(crate::LIST_KEYMAP.layers, Some(pending), Key::char('g')),
            Resolved::Act(Action::MoveTop)
        );
    }

    #[test]
    fn chord_miss_g_j_falls_through_to_move_down() {
        assert_eq!(
            resolve(
                crate::LIST_KEYMAP.layers,
                Some(Key::char('g')),
                Key::char('j')
            ),
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
            resolve(&[CONTEXT_LAYER, GLOBAL_STAND_IN], None, Key::char('d')),
            Resolved::Act(Action::ToggleSortDirection)
        );
    }

    // -- Invariants (Testing strategy, item 3) --------------------------

    #[test]
    fn no_context_duplicates_a_binding() {
        for (name, keymap) in crate::ALL_KEYMAPS {
            let bindings: Vec<Binding> = layer_bindings(keymap.layers)
                .into_iter()
                .map(|(b, _)| b)
                .collect();
            for binding in &bindings {
                let occurrences = bindings.iter().filter(|b| *b == binding).count();
                assert!(occurrences <= 1, "{name}: duplicate binding {binding:?}");
            }
        }
    }

    #[test]
    fn no_key_is_both_single_bound_and_a_chord_prefix() {
        for (name, keymap) in crate::ALL_KEYMAPS {
            let bindings = layer_bindings(keymap.layers);
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
                    "{name}: {prefix} is both single-bound and a chord prefix"
                );
            }
        }
    }

    #[test]
    fn every_table_binding_round_trips_through_display_and_from_str() {
        for (_, keymap) in crate::ALL_KEYMAPS {
            for key in layer_keys(keymap.layers) {
                assert_eq!(key.to_string().parse::<Key>(), Ok(key));
            }
        }
    }

    #[test]
    fn no_table_binds_q_and_only_comment_input_binds_esc() {
        for (name, keymap) in crate::ALL_KEYMAPS {
            for key in layer_keys(keymap.layers) {
                assert!(
                    !matches!(key.code, KeyCode::Char('q')),
                    "{name}: table binds {key}"
                );
                if key.code == KeyCode::Esc {
                    assert_eq!(
                        *name, "comment_input",
                        "{name}: table binds esc (Back/quit are the floor's, except comment_input's cancel)"
                    );
                }
            }
        }
    }

    // -- Binding snapshot (Testing strategy, item 4) --------------------

    #[test]
    fn binding_snapshot() {
        let mut lines = Vec::new();
        for &(context, tables) in crate::HELP_CONTEXTS {
            for table in tables {
                for (binding, action) in *table {
                    let binding_str = binding.to_string();
                    lines.push(format!(
                        "{context:<6} {binding_str:<10} -> {}",
                        action.label()
                    ));
                }
            }
        }
        insta::assert_snapshot!(lines.join("\n"));
    }

    /// The second snapshot (Testing strategy, item 4): `help_rows()`'s
    /// grouped, filterable output -- the exact data the help popup renders.
    #[test]
    fn help_rows_snapshot() {
        let lines: Vec<String> = help_rows(crate::HELP_CONTEXTS)
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
