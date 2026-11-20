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
use super::cells::{Cells, Step};
use super::view::LIVE_MAX;

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

    /// The table whose lines hold `at`, in live mode (step TB3), as the
    /// view boxes it: none above `LIVE_MAX`, where the view draws the
    /// text as written.
    fn cells_at(&self, at: usize) -> Option<Cells> {
        if !self.live || self.text().len() > LIVE_MAX {
            return None;
        }
        Cells::around(self.text(), at)
    }

    /// Left and Right: one grapheme; in a table in live mode, through its
    /// cells' text (`Cells::step`), never onto a hidden pipe or the padding.
    /// Out of the table's first or last cell, the line above's end or the
    /// line below's start; at the text's start or end, it stays.
    pub(super) fn horizontal(&mut self, forward: bool, select: bool) {
        let grapheme = |ed: &Self, at: usize| {
            if forward {
                ed.buffer.next_grapheme(at)
            } else {
                ed.buffer.prev_grapheme(at)
            }
        };
        let at = match self.cells_at(self.cursor) {
            None => grapheme(self, self.cursor),
            Some(cells) => match cells.step(self.cursor, forward) {
                Step::Grapheme => grapheme(self, self.cursor),
                Step::To(at) => self.snap(at),
                Step::Leave if forward && cells.end < self.text().len() => {
                    grapheme(self, cells.end)
                }
                Step::Leave if !forward && cells.start > 0 => grapheme(self, cells.start),
                Step::Leave => self.cursor,
            },
        };
        // Entering a table from the line above or below lands on its
        // first or last hidden pipe: on to the nearest cell that way.
        let at = match self.cells_at(at) {
            Some(cells) if cells.cell_at(at).is_none() => match cells.step(at, forward) {
                Step::To(cell) => self.snap(cell),
                _ => at,
            },
            _ => at,
        };
        self.move_to(at, select);
    }

    /// Home and End: the line's start or end; in a table's cell in live
    /// mode, the cell's (step TB3), as the line's own are hidden pipes.
    pub(super) fn home_end(&mut self, end: bool, select: bool) {
        let cell = self
            .cells_at(self.cursor)
            .and_then(|cells| cells.cell_at(self.cursor).cloned());
        let at = match (cell, end) {
            (Some(cell), false) => self.snap(cell.start),
            (Some(cell), true) => self.snap(cell.end),
            (None, false) => self.line_start(self.cursor),
            (None, true) => self.line_end(self.cursor),
        };
        self.move_to(at, select);
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

    /// Live tables (step TB3): Right and Left move through the cells'
    /// text, cell to cell at their edges, never onto a pipe or padding,
    /// entering from the lines around at the nearest cell and leaving at
    /// the first and last; Home and End stay in the cell; Shift selects
    /// across cells. Not live: one grapheme, as always.
    #[test]
    fn arrows_move_through_live_table_cells() {
        let text = "before\n| ab | c |\n|---|---|\n| d | é |\nafter";
        let at = |s: &str| text.find(s).unwrap();
        let (mut ed, mut clip) = editor(text);
        ed.set_live(true);
        ed.jump_to(at("before") + 6);
        let mut seen = Vec::new();
        for _ in 0..10 {
            ed.key(Key::Right, &mut clip);
            seen.push(ed.cursor());
        }
        let (ab, c, d, e) = (at("ab"), at("c |"), at("d |"), at("é"));
        let after = at("after");
        let way = [ab, ab + 1, ab + 2, c, c + 1, d, d + 1, e, e + 2, after];
        assert_eq!(seen, way);
        let mut back = Vec::new();
        for _ in 0..10 {
            ed.key(Key::Left, &mut clip);
            back.push(ed.cursor());
        }
        let mut way_back: Vec<usize> = way[..9].iter().rev().copied().collect();
        way_back.push(at("before") + 6);
        assert_eq!(back, way_back);
        // Home and End: the cell's start and end.
        ed.jump_to(ab + 1);
        ed.key(Key::End, &mut clip);
        assert_eq!(ed.cursor(), ab + 2);
        ed.key(Key::Home, &mut clip);
        assert_eq!(ed.cursor(), ab);
        // Shift selects across cells.
        ed.jump_to(ab + 1);
        ed.key(Key::ShiftRight, &mut clip);
        ed.key(Key::ShiftRight, &mut clip);
        assert_eq!(selected(&ed), "b | ");
        // Not live: graphemes and lines, pipes included.
        let (mut ed, mut clip) = editor(text);
        ed.jump_to(ab + 2);
        ed.key(Key::Right, &mut clip);
        assert_eq!(ed.cursor(), ab + 3);
        ed.key(Key::Home, &mut clip);
        assert_eq!(ed.cursor(), at("| ab"));
    }

    /// A table at the text's start or end: Left in its first cell and
    /// Right in its last stay there.
    #[test]
    fn arrows_stay_at_a_live_table_at_the_texts_ends() {
        let text = "| a | b |\n|---|---|";
        let (mut ed, mut clip) = editor(text);
        ed.set_live(true);
        ed.jump_to(2);
        ed.key(Key::Left, &mut clip);
        assert_eq!(ed.cursor(), 2);
        ed.jump_to(7);
        ed.key(Key::Right, &mut clip);
        assert_eq!(ed.cursor(), 7);
    }
}
