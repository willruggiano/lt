use std::fmt;
use std::str::FromStr;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A normalized key press: SHIFT folds into `Char` case, `BackTab` becomes
/// `Tab`+SHIFT, and `Esc` always clears every modifier.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Key {
    pub(crate) code: KeyCode,
    pub(crate) mods: KeyModifiers,
}

fn normalize(mut code: KeyCode, mut mods: KeyModifiers) -> (KeyCode, KeyModifiers) {
    if code == KeyCode::BackTab {
        code = KeyCode::Tab;
        mods.insert(KeyModifiers::SHIFT);
    }
    if code == KeyCode::Esc {
        // esc is esc: a modifier-carrying esc collapses to plain esc.
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
        // code folds it away.
        mods.remove(KeyModifiers::SHIFT);
    }
    (code, mods)
}

impl From<KeyEvent> for Key {
    /// Strips everything but `CONTROL`/`ALT`/`SHIFT` from the modifiers,
    /// then normalizes.
    fn from(ev: KeyEvent) -> Self {
        let mods = ev.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let (code, mods) = normalize(ev.code, mods);
        Self { code, mods }
    }
}

impl Key {
    // Precondition: `c`/`code` must already be canonical -- `const fn` can't
    // call `normalize`.
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

    /// ctrl + a non-char key; `ctrl` above only takes a `char` since
    /// ctrl+letter needs `normalize`'s lowercasing.
    pub(crate) const fn ctrl_code(code: KeyCode) -> Self {
        Self {
            code,
            mods: KeyModifiers::CONTROL,
        }
    }

    /// The canonical post-normalization form `BackTab` folds into.
    pub(crate) const fn shift_tab() -> Self {
        Self {
            code: KeyCode::Tab,
            mods: KeyModifiers::SHIFT,
        }
    }
}

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

/// No runtime config surface uses this yet; see [[keybinds.md#Non-goals]].
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
    /// any order, then a key token.
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
            // Unreachable without the kitty keyboard protocol's extended
            // flags, which this app does not enable; a debug form suffices.
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

    /// Submit chords tolerate shift/ctrl+shift on enter.
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

    #[test]
    fn shift_down_folds_to_plain_down() {
        let key = Key::from(ev(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(key, Key::plain(KeyCode::Down));
    }

    /// A literal `Tab`+SHIFT event (not `BackTab`) also normalizes to
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
        // Pins the per-key half a chord is built from.
        let g = Key::char('g');
        assert_eq!(format!("{g} {g}"), "g g");
    }
}
