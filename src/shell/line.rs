//! The shell's prompt line: what is being typed, and where the cursor is.
//!
//! The text is held in a zeroizing buffer of fixed capacity that never
//! grows (growing would leave unwiped copies), like the passphrase prompt
//! (docs/decisions/0016-passphrase-prompt.md). The cursor moves by
//! grapheme, so a character with its accents, or an emoji, is one step.

use unicode_segmentation::UnicodeSegmentation;
use zeroize::{Zeroize, Zeroizing};

/// The longest line, in bytes.
pub const MAX_LINE: usize = 4096;

pub struct Line {
    text: Zeroizing<String>,
    /// A byte offset on a grapheme boundary.
    cursor: usize,
}

impl Default for Line {
    fn default() -> Self {
        Self::new()
    }
}

impl Line {
    pub fn new() -> Self {
        Self {
            text: Zeroizing::new(String::with_capacity(MAX_LINE)),
            cursor: 0,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Inserts `s` at the cursor, as much of it as fits (whole characters).
    /// Returns false when some of it did not fit.
    pub fn insert(&mut self, s: &str) -> bool {
        let room = MAX_LINE - self.text.len();
        let mut end = s.len().min(room);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        self.text.insert_str(self.cursor, &s[..end]);
        self.cursor += end;
        end == s.len()
    }

    /// The grapheme boundary before the cursor.
    fn previous(&self) -> usize {
        self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(at, _)| at)
    }

    /// The grapheme boundary after the cursor.
    fn next(&self) -> usize {
        self.text[self.cursor..]
            .graphemes(true)
            .next()
            .map_or(self.cursor, |g| self.cursor + g.len())
    }

    /// Removes `range`, wiping the bytes that move or are freed.
    fn remove(&mut self, start: usize, end: usize) {
        let len = self.text.len();
        // Safe on bytes: start and end are char boundaries, so the result
        // is valid UTF-8 again once truncated.
        let mut bytes = std::mem::take(&mut *self.text).into_bytes();
        bytes.copy_within(end..len, start);
        bytes[len - (end - start)..].zeroize();
        bytes.truncate(len - (end - start));
        *self.text = String::from_utf8(bytes).unwrap_or_default();
        if self.cursor > start {
            self.cursor = self.cursor.max(end) - (end - start);
        }
    }

    pub fn backspace(&mut self) {
        let start = self.previous();
        self.remove(start, self.cursor);
    }

    pub fn delete(&mut self) {
        let end = self.next();
        self.remove(self.cursor, end);
    }

    pub fn left(&mut self) {
        self.cursor = self.previous();
    }

    pub fn right(&mut self) {
        self.cursor = self.next();
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Ctrl+U: everything before the cursor.
    pub fn kill_before(&mut self) {
        self.remove(0, self.cursor);
    }

    /// Ctrl+K: everything after the cursor.
    pub fn kill_after(&mut self) {
        let len = self.text.len();
        self.remove(self.cursor, len);
    }

    /// Ctrl+W: the word before the cursor, and the spaces after it.
    pub fn kill_word(&mut self) {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end_matches(' ');
        let start = trimmed.rfind(' ').map_or(0, |i| i + 1);
        self.remove(start, self.cursor);
    }

    /// Replaces the text from `start` to the cursor with `s` (completion).
    pub fn replace_before_cursor(&mut self, start: usize, s: &str) {
        let start = start.min(self.cursor);
        self.remove(start, self.cursor);
        self.cursor = start;
        self.insert(s);
    }

    /// Replaces the whole line (from history), cursor at the end.
    pub fn set(&mut self, s: &str) {
        self.clear();
        self.insert(s);
    }

    /// Empties the line, wiping it; the buffer is kept.
    pub fn clear(&mut self) {
        let len = self.text.len();
        self.remove(0, len);
        self.cursor = 0;
    }

    /// Takes the line out, leaving an empty one with the same capacity.
    pub fn take(&mut self) -> Zeroizing<String> {
        let mut taken = Zeroizing::new(String::with_capacity(self.text.len()));
        taken.push_str(&self.text);
        self.clear();
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str) -> Line {
        let mut line = Line::new();
        line.insert(s);
        line
    }

    #[test]
    fn editing_moves_by_grapheme() {
        let mut l = line("ae\u{301}日");
        l.backspace();
        assert_eq!(l.as_str(), "ae\u{301}");
        l.left();
        assert_eq!(l.cursor(), 1);
        l.insert("x");
        assert_eq!(l.as_str(), "axe\u{301}");
        l.delete();
        assert_eq!(l.as_str(), "ax");
        l.home();
        l.delete();
        assert_eq!(l.as_str(), "x");
        l.end();
        l.right();
        assert_eq!(l.cursor(), 1);
    }

    #[test]
    fn kills() {
        let mut l = line("view  work/roadmap");
        l.kill_word();
        assert_eq!(l.as_str(), "view  ");
        l.kill_word();
        assert_eq!(l.as_str(), "");
        let mut l = line("abcdef");
        for _ in 0..3 {
            l.left();
        }
        l.kill_after();
        assert_eq!(l.as_str(), "abc");
        l.left();
        l.kill_before();
        assert_eq!((l.as_str(), l.cursor()), ("c", 0));
    }

    #[test]
    fn capacity_is_fixed() {
        let mut l = line("");
        let (ptr, cap) = (l.text.as_ptr(), l.text.capacity());
        l.insert(&"x".repeat(MAX_LINE + 10));
        assert_eq!(l.as_str().len(), MAX_LINE);
        l.insert("y");
        assert_eq!(l.as_str().len(), MAX_LINE);
        l.backspace();
        l.insert("日");
        assert_eq!(
            l.as_str().len(),
            MAX_LINE - 1,
            "a whole character or nothing"
        );
        assert_eq!((l.text.as_ptr(), l.text.capacity()), (ptr, cap));
    }

    #[test]
    fn take_and_set() {
        let mut l = line("ls work");
        let taken = l.take();
        assert_eq!((taken.as_str(), l.as_str(), l.cursor()), ("ls work", "", 0));
        l.set("help");
        assert_eq!((l.as_str(), l.cursor()), ("help", 4));
    }
}
