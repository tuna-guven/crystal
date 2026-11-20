//! Where the editor's cursor and selection go (its own file since step
//! RF6): the moves of the keys and the mouse, by grapheme, word, line and
//! page, and the snapping that keeps the cursor on a grapheme boundary.
//!
//! The rule all of it keeps: the cursor and the selection's anchor are
//! always on a grapheme boundary of the text (`snap` after any edit), so
//! no edit splits a character and the view always has a cell to draw the
//! cursor in. A `\r\n` is one grapheme: the cursor stops before it,
//! never between its bytes. Up and Down keep a goal column in graphemes
//! (`goal`), dropped by any other move.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use super::Editor;

impl Editor {
    /// Puts the cursor at `at` with nothing selected. After an edit, `at`
    /// can be inside a grapheme (a letter typed before a combining accent
    /// joins it); the cursor then goes to that grapheme's end.
    pub(super) fn collapse(&mut self, at: usize) {
        self.cursor = self.snap(at);
        self.anchor = None;
        self.goal = None;
    }

    /// `at` if it is on a grapheme boundary, else the end of the grapheme
    /// it is inside.
    pub(super) fn snap(&self, at: usize) -> usize {
        if at == 0 {
            0
        } else {
            let start = self.buffer.prev_grapheme(at);
            self.buffer.next_grapheme(start).max(at)
        }
    }

    /// Moves the cursor; with Shift the selection grows from where it
    /// started, without it the selection is dropped.
    pub(super) fn move_to(&mut self, at: usize, select: bool) {
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = at;
        self.goal = None;
    }

    /// Puts the cursor at `at` (on a grapheme's start), the selection
    /// dropped: where a link followed in live mode leads (step LF3).
    pub fn jump_to(&mut self, at: usize) {
        let at = self.snap(at.min(self.text().len()));
        self.move_to(at, false);
    }

    /// Selects `range`, the cursor at its end; an empty one only moves
    /// the cursor.
    pub(super) fn select(&mut self, range: Range<usize>) {
        let (start, end) = (self.snap(range.start), self.snap(range.end));
        self.move_to(start, false);
        if end > start {
            self.move_to(end, true);
        }
    }

    /// The word around `at` in its line, by Unicode word boundaries (a
    /// run of spaces or one punctuation mark counts as a word too); empty
    /// at the line's end.
    pub(super) fn word_at(&self, at: usize) -> Range<usize> {
        let (start, end) = (self.line_start(at), self.line_end(at));
        self.text()
            .get(start..end)
            .unwrap_or("")
            .split_word_bound_indices()
            .map(|(i, word)| start + i..start + i + word.len())
            .find(|word| word.contains(&at))
            .unwrap_or(at..at)
    }

    /// The line around `at`, with its line break if it has one.
    pub(super) fn line_at(&self, at: usize) -> Range<usize> {
        let end = self.line_end(at);
        let end = if end < self.text().len() {
            self.buffer.next_grapheme(end)
        } else {
            end
        };
        self.line_start(at)..end
    }

    pub(super) fn line_start(&self, at: usize) -> usize {
        self.text()
            .get(..at)
            .and_then(|head| head.rfind('\n'))
            .map_or(0, |i| i + 1)
    }

    /// Where the line's break is: its `\n`, or the `\r` of a `\r\n` (one
    /// grapheme, which the cursor never splits); or the end of the text.
    pub(super) fn line_end(&self, at: usize) -> usize {
        let text = self.text();
        match text.get(at..).and_then(|tail| tail.find('\n')) {
            Some(i) if text[..at + i].ends_with('\r') && i > 0 => at + i - 1,
            Some(i) => at + i,
            None => text.len(),
        }
    }

