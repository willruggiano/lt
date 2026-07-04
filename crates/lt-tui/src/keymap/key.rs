use std::fmt;
use std::str::FromStr;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A normalized key press. Canonical form:
/// - Char keys carry case in the char itself; SHIFT is always cleared for
///   `Char` (shift+p arrives as `Char('P')`, stored as `'P'`).
/// - ctrl+letter is stored lowercase (`"ctrl+d"`, never `"ctrl+D"`).
/// - `BackTab` is normalized to `Tab` + SHIFT.
/// - `Esc` always clears every modifier: esc is esc, regardless of what
///   shift/alt/ctrl the terminal tacked onto it.
/// - SHIFT is cleared for every other code except `Tab` (whose SHIFT bit is
///   what distinguishes it from `shift+tab`); `Char` already folds SHIFT into
///   case above. This is what makes shift+enter/ctrl+shift+enter and
///   shift+arrow/pgdn match their unshifted bindings.
/// - Only `CONTROL`/`ALT`/`SHIFT` modifier bits are retained; kitty's extra
///   `KeyEventState` bits never reach this type since `Key` has no field for
///   them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Key {
    pub(crate) code: KeyCode,
    pub(crate) mods: KeyModifiers,
}

/// Fold `code`/`mods` into canonical form, shared by [`From<KeyEvent>`] and
/// [`FromStr`] so both entry points agree on the same normalization.
fn normalize(mut code: KeyCode, mut mods: KeyModifiers) -> (KeyCode, KeyModifiers) {
    if code == KeyCode::BackTab {
        code = KeyCode::Tab;
        mods.insert(KeyModifiers::SHIFT);
    }
    if code == KeyCode::Esc {
        // esc is esc: a modifier-carrying esc (shift+esc, alt+esc, ...)
        // collapses to plain esc so every esc check compares the normalized
        // key rather than re-deriving which modifiers to ignore.
        mods = KeyModifiers::NONE;
    }
    if let KeyCode::Char(c) = code {
        if mods.contains(KeyModifiers::SHIFT) {
            code = KeyCode::Char(c.to_ascii_uppercase());
            mods.remove(KeyModifiers::SHIFT);
        }
        if mods.contains(KeyModifiers::CONTROL)
            && let KeyCode::Char(c) = code
        {
            code = KeyCode::Char(c.to_ascii_lowercase());
        }
    } else if code != KeyCode::Tab {
        // SHIFT only distinguishes Tab (-> shift+tab); every other non-Char
        // code folds it away, restoring shift+enter/ctrl+shift+enter and
        // shift+arrow/pgdn tolerance.
        mods.remove(KeyModifiers::SHIFT);
    }
    (code, mods)
}

impl From<KeyEvent> for Key {
    /// The sole entry point from crossterm: strips everything but
    /// `CONTROL`/`ALT`/`SHIFT` from the modifiers (dropping kitty's
    /// `KeyEventState` and any other modifier bit), then normalizes.
    fn from(ev: KeyEvent) -> Self {
        let mods = ev.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let (code, mods) = normalize(ev.code, mods);
        Self { code, mods }
    }
}

impl Key {
    // Table-building const constructors. Callers pass the already-canonical
    // form directly (e.g. `Key::char('G')` for a bare capital G); these do
    // not re-run `normalize` since `const fn` cannot call it.
    pub(crate) const fn char(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            mods: KeyModifiers::NONE,
        }
    }

    pub(crate) const fn ctrl(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            mods: KeyModifiers::CONTROL,
        }
    }

    pub(crate) const fn plain(code: KeyCode) -> Self {
        Self {
            code,
            mods: KeyModifiers::NONE,
        }
    }

    pub(crate) const fn alt(code: KeyCode) -> Self {
        Self {
            code,
            mods: KeyModifiers::ALT,
        }
    }

    /// ctrl + a non-char key, e.g. `ctrl+enter`'s submit chord. The `ctrl`
    /// ctor above only takes a `char` since ctrl+letter needs `normalize`'s
    /// lowercasing; a non-char code has no such folding.
    pub(crate) const fn ctrl_code(code: KeyCode) -> Self {
        Self {
            code,
            mods: KeyModifiers::CONTROL,
        }
    }

    /// `shift+tab`, the canonical post-normalization form `BackTab` folds
    /// into (see `normalize`); table rows want it directly rather than
    /// through a `KeyCode::BackTab` roundabout.
    pub(crate) const fn shift_tab() -> Self {
        Self {
            code: KeyCode::Tab,
            mods: KeyModifiers::SHIFT,
        }
    }
}

