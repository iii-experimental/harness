//! Single-line UTF-8-safe editor buffer used by the input row.
//!
//! Multi-line input via Shift+Enter is deliberately deferred; this 0.1 keeps
//! the surface narrow so the focus stays on event-loop integration. The cursor
//! tracks a byte offset into a `String` and every public method keeps that
//! offset on a UTF-8 boundary.

/// Mutable text buffer with a single byte-offset cursor.
#[derive(Debug, Clone, Default)]
pub struct EditorBuffer {
    text: String,
    cursor: usize,
}

impl EditorBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Display column for the cursor. `unicode-width` would give wide-char
    /// awareness; for 0.1 we approximate with `chars().count()`.
    pub fn display_cursor(&self) -> usize {
        self.text[..self.cursor].chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Replace the entire buffer, placing the cursor at the end.
    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    /// Drain the buffer, returning the text.
    pub fn take(&mut self) -> String {
        let out = std::mem::take(&mut self.text);
        self.cursor = 0;
        out
    }

    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn insert_str(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn delete_back(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.prev_char_boundary(self.cursor);
        self.text.replace_range(prev..self.cursor, "");
        self.cursor = prev;
    }

    pub fn delete_forward(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.next_char_boundary(self.cursor);
        self.text.replace_range(self.cursor..next, "");
    }

    /// Delete the word ending at the cursor (Ctrl+W).
    /// A word is a run of non-whitespace; trailing whitespace is also eaten.
    pub fn delete_word_back(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let bytes = self.text.as_bytes();
        let mut idx = self.cursor;
        while idx > 0 {
            let prev = self.prev_char_boundary(idx);
            if !(bytes[prev] as char).is_whitespace() {
                break;
            }
            idx = prev;
        }
        while idx > 0 {
            let prev = self.prev_char_boundary(idx);
            if (bytes[prev] as char).is_whitespace() {
                break;
            }
            idx = prev;
        }
        self.text.replace_range(idx..self.cursor, "");
        self.cursor = idx;
    }

    pub fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor = self.prev_char_boundary(self.cursor);
    }

    pub fn move_right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        self.cursor = self.next_char_boundary(self.cursor);
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    fn prev_char_boundary(&self, mut idx: usize) -> usize {
        if idx == 0 {
            return 0;
        }
        idx -= 1;
        while idx > 0 && !self.text.is_char_boundary(idx) {
            idx -= 1;
        }
        idx
    }

    fn next_char_boundary(&self, mut idx: usize) -> usize {
        let len = self.text.len();
        if idx >= len {
            return len;
        }
        idx += 1;
        while idx < len && !self.text.is_char_boundary(idx) {
            idx += 1;
        }
        idx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_take_returns_text() {
        let mut b = EditorBuffer::new();
        b.insert_char('h');
        b.insert_char('i');
        assert_eq!(b.text(), "hi");
        assert_eq!(b.cursor(), 2);
        let taken = b.take();
        assert_eq!(taken, "hi");
        assert!(b.is_empty());
        assert_eq!(b.cursor(), 0);
    }

    #[test]
    fn delete_back_removes_one_char() {
        let mut b = EditorBuffer::new();
        b.set("abc");
        b.delete_back();
        assert_eq!(b.text(), "ab");
        assert_eq!(b.cursor(), 2);
    }

    #[test]
    fn delete_back_at_start_is_noop() {
        let mut b = EditorBuffer::new();
        b.set("abc");
        b.home();
        b.delete_back();
        assert_eq!(b.text(), "abc");
        assert_eq!(b.cursor(), 0);
    }

    #[test]
    fn delete_word_back_eats_word_and_trailing_spaces() {
        let mut b = EditorBuffer::new();
        b.set("hello world");
        b.delete_word_back();
        assert_eq!(b.text(), "hello ");
        b.delete_word_back();
        assert_eq!(b.text(), "");
    }

    #[test]
    fn move_left_right_walks_utf8_boundaries() {
        let mut b = EditorBuffer::new();
        // multi-byte char between ascii
        b.set("a\u{1F600}b");
        b.home();
        b.move_right();
        assert_eq!(b.cursor(), 1); // past 'a'
        b.move_right();
        assert_eq!(b.cursor(), 5); // past emoji (4 bytes)
        b.move_right();
        assert_eq!(b.cursor(), 6); // past 'b'
        b.move_left();
        assert_eq!(b.cursor(), 5);
        b.move_left();
        assert_eq!(b.cursor(), 1);
    }

    #[test]
    fn home_and_end_jump() {
        let mut b = EditorBuffer::new();
        b.set("hello");
        b.home();
        assert_eq!(b.cursor(), 0);
        b.end();
        assert_eq!(b.cursor(), 5);
    }

    #[test]
    fn insert_char_in_middle() {
        let mut b = EditorBuffer::new();
        b.set("ac");
        b.move_left();
        b.insert_char('b');
        assert_eq!(b.text(), "abc");
        assert_eq!(b.cursor(), 2);
    }

    #[test]
    fn display_cursor_counts_chars_not_bytes() {
        let mut b = EditorBuffer::new();
        b.set("a\u{1F600}b");
        // cursor is at end (byte 6), display should be 3 chars
        assert_eq!(b.display_cursor(), 3);
    }
}
