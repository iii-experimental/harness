//! Multi-line UTF-8-safe editor buffer used by the input row.
//!
//! Lines are stored as a `Vec<String>`; the cursor is `(row, col)` where `col`
//! is a byte offset into `lines[row]` kept on a UTF-8 boundary. All editing
//! operations operate on the current row except `insert_newline`, `move_up`,
//! and `move_down` which cross row boundaries.

/// Mutable text buffer with `(row, col)` cursor over a vector of lines.
#[derive(Debug, Clone)]
pub struct EditorBuffer {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
}

impl Default for EditorBuffer {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
        }
    }
}

impl EditorBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Joined buffer text with `\n` between rows.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn current_line(&self) -> &str {
        &self.lines[self.cursor_row]
    }

    pub fn cursor_row(&self) -> usize {
        self.cursor_row
    }

    pub fn cursor_col(&self) -> usize {
        self.cursor_col
    }

    /// Byte cursor into `current_line()`. Kept for tests + back-compat with the
    /// single-line surface.
    pub fn cursor(&self) -> usize {
        self.cursor_col
    }

    /// Display column for the cursor on the current row (chars, not bytes).
    pub fn display_cursor(&self) -> usize {
        self.lines[self.cursor_row][..self.cursor_col]
            .chars()
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn is_multiline(&self) -> bool {
        self.lines.len() > 1
    }

    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    /// Replace the entire buffer, placing the cursor at the end. Embedded `\n`
    /// in `text` produce real rows.
    pub fn set(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(str::to_string).collect()
        };
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = self.lines[self.cursor_row].len();
    }

    /// Drain the buffer, returning the joined text.
    pub fn take_text(&mut self) -> String {
        let out = self.text();
        self.clear();
        out
    }

    /// Alias for back-compat with single-line callers.
    pub fn take(&mut self) -> String {
        self.take_text()
    }

    pub fn insert_char(&mut self, c: char) {
        let row = &mut self.lines[self.cursor_row];
        row.insert(self.cursor_col, c);
        self.cursor_col += c.len_utf8();
    }

    pub fn insert_str(&mut self, s: &str) {
        let row = &mut self.lines[self.cursor_row];
        row.insert_str(self.cursor_col, s);
        self.cursor_col += s.len();
    }

    /// Split current row at the cursor, dropping a new empty row in front.
    pub fn insert_newline(&mut self) {
        let tail = self.lines[self.cursor_row].split_off(self.cursor_col);
        self.lines.insert(self.cursor_row + 1, tail);
        self.cursor_row += 1;
        self.cursor_col = 0;
    }

    pub fn delete_back(&mut self) {
        if self.cursor_col == 0 {
            if self.cursor_row == 0 {
                return;
            }
            // Join with previous row.
            let cur = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
            self.lines[self.cursor_row].push_str(&cur);
            return;
        }
        let prev = self.prev_char_boundary(self.cursor_col);
        let row = &mut self.lines[self.cursor_row];
        row.replace_range(prev..self.cursor_col, "");
        self.cursor_col = prev;
    }

    pub fn delete_forward(&mut self) {
        let line_len = self.lines[self.cursor_row].len();
        if self.cursor_col >= line_len {
            // Pull the next row up.
            if self.cursor_row + 1 < self.lines.len() {
                let next = self.lines.remove(self.cursor_row + 1);
                self.lines[self.cursor_row].push_str(&next);
            }
            return;
        }
        let next = self.next_char_boundary(self.cursor_col);
        self.lines[self.cursor_row].replace_range(self.cursor_col..next, "");
    }

    /// Delete the word ending at the cursor (Ctrl+W). Stays within the current
    /// row; if at column 0, joins with previous row instead of crossing words.
    pub fn delete_word_back(&mut self) {
        if self.cursor_col == 0 {
            self.delete_back();
            return;
        }
        let row = &self.lines[self.cursor_row];
        let bytes = row.as_bytes();
        let mut idx = self.cursor_col;
        while idx > 0 {
            let prev = prev_boundary(row, idx);
            if !(bytes[prev] as char).is_whitespace() {
                break;
            }
            idx = prev;
        }
        while idx > 0 {
            let prev = prev_boundary(row, idx);
            if (bytes[prev] as char).is_whitespace() {
                break;
            }
            idx = prev;
        }
        self.lines[self.cursor_row].replace_range(idx..self.cursor_col, "");
        self.cursor_col = idx;
    }

    pub fn move_left(&mut self) {
        if self.cursor_col == 0 {
            if self.cursor_row > 0 {
                self.cursor_row -= 1;
                self.cursor_col = self.lines[self.cursor_row].len();
            }
            return;
        }
        self.cursor_col = self.prev_char_boundary(self.cursor_col);
    }

    pub fn move_right(&mut self) {
        let line_len = self.lines[self.cursor_row].len();
        if self.cursor_col >= line_len {
            if self.cursor_row + 1 < self.lines.len() {
                self.cursor_row += 1;
                self.cursor_col = 0;
            }
            return;
        }
        self.cursor_col = self.next_char_boundary(self.cursor_col);
    }

    /// Returns true if the cursor row changed.
    pub fn move_up(&mut self) -> bool {
        if self.cursor_row == 0 {
            return false;
        }
        let want = self.lines[self.cursor_row][..self.cursor_col]
            .chars()
            .count();
        self.cursor_row -= 1;
        self.cursor_col = char_index_to_byte(&self.lines[self.cursor_row], want);
        true
    }

    /// Returns true if the cursor row changed.
    pub fn move_down(&mut self) -> bool {
        if self.cursor_row + 1 >= self.lines.len() {
            return false;
        }
        let want = self.lines[self.cursor_row][..self.cursor_col]
            .chars()
            .count();
        self.cursor_row += 1;
        self.cursor_col = char_index_to_byte(&self.lines[self.cursor_row], want);
        true
    }

    pub fn home(&mut self) {
        self.cursor_col = 0;
    }

    pub fn end(&mut self) {
        self.cursor_col = self.lines[self.cursor_row].len();
    }

    pub fn cursor_at_first_row(&self) -> bool {
        self.cursor_row == 0
    }

    pub fn cursor_at_last_row(&self) -> bool {
        self.cursor_row + 1 == self.lines.len()
    }

    fn prev_char_boundary(&self, idx: usize) -> usize {
        prev_boundary(&self.lines[self.cursor_row], idx)
    }

    fn next_char_boundary(&self, idx: usize) -> usize {
        next_boundary(&self.lines[self.cursor_row], idx)
    }
}