/// Map a named-key token (case-insensitive) to its `KeyCode`, or a
/// single-character token to `Char`. The inverse of [`fmt::Display`]'s
/// per-code arm below.
fn code_from_str(token: &str) -> Option<KeyCode> {
    Some(match token.to_ascii_lowercase().as_str() {
        "enter" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pgup" | "pageup" => KeyCode::PageUp,
        "pgdn" | "pagedown" => KeyCode::PageDown,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "space" => KeyCode::Char(' '),
        _ => {
            let mut chars = token.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(c)
        }
    })
}

/// Parse error for [`Key::from_str`]. The keymap has no runtime config
/// surface yet ([[docs/design/keybinds.md]] Non-goals); `FromStr` exists for
/// the round-trip tests and a future config layer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ParseKeyError;

impl fmt::Display for ParseKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid key")
    }
}

impl std::error::Error for ParseKeyError {}

impl FromStr for Key {
    type Err = ParseKeyError;

    /// Lenient: modifier prefixes (`ctrl+`/`control+`, `alt+`, `shift+`) in
    /// any order, then a key token. `"shift+p"` folds to `'P'` through the
    /// same [`normalize`] path `From<KeyEvent>` uses.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut mods = KeyModifiers::NONE;
        let mut parts = s.split('+').peekable();
        let mut key_token = "";
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                key_token = part;
                break;
            }
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => mods.insert(KeyModifiers::CONTROL),
                "alt" => mods.insert(KeyModifiers::ALT),
                "shift" => mods.insert(KeyModifiers::SHIFT),
                _ => return Err(ParseKeyError),
            }
        }
        let code = code_from_str(key_token).ok_or(ParseKeyError)?;
        let (code, mods) = normalize(code, mods);
        Ok(Self { code, mods })
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.contains(KeyModifiers::CONTROL) {
            write!(f, "ctrl+")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            write!(f, "alt+")?;
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            write!(f, "shift+")?;
        }
        match self.code {
            KeyCode::Char(' ') => write!(f, "space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::Enter => write!(f, "enter"),
            KeyCode::Esc => write!(f, "esc"),
            KeyCode::Tab => write!(f, "tab"),
            KeyCode::Backspace => write!(f, "backspace"),
            KeyCode::Up => write!(f, "up"),
            KeyCode::Down => write!(f, "down"),
            KeyCode::Left => write!(f, "left"),
            KeyCode::Right => write!(f, "right"),
            KeyCode::Home => write!(f, "home"),
            KeyCode::End => write!(f, "end"),
            KeyCode::PageUp => write!(f, "pgup"),
            KeyCode::PageDown => write!(f, "pgdn"),
            KeyCode::Delete => write!(f, "delete"),
            KeyCode::Insert => write!(f, "insert"),
            KeyCode::F(n) => write!(f, "f{n}"),
            // Keys not reachable through a normal terminal without the kitty
            // keyboard protocol's REPORT_ALL_KEYS_AS_ESCAPE_CODES flag, which
            // this app does not enable (lib.rs's PushKeyboardEnhancementFlags
            // call); no table binds them, so a plain debug form is adequate.
            other => write!(f, "{other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEventKind, KeyEventState};

    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn ev_with_state(code: KeyCode, mods: KeyModifiers, state: KeyEventState) -> KeyEvent {
        KeyEvent::new_with_kind_and_state(code, mods, KeyEventKind::Press, state)
    }

    /// Test 1 (docs/design/keybinds.md, Testing strategy): round-trip
    /// agreement for representative events.
    #[test]
    fn round_trip_shift_g() {
        let key = Key::from(ev(KeyCode::Char('G'), KeyModifiers::SHIFT));
        assert_eq!(key.to_string(), "G");
        assert_eq!(key.to_string().parse::<Key>().unwrap(), key);
    }

    #[test]
    fn round_trip_back_tab() {
        let key = Key::from(ev(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(key.to_string(), "shift+tab");
        assert_eq!(key.to_string().parse::<Key>().unwrap(), key);
    }

    #[test]
    fn round_trip_ctrl_shift_d() {
        let key = Key::from(ev(
            KeyCode::Char('D'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(key.to_string(), "ctrl+d");
        assert_eq!(key.to_string().parse::<Key>().unwrap(), key);
    }

    #[test]
    fn round_trip_kitty_state_bits_are_dropped() {
        let with_state = Key::from(ev_with_state(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
            KeyEventState::CAPS_LOCK,
        ));
        let without_state = Key::from(ev(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(with_state, without_state);
        assert_eq!(with_state.to_string().parse::<Key>().unwrap(), with_state);
    }

    /// Rule (Types, "esc is esc"): a modifier-carrying esc collapses to
    /// plain esc, matching the plain-esc `COMMENT_INPUT` row and the
    /// code-only esc checks in `dispatch_key`/`unbound_flow`.
    #[test]
    fn esc_clears_every_modifier() {
        for mods in [
            KeyModifiers::SHIFT,
            KeyModifiers::ALT,
            KeyModifiers::CONTROL,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ] {
            let key = Key::from(ev(KeyCode::Esc, mods));
            assert_eq!(key, Key::plain(KeyCode::Esc), "mods {mods:?}");
            assert_eq!(key.to_string(), "esc");
            assert_eq!(key.to_string().parse::<Key>().unwrap(), key);
        }
    }

    /// Rule (Types, "SHIFT cleared except Tab"): shift+enter and
    /// ctrl+shift+enter fold to their unshifted forms -- the submit chords'
    /// modifier tolerance.
    #[test]
    fn shift_enter_folds_to_plain_enter() {
        let key = Key::from(ev(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(key, Key::plain(KeyCode::Enter));
    }

    #[test]
    fn ctrl_shift_enter_folds_to_ctrl_enter() {
        let key = Key::from(ev(
            KeyCode::Enter,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(key, Key::ctrl_code(KeyCode::Enter));
    }

    /// Shift+arrow/pgdn scrolling likewise folds to the unshifted binding.
    #[test]
    fn shift_down_folds_to_plain_down() {
        let key = Key::from(ev(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(key, Key::plain(KeyCode::Down));
    }

    /// Tab is the one code SHIFT still distinguishes: a literal
    /// `Tab`+SHIFT event (as opposed to `BackTab`) still normalizes to
    /// `shift+tab`.
    #[test]
    fn shift_tab_code_stays_shifted() {
        let key = Key::from(ev(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(key, Key::shift_tab());
    }

    #[test]
    fn from_str_is_lenient_about_shift_folding() {
        assert_eq!("shift+p".parse::<Key>().unwrap(), Key::char('P'));
    }

    #[test]
    fn from_str_rejects_unknown_modifiers() {
        assert!("meta+p".parse::<Key>().is_err());
    }

    #[test]
    fn space_round_trips_through_its_named_form() {
        let key = Key::char(' ');
        assert_eq!(key.to_string(), "space");
        assert_eq!(key.to_string().parse::<Key>().unwrap(), key);
    }

    #[test]
    fn alt_ctor_round_trips() {
        let key = Key::alt(KeyCode::Enter);
        assert_eq!(key.to_string(), "alt+enter");
        assert_eq!(key.to_string().parse::<Key>().unwrap(), key);
    }

    #[test]
    fn ctrl_code_ctor_round_trips() {
        let key = Key::ctrl_code(KeyCode::Enter);
        assert_eq!(key.to_string(), "ctrl+enter");
        assert_eq!(key.to_string().parse::<Key>().unwrap(), key);
    }

    #[test]
    fn shift_tab_ctor_round_trips() {
        let key = Key::shift_tab();
        assert_eq!(key.to_string(), "shift+tab");
        assert_eq!(key.to_string().parse::<Key>().unwrap(), key);
    }

    #[test]
    fn binding_round_trip_g_g() {
        // The `Binding` half of the round-trip (chord display) is exercised
        // in `keymap::mod`'s invariant test; this pins the per-key half a
        // chord is built from.
        let g = Key::char('g');
        assert_eq!(format!("{g} {g}"), "g g");
    }
}