    /// Up, Down, PgUp and PgDn: `count` lines, to the goal column (in
    /// graphemes) or the end of a shorter line.
    pub(super) fn vertical(&mut self, down: bool, count: usize, select: bool) {
        let start = self.line_start(self.cursor);
        let goal = self.goal.unwrap_or_else(|| {
            self.text()
                .get(start..self.cursor)
                .map_or(0, |part| part.graphemes(true).count())
        });
        let mut at = self.cursor;
        for _ in 0..count {
            if down {
                let end = self.line_end(at);
                if end >= self.text().len() {
                    break;
                }
                // Past the whole line break, `\n` or `\r\n`.
                at = self.buffer.next_grapheme(end);
            } else {
                let line = self.line_start(at);
                if line == 0 {
                    break;
                }
                // The previous line's start: `line - 1` itself can be the
                // `\n` inside a `\r\n`.
                at = self.line_start(line - 1);
            }
        }
        let line = self.line_start(at);
        let text = self.text().get(line..self.line_end(at)).unwrap_or("");
        let offset: usize = text.graphemes(true).take(goal).map(str::len).sum();
        self.move_to(line + offset, select);
        self.goal = Some(goal);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{editor, keys, selected, typed};
    use crate::shell::keys::Key;

    #[test]
    fn shift_selects_and_typing_replaces_the_selection() {
        let (mut ed, mut clip) = editor("one two");
        keys(
            &mut ed,
            &mut clip,
            [Key::ShiftRight, Key::ShiftRight, Key::ShiftRight],
        );
        assert_eq!(selected(&ed), "one");
        typed(&mut ed, &mut clip, "1");
        assert_eq!(ed.text(), "1 two");
        keys(&mut ed, &mut clip, [Key::ShiftEnd]);
        assert_eq!(selected(&ed), " two");
        ed.key(Key::Backspace, &mut clip);
        assert_eq!(ed.text(), "1");
        // A movement without Shift drops the selection.
        keys(&mut ed, &mut clip, [Key::ShiftHome, Key::Right]);
        assert_eq!(ed.selection(), None);
    }

    #[test]
    fn moves_by_lines_and_graphemes() {
        let (mut ed, mut clip) = editor("abcd\nx\nwxyz");
        keys(&mut ed, &mut clip, [Key::End, Key::Down]);
        assert_eq!(ed.cursor(), 6, "the end of the shorter line");
        ed.key(Key::Down, &mut clip);
        assert_eq!(ed.cursor(), 11, "the goal column comes back");
        keys(&mut ed, &mut clip, [Key::Home, Key::Up, Key::Up]);
        assert_eq!(ed.cursor(), 0);
        ed.key(Key::Up, &mut clip);
        assert_eq!(ed.cursor(), 0, "the first line stays");

        let (mut ed, mut clip) = editor("e\u{301}x");
        ed.key(Key::Right, &mut clip);
        assert_eq!(ed.cursor(), 3, "e and its accent are one step");
        ed.key(Key::Backspace, &mut clip);
        assert_eq!(ed.text(), "x");
    }

    #[test]
    fn typing_before_a_combining_accent_keeps_the_cursor_on_a_grapheme() {
        let (mut ed, mut clip) = editor("\u{301}");
        typed(&mut ed, &mut clip, "e");
        assert_eq!(ed.text(), "e\u{301}");
        assert_eq!(ed.cursor(), 3, "after the joined grapheme, not inside it");
    }

    #[test]
    fn lines_end_before_a_windows_line_break() {
        let (mut ed, mut clip) = editor("ab\r\ncd");
        keys(&mut ed, &mut clip, [Key::Down, Key::End, Key::Up]);
        assert_eq!(ed.cursor(), 2, "before the \\r\\n, not between them");
    }

    #[test]
    fn pages_move_by_the_view_height() {
        let (mut ed, mut clip) = editor("0\n1\n2\n3\n4\n5");
        ed.set_page(2);
        ed.key(Key::PageDown, &mut clip);
        assert_eq!(ed.cursor(), 4);
        keys(&mut ed, &mut clip, [Key::PageDown, Key::PageDown]);
        assert_eq!(ed.cursor(), 10, "stops on the last line");
        ed.key(Key::PageUp, &mut clip);
        assert_eq!(ed.cursor(), 6);
    }
}