fn prev_boundary(s: &str, mut idx: usize) -> usize {
    if idx == 0 {
        return 0;
    }
    idx -= 1;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn next_boundary(s: &str, mut idx: usize) -> usize {
    let len = s.len();
    if idx >= len {
        return len;
    }
    idx += 1;
    while idx < len && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

fn char_index_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map_or_else(|| s.len(), |(b, _)| b)
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
        b.set("a\u{1F600}b");
        b.home();
        b.move_right();
        assert_eq!(b.cursor(), 1);
        b.move_right();
        assert_eq!(b.cursor(), 5);
        b.move_right();
        assert_eq!(b.cursor(), 6);
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
        assert_eq!(b.display_cursor(), 3);
    }

    #[test]
    fn insert_newline_at_end_of_line_appends_empty_row() {
        let mut b = EditorBuffer::new();
        b.set("hello");
        b.insert_newline();
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.cursor_row(), 1);
        assert_eq!(b.cursor_col(), 0);
        assert_eq!(b.text(), "hello\n");
    }

    #[test]
    fn insert_newline_in_middle_splits_row() {
        let mut b = EditorBuffer::new();
        b.set("abcdef");
        b.home();
        b.move_right();
        b.move_right();
        b.move_right();
        b.insert_newline();
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.lines()[0], "abc");
        assert_eq!(b.lines()[1], "def");
        assert_eq!(b.cursor_row(), 1);
        assert_eq!(b.cursor_col(), 0);
    }

    #[test]
    fn insert_newline_at_start_pushes_text_down() {
        let mut b = EditorBuffer::new();
        b.set("abc");
        b.home();
        b.insert_newline();
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.lines()[0], "");
        assert_eq!(b.lines()[1], "abc");
        assert_eq!(b.cursor_row(), 1);
        assert_eq!(b.cursor_col(), 0);
    }

    #[test]
    fn move_up_down_walks_rows() {
        let mut b = EditorBuffer::new();
        b.set("foo\nbar\nbaz");
        // cursor at end of "baz"
        assert_eq!(b.cursor_row(), 2);
        assert!(b.move_up());
        assert_eq!(b.cursor_row(), 1);
        assert!(b.move_up());
        assert_eq!(b.cursor_row(), 0);
        assert!(!b.move_up());
        assert!(b.move_down());
        assert_eq!(b.cursor_row(), 1);
    }

    #[test]
    fn take_text_round_trip_with_newlines() {
        let mut b = EditorBuffer::new();
        b.set("line1\nline2\nline3");
        let taken = b.take_text();
        assert_eq!(taken, "line1\nline2\nline3");
        assert!(b.is_empty());
        assert_eq!(b.line_count(), 1);
    }

    #[test]
    fn delete_back_at_col_zero_joins_with_previous_row() {
        let mut b = EditorBuffer::new();
        b.set("ab\ncd");
        // cursor at end of "cd". Move to col 0 of row 1.
        b.home();
        assert_eq!(b.cursor_row(), 1);
        assert_eq!(b.cursor_col(), 0);
        b.delete_back();
        assert_eq!(b.line_count(), 1);
        assert_eq!(b.text(), "abcd");
        assert_eq!(b.cursor_col(), 2);
    }
}
