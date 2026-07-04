use crossterm::event::{KeyCode, KeyModifiers};

/// A single-line text input with a byte-offset cursor and vim/emacs-style
/// key bindings (see `handle_key`).
#[derive(Clone, Default)]
pub struct TextInput {
    pub value: String,
    /// Byte offset of the cursor, always on a char boundary.
    pub cursor: usize,
    /// If set, `cursor..selection_end` is "selected"; always >= cursor and
    /// on a char boundary. Typing replaces it; movement clears it.
    pub selection_end: Option<usize>,
}

impl From<String> for TextInput {
    fn from(value: String) -> Self {
        let cursor = value.len();
        Self {
            value,
            cursor,
            selection_end: None,
        }
    }
}

impl TextInput {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `(before_cursor, char_at_cursor_or_none, after_cursor_past_that_char)`.
    /// Useful for rendering the cursor position.
    pub fn display_parts(&self) -> (&str, Option<char>, &str) {
        let before = &self.value[..self.cursor];
        let rest = &self.value[self.cursor..];
        let mut chars = rest.chars();
        let ch = chars.next();
        (before, ch, chars.as_str())
    }

    fn prev_char_boundary(&self) -> usize {
        if self.cursor == 0 {
            return 0;
        }
        let mut i = self.cursor - 1;
        while !self.value.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    fn next_char_boundary(&self) -> usize {
        if self.cursor >= self.value.len() {
            return self.value.len();
        }
        let Some(ch) = self.value[self.cursor..].chars().next() else {
            return self.cursor;
        };
        self.cursor + ch.len_utf8()
    }

    fn prev_word_boundary(&self) -> usize {
        let before = &self.value[..self.cursor];
        let trimmed = before.trim_end();
        match trimmed.rfind(|c: char| c.is_whitespace()) {
            Some(i) => trimmed[i..]
                .chars()
                .next()
                .map_or(i, |ws_char| i + ws_char.len_utf8()),
            None => 0,
        }
    }

    fn next_word_boundary(&self) -> usize {
        let rest = &self.value[self.cursor..];
        let mut chars = rest.char_indices().peekable();
        // Skip leading whitespace.
        while let Some(&(_, c)) = chars.peek() {
            if c.is_whitespace() {
                chars.next();
            } else {
                break;
            }
        }
        // Skip word characters.
        for (i, c) in chars {
            if c.is_whitespace() {
                return self.cursor + i;
            }
        }
        self.value.len()
    }

    pub fn move_left(&mut self) {
        self.selection_end = None;
        self.cursor = self.prev_char_boundary();
    }

    pub fn move_right(&mut self) {
        self.selection_end = None;
        self.cursor = self.next_char_boundary();
    }

    pub fn move_word_left(&mut self) {
        self.selection_end = None;
        self.cursor = self.prev_word_boundary();
    }

    pub fn move_word_right(&mut self) {
        self.selection_end = None;
        self.cursor = self.next_word_boundary();
    }

    pub fn move_start(&mut self) {
        self.selection_end = None;
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.selection_end = None;
        self.cursor = self.value.len();
    }

    /// If a selection is active, deletes it instead.
    pub fn backspace(&mut self) {
        if let Some(end) = self.selection_end.take() {
            self.value.drain(self.cursor..end);
        } else if self.cursor > 0 {
            let prev = self.prev_char_boundary();
            self.value.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    /// If a selection is active, deletes it instead.
    pub fn delete_forward(&mut self) {
        if let Some(end) = self.selection_end.take() {
            self.value.drain(self.cursor..end);
        } else if self.cursor < self.value.len() {
            let next = self.next_char_boundary();
            self.value.drain(self.cursor..next);
        }
    }

    pub fn delete_word_before(&mut self) {
        self.selection_end = None;
        let start = self.prev_word_boundary();
        self.value.drain(start..self.cursor);
        self.cursor = start;
    }

    pub fn delete_word_after(&mut self) {
        self.selection_end = None;
        let end = self.next_word_boundary();
        self.value.drain(self.cursor..end);
    }

    pub fn delete_to_start(&mut self) {
        self.selection_end = None;
        self.value.drain(..self.cursor);
        self.cursor = 0;
    }

    pub fn delete_to_end(&mut self) {
        self.selection_end = None;
        self.value.truncate(self.cursor);
    }

    /// If a selection is active, it's deleted first so typing replaces it.
    pub fn insert(&mut self, c: char) {
        if let Some(end) = self.selection_end.take() {
            self.value.drain(self.cursor..end);
        }
        self.value.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// `Some(true)` when a deletion key was handled; `None` if `code` isn't
    /// one.
    fn handle_deletion_key(&mut self, code: KeyCode, ctrl: bool, alt: bool) -> Option<bool> {
        match code {
            KeyCode::Backspace => self.backspace(),
            KeyCode::Char('h') if ctrl => self.backspace(),
            KeyCode::Char('w') if ctrl => self.delete_word_before(),
            KeyCode::Char('u') if ctrl => self.delete_to_start(),
            KeyCode::Char('k') if ctrl => self.delete_to_end(),
            KeyCode::Char('d') if ctrl => self.delete_forward(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Char('d') if alt => self.delete_word_after(),
            _ => return None,
        }
        Some(true)
    }

    /// Returns `true` if the value changed; `false` if only the cursor
    /// moved or the key was unhandled.
    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        let alt = modifiers.contains(KeyModifiers::ALT);
        if let Some(changed) = self.handle_deletion_key(code, ctrl, alt) {
            return changed;
        }
        match code {
            // -- movement ----------------------------------------------------
            KeyCode::Char('a') if ctrl => {
                self.move_start();
                false
            }
            KeyCode::Char('e') if ctrl => {
                self.move_end();
                false
            }
            KeyCode::Char('f') if ctrl => {
                self.move_right();
                false
            }
            KeyCode::Char('b') if ctrl => {
                self.move_left();
                false
            }
            KeyCode::Left if ctrl => {
                self.move_word_left();
                false
            }
            KeyCode::Right if ctrl => {
                self.move_word_right();
                false
            }
            KeyCode::Left => {
                self.move_left();
                false
            }
            KeyCode::Right => {
                self.move_right();
                false
            }
            KeyCode::Home => {
                self.move_start();
                false
            }
            KeyCode::End => {
                self.move_end();
                false
            }
            // -- insert ------------------------------------------------------
            KeyCode::Char(c) if !ctrl && !alt => {
                self.insert(c);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod text_input_tests {
    use super::*;

    fn input(s: &str, cursor: usize) -> TextInput {
        TextInput {
            value: s.to_string(),
            cursor,
            selection_end: None,
        }
    }

    #[test]
    fn from_string_places_cursor_at_end() {
        let t = TextInput::from("hello".to_string());
        assert_eq!(t.cursor, 5);
        assert_eq!(t.value.as_str(), "hello");
    }

    #[test]
    fn display_parts_splits_around_cursor() {
        let t = input("hello", 2);
        assert_eq!(t.display_parts(), ("he", Some('l'), "lo"));
        let at_end = input("hi", 2);
        assert_eq!(at_end.display_parts(), ("hi", None, ""));
    }

    #[test]
    fn insert_advances_cursor_and_handles_multibyte() {
        let mut t = TextInput::new();
        t.insert('a');
        t.insert('é'); // 2-byte char
        assert_eq!(t.value, "aé");
        assert_eq!(t.cursor, 3);
    }

    #[test]
    fn move_left_right_respect_char_boundaries() {
        let mut t = input("aé", 3);
        t.move_left();
        assert_eq!(t.cursor, 1); // stepped over the 2-byte 'é'
        t.move_left();
        assert_eq!(t.cursor, 0);
        t.move_left(); // clamp at 0
        assert_eq!(t.cursor, 0);
        t.move_right();
        assert_eq!(t.cursor, 1);
        t.move_end();
        assert_eq!(t.cursor, 3);
        t.move_right(); // clamp at end
        assert_eq!(t.cursor, 3);
        t.move_start();
        assert_eq!(t.cursor, 0);
    }

    #[test]
    fn word_movement_skips_whitespace_runs() {
        let mut t = input("foo  bar baz", 12);
        t.move_word_left();
        assert_eq!(&t.value[t.cursor..], "baz");
        t.move_word_left();
        assert_eq!(&t.value[t.cursor..], "bar baz");
        let mut f = input("foo  bar", 0);
        f.move_word_right();
        assert_eq!(f.cursor, 3); // stops at end of "foo"
        f.move_word_right();
        assert_eq!(f.cursor, 8); // skips spaces, then to end of "bar"
    }

    #[test]
    fn backspace_and_delete_forward() {
        let mut t = input("abc", 2);
        t.backspace();
        assert_eq!((t.value.as_str(), t.cursor), ("ac", 1));
        let mut at_start = input("abc", 0);
        at_start.backspace(); // no-op
        assert_eq!(at_start.value, "abc");
        let mut d = input("abc", 1);
        d.delete_forward();
        assert_eq!((d.value.as_str(), d.cursor), ("ac", 1));
        let mut at_end = input("abc", 3);
        at_end.delete_forward(); // no-op
        assert_eq!(at_end.value, "abc");
    }

    #[test]
    fn word_and_line_deletions() {
        let mut w = input("foo bar", 7);
        w.delete_word_before();
        assert_eq!((w.value.as_str(), w.cursor), ("foo ", 4));
        let mut a = input("foo bar", 3);
        a.delete_word_after();
        assert_eq!(a.value, "foo"); // deletes " bar"
        let mut u = input("foo bar", 4);
        u.delete_to_start();
        assert_eq!((u.value.as_str(), u.cursor), ("bar", 0));
        let mut k = input("foo bar", 3);
        k.delete_to_end();
        assert_eq!(k.value, "foo");
    }

    #[test]
    fn selection_is_replaced_or_deleted_then_cleared() {
        // Insert over a selection replaces the range.
        let mut t = TextInput {
            value: "hello".to_string(),
            cursor: 1,
            selection_end: Some(4),
        };
        t.insert('X');
        assert_eq!(t.value, "hXo");
        assert!(t.selection_end.is_none());

        // Backspace deletes the selection.
        let mut b = TextInput {
            value: "hello".to_string(),
            cursor: 1,
            selection_end: Some(4),
        };
        b.backspace();
        assert_eq!(b.value, "ho");
        assert!(b.selection_end.is_none());

        // delete_forward deletes the selection.
        let mut d = TextInput {
            value: "hello".to_string(),
            cursor: 1,
            selection_end: Some(4),
        };
        d.delete_forward();
        assert_eq!(d.value, "ho");

        // Movement clears a selection without editing.
        let mut m = TextInput {
            value: "hello".to_string(),
            cursor: 1,
            selection_end: Some(4),
        };
        m.move_left();
        assert!(m.selection_end.is_none());
        assert_eq!(m.value, "hello");
    }

    #[test]
    fn handle_key_insert_movement_and_unhandled() {
        let ctrl = KeyModifiers::CONTROL;
        let none = KeyModifiers::NONE;
        let mut ti = TextInput::new();

        // Insert returns true (changed).
        assert!(ti.handle_key(KeyCode::Char('a'), none));
        assert_eq!(ti.value, "a");

        // Movement returns false (cursor only).
        for (code, mods) in [
            (KeyCode::Left, none),
            (KeyCode::Char('e'), ctrl), // move_end
            (KeyCode::Char('a'), ctrl), // move_start
            (KeyCode::Home, none),
            (KeyCode::End, none),
            (KeyCode::Right, ctrl), // word right
        ] {
            assert!(!ti.handle_key(code, mods));
        }

        // Unhandled key, and a non-binding ctrl+char, are ignored.
        assert!(!ti.handle_key(KeyCode::Esc, none));
        assert!(!ti.handle_key(KeyCode::Char('z'), ctrl));
        assert_eq!(ti.value, "a");
    }

    #[test]
    fn handle_key_deletions_change_buffer() {
        let ctrl = KeyModifiers::CONTROL;
        let alt = KeyModifiers::ALT;
        let none = KeyModifiers::NONE;

        let mut wrd = input("foo bar", 7);
        assert!(wrd.handle_key(KeyCode::Char('w'), ctrl)); // delete word before
        assert_eq!(wrd.value, "foo ");
        assert!(wrd.handle_key(KeyCode::Backspace, none));
        assert!(wrd.handle_key(KeyCode::Char('u'), ctrl)); // delete to start
        assert_eq!(wrd.value, "");

        let mut fwd = input("abcd", 0);
        assert!(fwd.handle_key(KeyCode::Char('d'), ctrl)); // forward delete
        assert_eq!(fwd.value, "bcd");
        assert!(fwd.handle_key(KeyCode::Delete, none));
        assert_eq!(fwd.value, "cd");

        let mut cut = input("hello world", 5);
        assert!(cut.handle_key(KeyCode::Char('k'), ctrl)); // delete to end
        assert_eq!(cut.value, "hello");

        let mut after = input("foo bar", 0);
        assert!(after.handle_key(KeyCode::Char('d'), alt)); // delete word after
        assert_eq!(after.value, " bar");
    }
}
