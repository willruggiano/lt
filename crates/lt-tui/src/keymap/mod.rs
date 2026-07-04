//! The keymap: [`Key`]/[`Binding`]/[`Action`], [`resolve`] (a key through a
//! view's declared layers into an action), and [`help_rows`] (help-panel rows
//! from the same tables). [`GLOBAL`] is the shared navigation layer.

mod action;
mod key;

use std::fmt;

pub(crate) use action::Action;
use crossterm::event::KeyCode;
pub(crate) use key::Key;

/// A single- or two-key binding. `Chord(Key, Key)` over `Vec<Key>` makes
/// deeper nesting unrepresentable rather than untested.
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

pub(crate) type Table = &'static [(Binding, Action)];

/// A view's effective resolution layers, in precedence order: its own table
/// first, then any shared layers.
pub(crate) type Layers = &'static [Table];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Resolved {
    Act(Action),
    Pending(Key),
    Unbound(Key),
}

// ---------------------------------------------------------------------------
// Shared vocabulary
// ---------------------------------------------------------------------------

/// A view whose own table forwards to a text editor instead of cascading
/// skips this layer, so a navigation letter never steals a character from
/// the editor.
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
// Resolution
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

/// `layers` is a slice rather than a fixed array since layer count varies by
/// context (one to three).
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
// Help overlay
// ---------------------------------------------------------------------------

/// One or more equivalent bindings for the same `(context, action)` -- e.g.
/// `j`/`down` -- grouped so the panel shows them on one line.
pub(crate) struct HelpRow {
    pub(crate) bindings: Vec<Binding>,
    pub(crate) label: &'static str,
    pub(crate) context: &'static str,
    /// `Display` forms joined with `" / "`, e.g. `"j / down"`.
    pub(crate) binding_form: String,
    /// Lowercase `"{binding_form} {label} {context}"`, for filter matching.
    pub(crate) haystack: String,
}

impl HelpRow {
    /// `bindings` still open to appending; `binding_form`/`haystack` are
    /// empty until `finalize`.
    fn open(bindings: Vec<Binding>, label: &'static str, context: &'static str) -> Self {
        Self {
            bindings,
            label,
            context,
            binding_form: String::new(),
            haystack: String::new(),
        }
    }

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

/// `esc`/`q` aren't table bindings, so they're appended here by hand.
fn floor_rows() -> Vec<HelpRow> {
    vec![
        HelpRow::open(
            vec![Binding::Single(Key::plain(KeyCode::Esc))],
            "close, back to the view beneath",
            "overlay",
        ),
        // Unlike esc, q is typed in text contexts, so its "close" meaning
        // only applies outside one.
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

    fn layer_bindings(layers: Layers) -> Vec<(Binding, Action)> {
        layers
            .iter()
            .flat_map(|layer| layer.iter())
            .copied()
            .collect()
    }

    fn layer_keys(layers: Layers) -> Vec<Key> {
        layer_bindings(layers)
            .iter()
            .flat_map(|(binding, _)| binding_keys(binding))
            .collect()
    }

    // -- Resolution units -------------------------------------------------

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
        // No production table overrides GLOBAL (the invariant test below
        // forbids it), so this uses synthetic layers instead.
        const CONTEXT_LAYER: &[(Binding, Action)] =
            &[(Binding::Single(Key::char('d')), Action::ToggleSortDirection)];
        const GLOBAL_STAND_IN: &[(Binding, Action)] =
            &[(Binding::Single(Key::char('d')), Action::MoveDown)];

        assert_eq!(
            resolve(&[CONTEXT_LAYER, GLOBAL_STAND_IN], None, Key::char('d')),
            Resolved::Act(Action::ToggleSortDirection)
        );
    }

    // -- Invariants ---------------------------------------------------------

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

    // -- Binding snapshot -----------------------------------------------

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

    /// `help_rows()`'s grouped, filterable output, as opposed to the raw
    /// table above.
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
