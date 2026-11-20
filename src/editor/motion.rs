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
use super::cells::{Cells, Step, Tab};
use super::repad::repad;
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
    pub(super) fn cells_at(&self, at: usize) -> Option<Cells> {
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
        // first or last hidden pipe.
        let at = self.in_cell(at, forward);
        self.move_to(at, select);
    }

    /// `at`, unless in live mode it is on a table's hidden pipe, padding
    /// or `|---|` line (steps TB3 to TB6): then the nearest cell's text on
    /// its line (`Cells::nearest`), else the nearest cell `forward` or
    /// back (`Cells::cell_toward`). Every move ends here, so no key leaves
    /// the cursor where the box hides it, wherever it started.
    fn in_cell(&self, at: usize, forward: bool) -> usize {
        match self.cells_at(at) {
            Some(cells) if cells.cell_at(at).is_none() => cells
                .nearest(self.text(), at)
                .or_else(|| cells.cell_toward(at, forward))
                .map_or(at, |to| self.snap(to)),
            _ => at,
        }
    }

    /// Tab and Shift+Tab in a table in live mode (step TB4): to the next
    /// or the previous cell's start (`Cells::tab`), the selection dropped;
    /// before the first cell nothing happens (from a pipe or the padding,
    /// the cursor goes into the nearest cell), so a table never gets a tab
    /// character from them. Tab's missing cells or new row (step TC1) go
    /// in, then the cursor into the first new cell; then the table is
    /// padded (step TP2). All of it is one undo step. If the edit fails,
    /// nothing changes and the cursor stays (a full note cannot get here:
    /// live mode ends at `LIVE_MAX`, far below the size limit).
    pub(super) fn tab_cell(&mut self, cells: &Cells, forward: bool) {
        match cells.tab(self.text(), self.cursor, forward) {
            Some(Tab::To(at)) => {
                let at = self.snap(at);
                self.move_to(at, false);
            }
            Some(Tab::Add { at, text, cursor }) => {
                let back = self.cursor;
                self.move_to(at, false);
                if !self.replace_selection(&text) {
                    self.move_to(back, false);
                    return;
                }
                self.collapse(cursor);
            }
            // From a pipe or the padding with no cell that way: to the
            // nearest cell, as every move ends (step TD2's property test).
            None => {
                let at = self.in_cell(self.cursor, forward);
                self.move_to(at, false);
                return;
            }
        }
        self.pad_table(self.cursor);
        self.buffer.commit();
    }

    /// Pads the live table whose lines hold `at` (`repad`, steps TP1 to
    /// TP3), the cursor kept at its offset in its cell or where it was
    /// around the table; not committed, so the caller decides the undo
    /// step. Nothing happens for a table refused or already padded, or if
    /// the padding would overfill the note.
    pub(super) fn pad_table(&mut self, at: usize) {
        if !self.live || self.text().len() > LIVE_MAX {
            return;
        }
        let Some(pad) = repad(self.text(), at) else {
            return;
        };
        let cursor = pad.map(self.cursor);
        if self.buffer.replace(pad.range.clone(), &pad.text).is_ok() {
            self.dirty = true;
            self.collapse(cursor);
        }
    }

    /// The live table the cursor is in, by its first line's start: noted
    /// before a move, to pad it if the move leaves it (step TP3).
    pub(super) fn table_here(&self) -> Option<usize> {
        self.cells_at(self.cursor).map(|cells| cells.start)
    }

    /// After a move from the table starting at `before` (`table_here`):
    /// if the cursor is out of it and selects nothing, the table is
    /// padded, its own undo step, the goal column kept. Only moves call
    /// this, never undo, so Ctrl+Z can always go back past a padding.
    pub(super) fn pad_left(&mut self, before: Option<usize>) {
        let Some(start) = before else {
            return;
        };
        if self.anchor.is_some() || self.table_here() == Some(start) {
            return;
        }
        let goal = self.goal;
        self.pad_table(start);
        self.buffer.commit();
        self.goal = goal;
    }

    /// Backspace in a live table's body row of empty cells (step TD1):
    /// deletes the row and its line break, the cursor to the end of the
    /// row above's last cell; its own undo step. False elsewhere (with a
    /// selection too), for Backspace to delete as always.
    pub(super) fn delete_empty_row(&mut self) -> bool {
        if self.selection().is_some() {
            return false;
        }
        let Some((range, cursor)) = self
            .cells_at(self.cursor)
            .and_then(|cells| cells.empty_row(self.text(), self.cursor))
        else {
            return false;
        };
        self.buffer.commit();
        if self.buffer.replace(range, "").is_ok() {
            self.dirty = true;
            self.collapse(cursor);
        }
        self.buffer.commit();
        true
    }

    /// Where a click or a drag at `at` puts the cursor (step TD2): in a
    /// live table, in a cell, as every key does (`in_cell`), never on a
    /// hidden pipe, the padding or the `|---|` line (a click on a border
    /// row lands on the line below's start, its first pipe).
    pub(super) fn clicked(&self, at: usize) -> usize {
        self.in_cell(at, true)
    }

    /// What a double click (the word) or a triple click (`line`, the
    /// line) at `at` selects. In a live table, inside the cell the click
    /// lands in (`clicked`): the word, cut to the cell's text, or the
    /// cell's whole text, as in Obsidian; never a hidden pipe or the
    /// padding, which would draw the table as written.
    pub(super) fn clicked_range(&self, at: usize, line: bool) -> Range<usize> {
        let inside = self.clicked(at);
        let cell = self
            .cells_at(inside)
            .and_then(|cells| cells.cell_at(inside).cloned());
        match (cell, line) {
            (None, false) => self.word_at(at),
            (None, true) => self.line_at(at),
            (Some(cell), true) => cell,
            (Some(cell), false) => {
                // In the padding after the text: its last word.
                let at = if inside >= cell.end && cell.start < cell.end {
                    self.buffer.prev_grapheme(cell.end)
                } else {
                    inside
                };
                let word = self.word_at(at);
                let start = word.start.max(cell.start);
                start..word.end.min(cell.end).max(start)
            }
        }
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
        let at = self.in_cell(at, end);
        self.move_to(at, select);
    }

    /// Up, Down, PgUp and PgDn: `count` lines, to the goal column (in
    /// graphemes) or the end of a shorter line. In a table's cell in live
    /// mode, Up and Down go row by row (`Cells::vertical`), the goal the
    /// offset in the cell; out of its first or last row, by line again,
    /// with that goal. A move by line that lands on a table's hidden pipe
    /// or padding (entering it, or by pages) goes on to the nearest cell
    /// on that line (`Cells::nearest`), its goal dropped, so the cursor is
    /// never where the box hides it (step TB5).
    pub(super) fn vertical(&mut self, down: bool, count: usize, select: bool) {
        if count == 1 {
            let cells = self.cells_at(self.cursor);
            let row =
                cells.and_then(|cells| cells.vertical(self.text(), self.cursor, down, self.goal));
            if let Some((at, goal)) = row {
                let at = self.snap(at);
                self.move_to(at, select);
                self.goal = Some(goal);
                return;
            }
        }
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
        let to = self.in_cell(self.cursor, down);
        if to != self.cursor {
            self.move_to(to, select);
        }
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
        // A blank line ends the table: `after` right below it would be
        // one more row.
        // Padded already (step TP3 pads a table the cursor leaves).
        let text = "before\n| ab  | c   |\n| --- | --- |\n| d   | é   |\n\nafter";
        let at = |s: &str| text.find(s).unwrap();
        let (mut ed, mut clip) = editor(text);
        ed.set_live(true);
        ed.jump_to(at("before") + 6);
        let mut seen = Vec::new();
        for _ in 0..10 {
            ed.key(Key::Right, &mut clip);
            seen.push(ed.cursor());
        }
        let (ab, c, d, e) = (at("ab"), at("c "), at("d "), at("é"));
        let after = at("\n\nafter") + 1;
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
        assert_eq!(selected(&ed), "b  | ");
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

    /// Tab and Shift+Tab in a live table (step TB4): cell to cell, row to
    /// row, Shift+Tab nothing at the first cell, and undo still takes
    /// typing in a cell back on its own. Outside a table, and not live,
    /// Tab types a tab; Shift+Tab does nothing there. (The table is
    /// padded already, so Tab only moves.)
    #[test]
    fn tabs_move_between_live_table_cells() {
        let text = "| ab  | c   |\n| --- | --- |\n| d   | e   |\n\nafter";
        let at = |s: &str| text.find(s).unwrap();
        let (mut ed, mut clip) = editor(text);
        ed.set_live(true);
        ed.jump_to(at("ab") + 1);
        let mut seen = Vec::new();
        for _ in 0..3 {
            ed.key(Key::Tab, &mut clip);
            seen.push(ed.cursor());
        }
        assert_eq!(seen, [at("c "), at("d "), at("e ")]);
        for _ in 0..4 {
            ed.key(Key::BackTab, &mut clip);
        }
        assert_eq!(ed.cursor(), at("ab"));
        assert_eq!(ed.text(), text);
        // Typing, a Tab (which pads `xab` narrower), more typing: three
        // undo steps.
        typed(&mut ed, &mut clip, "x");
        ed.key(Key::Tab, &mut clip);
        typed(&mut ed, &mut clip, "y");
        let undo = |ed: &mut super::Editor, clip: &mut crate::ui::clipboard::Clipboard| {
            ed.key(Key::Ctrl('z'), clip);
            ed.text().to_owned()
        };
        assert_eq!(
            undo(&mut ed, &mut clip),
            text.replacen("| ab  |", "| xab |", 1)
        );
        assert_eq!(undo(&mut ed, &mut clip), text.replacen("ab", "xab", 1));
        assert_eq!(undo(&mut ed, &mut clip), text);
        // Outside the table: a tab; Shift+Tab nothing.
        let (mut ed, mut clip) = editor(text);
        ed.set_live(true);
        ed.jump_to(text.len());
        ed.key(Key::BackTab, &mut clip);
        assert_eq!((ed.text(), ed.cursor()), (text, text.len()));
        ed.key(Key::Tab, &mut clip);
        assert_eq!(ed.text(), format!("{text}\t"));
        // Not live: a tab in the cell.
        let (mut ed, mut clip) = editor(text);
        ed.jump_to(at("ab"));
        ed.key(Key::Tab, &mut clip);
        assert!(ed.text().starts_with("| \tab"));
    }

    /// As in Obsidian (step TC1): Tab in the last cell adds a row of
    /// empty cells, the cursor after the first one's first space so
    /// typing reads `| q |`; in a short row's last cell it adds the
    /// missing cells first. Each Tab is one undo step with its padding.
    #[test]
    fn tab_in_the_last_cell_adds_a_row() {
        let text = "| ab  | c   |\n| --- | --- |\n| d   | e   |\n\nafter";
        let end = text.find("\n\nafter").unwrap();
        let (mut ed, mut clip) = editor(text);
        ed.set_live(true);
        ed.jump_to(text.find("e ").unwrap());
        ed.key(Key::Tab, &mut clip);
        let added = [&text[..end], "\n|     |     |", &text[end..]].concat();
        assert_eq!((ed.text(), ed.cursor()), (added.as_str(), end + 3));
        typed(&mut ed, &mut clip, "q");
        ed.key(Key::Tab, &mut clip);
        typed(&mut ed, &mut clip, "r");
        assert!(ed.text().contains("\n| q   | r"), "{:?}", ed.text());
        for _ in 0..4 {
            ed.key(Key::Ctrl('z'), &mut clip);
        }
        assert_eq!(ed.text(), text);

        // A short row, without its closing pipe: filled, then a new row.
        let text = "| a   | b   |\n| --- | --- |\n| d\n\nafter";
        let (mut ed, mut clip) = editor(text);
        ed.set_live(true);
        ed.jump_to(text.find('d').unwrap() + 1);
        ed.key(Key::Tab, &mut clip);
        let filled = text.replacen("| d\n", "| d   |     |\n", 1);
        assert_eq!(ed.text(), filled);
        assert_eq!(ed.cursor(), filled.find("| d   | ").unwrap() + 8);
        ed.key(Key::Tab, &mut clip);
        let added = filled.replacen("|     |\n", "|     |\n|     |     |\n", 1);
        assert_eq!(ed.text(), added);
        ed.key(Key::Ctrl('z'), &mut clip);
        assert_eq!(ed.text(), filled);
        ed.key(Key::Ctrl('z'), &mut clip);
        assert_eq!(ed.text(), text);
    }

    /// Leaving a table by a move pads it (step TP3), its own undo step,
    /// the cursor where the move put it; moves inside, Shift+arrows out,
    /// and undo do not pad. A click out pads too; a click in does not.
    #[test]
    fn leaving_a_table_pads_it() {
        use crate::shell::keys::MouseEvent;
        let text = "|a|bb|\n|-|-|\n|c|d|\n\nafter";
        let padded = "| a   | bb  |\n| --- | --- |\n| c   | d   |\n\nafter";
        let blank = |t: &str| t.find("\n\nafter").unwrap() + 1;
        let (mut ed, mut clip) = editor(text);
        ed.set_live(true);
        ed.jump_to(1);
        keys(&mut ed, &mut clip, [Key::Right, Key::Right, Key::Down]);
        assert_eq!(ed.text(), text, "moves inside: as written");
        ed.key(Key::Down, &mut clip);
        assert_eq!((ed.text(), ed.cursor()), (padded, blank(padded)));
        ed.key(Key::Ctrl('z'), &mut clip);
        assert_eq!(ed.text(), text, "undo: as written, and not padded again");

        // Selecting out of the table: as written.
        let (mut ed, mut clip) = editor(text);
        ed.set_live(true);
        ed.jump_to(text.find('c').unwrap());
        ed.key(Key::ShiftDown, &mut clip);
        assert_eq!((ed.text(), ed.cursor()), (text, blank(text)));

        // A click: in the table nothing, out of it padded.
        let (mut ed, _) = editor(text);
        ed.set_live(true);
        let now = std::time::Instant::now();
        ed.mouse(MouseEvent::Press, Some(1), now);
        ed.mouse(MouseEvent::Press, Some(text.find('d').unwrap()), now);
        assert_eq!(ed.text(), text);
        ed.mouse(MouseEvent::Press, Some(text.len()), now);
        assert_eq!((ed.text(), ed.cursor()), (padded, padded.len()));
    }

    /// Backspace in an empty body row deletes it (step TD1), the cursor at
    /// the end of the row above, one undo step; in a row with text, or
    /// not live, it deletes one character as always.
    #[test]
    fn backspace_deletes_an_empty_row() {
        let text = "| ab  | c   |\n| --- | --- |\n| d   | e   |\n|     |     |\n\nafter";
        let (mut ed, mut clip) = editor(text);
        ed.set_live(true);
        let empty = text.rfind("|     |").unwrap() + 2;
        ed.jump_to(empty);
        ed.key(Key::Backspace, &mut clip);
        let gone = text.replacen("\n|     |     |", "", 1);
        assert_eq!(
            (ed.text(), ed.cursor()),
            (gone.as_str(), text.find("e ").unwrap() + 1)
        );
        ed.key(Key::Ctrl('z'), &mut clip);
        assert_eq!(ed.text(), text);
        // A row with text: one character.
        ed.jump_to(text.find("e ").unwrap() + 1);
        ed.key(Key::Backspace, &mut clip);
        assert_eq!(ed.text(), text.replacen("| e ", "|  ", 1));
        // Not live: one character.
        let (mut ed, mut clip) = editor(text);
        ed.jump_to(empty);
        ed.key(Key::Backspace, &mut clip);
        assert_eq!(ed.text().len(), text.len() - 1);
    }

    /// Clicks and drags in a live table land in a cell (step TD2): on a
    /// row's first pipe (where a click on the border row above lands), at
    /// the nearest cell; on the `|---|` line, at the next cell.
    #[test]
    fn clicks_land_in_cells() {
        use crate::shell::keys::MouseEvent;
        let text = "| ab  | c   |\n| --- | --- |\n| d   | e   |\n\nafter";
        let at = |s: &str| text.find(s).unwrap();
        let (mut ed, _) = editor(text);
        ed.set_live(true);
        let now = std::time::Instant::now();
        ed.mouse(MouseEvent::Press, Some(at("| d")), now);
        assert_eq!(ed.cursor(), at("d "));
        ed.mouse(MouseEvent::Press, Some(at("| ---")), now);
        assert_eq!(ed.cursor(), at("d "));
        ed.mouse(MouseEvent::Press, Some(at("ab")), now);
        ed.mouse(MouseEvent::Drag, Some(at("| d")), now);
        assert_eq!(
            (ed.selection(), ed.cursor()),
            (Some(at("ab")..at("d ")), at("d "))
        );
        assert_eq!(ed.text(), text);
    }

    /// Double and triple clicks in a live table select inside the cell
    /// the click lands in: the word, or the cell's text; never a pipe or
    /// the padding, so the table stays boxed. A double click on a border
    /// row lands on the line below, as a single click does. Outside the
    /// table, words and lines as before.
    #[test]
    fn double_and_triple_clicks_stay_in_cells() {
        use crate::editor::tests::selected;
        use crate::shell::keys::MouseEvent;
        let text = "| ab cd | x   |\n| ----- | --- |\n| e     | f   |\n\nafter";
        let at = |s: &str| text.find(s).unwrap();
        let (mut ed, _) = editor(text);
        ed.set_live(true);
        let t0 = std::time::Instant::now();
        let mut t = 0;
        let mut clicks = |ed: &mut crate::editor::Editor, place: usize, n: u64| {
            t += 5_000;
            for i in 0..n {
                let now = t0 + std::time::Duration::from_millis(t + i);
                ed.mouse(MouseEvent::Press, Some(place), now);
            }
        };
        let in_cells = |ed: &crate::editor::Editor| {
            // In the cell's text, not its padding (drawn as written).
            let cursor = ed.cursor();
            let cells = ed.cells_at(cursor).unwrap();
            cells
                .cell_at(cursor)
                .is_some_and(|cell| cell.start <= cursor && cursor <= cell.end)
        };
        for (place, n, want) in [
            (at("| e"), 2, "e"),
            (at("| ---"), 2, "e"),
            (at("cd"), 2, "cd"),
            (at("x") + 2, 2, "x"),
            (at("| x") + 1, 2, "x"),
            (at("ab"), 3, "ab cd"),
            (at("| e"), 3, "e"),
        ] {
            clicks(&mut ed, place, n);
            assert_eq!(selected(&ed), want, "{n} clicks at {place}");
            assert!(in_cells(&ed), "{n} clicks at {place}");
        }
        clicks(&mut ed, at("after") + 1, 2);
        assert_eq!(selected(&ed), "after");
        clicks(&mut ed, at("after") + 1, 3);
        assert_eq!(selected(&ed), "after");
        assert_eq!(ed.text(), text);
    }

    /// Tab pads the table (step TP2): pipes lined up, the cursor in the
    /// cell Tab went to; one Ctrl+Z gives the table back as written.
    /// Not live: Tab types a tab, nothing is padded.
    #[test]
    fn tab_pads_the_table() {
        let text = "|a|bb|\n|-|-|\n|c|d|\n";
        let padded = "| a   | bb  |\n| --- | --- |\n| c   | d   |\n";
        let (mut ed, mut clip) = editor(text);
        ed.set_live(true);
        ed.jump_to(1);
        ed.key(Key::Tab, &mut clip);
        assert_eq!(
            (ed.text(), ed.cursor()),
            (padded, padded.find("bb").unwrap())
        );
        ed.key(Key::Ctrl('z'), &mut clip);
        assert_eq!(ed.text(), text);
        let (mut ed, mut clip) = editor(text);
        ed.jump_to(1);
        ed.key(Key::Tab, &mut clip);
        assert_eq!(ed.text(), "|\ta|bb|\n|-|-|\n|c|d|\n");
    }

    /// Up and Down in a live table (step TB5): the same column's cell a row
    /// away (a short row's last cell when it has no such column), the
    /// offset kept across a short cell; out of the first and last
    /// rows by line, with that offset as the goal column; entering from a
    /// line around, or by a page, at the nearest cell, never on a pipe.
    /// Shift selects. Not live: by line, as always.
    #[test]
    fn up_and_down_move_through_live_table_rows() {
        let text = "before\n| ab | cde |\n|---|---|\n| f | g |\n| hij |\n\nafter";
        let at = |s: &str| text.find(s).unwrap();
        let (c, f, g, h) = (at("cde"), at("f |"), at("g |"), at("hij"));
        let (mut ed, mut clip) = editor(text);
        ed.set_live(true);
        ed.jump_to(c + 2);
        let mut seen = Vec::new();
        for key in [Key::Down, Key::Down, Key::Up, Key::Up, Key::Up] {
            ed.key(key, &mut clip);
            seen.push(ed.cursor());
        }
        // The short row's only cell is column 0, so Up from it goes to
        // `f`, then `ab`; the offset 2 is kept throughout.
        assert_eq!(seen, [g + 1, h + 2, f + 1, at("ab") + 2, 2]);
        // Leaving padded the table, its short row filled (step TP3): the
        // rest runs on the padded text.
        let text = "before\n| ab  | cde |\n| --- | --- |\n| f   | g   |\n| hij |     |\n\nafter";
        assert_eq!(ed.text(), text);
        let at = |s: &str| text.find(s).unwrap();
        let (f, h) = (at("f "), at("hij"));
        // Entering from above at column 0: the first cell, not the pipe.
        ed.jump_to(0);
        ed.key(Key::Down, &mut clip);
        assert_eq!(ed.cursor(), at("ab"));
        // Out of the last row, and back in from below.
        ed.jump_to(h + 1);
        ed.key(Key::Down, &mut clip);
        assert_eq!(ed.cursor(), at("\n\nafter") + 1);
        // Down and back up: the line's goal column brings it back.
        ed.key(Key::Down, &mut clip);
        ed.key(Key::Up, &mut clip);
        ed.key(Key::Up, &mut clip);
        assert_eq!(ed.cursor(), h + 1);
        // From below at column 0: on a pipe, so on to the nearest cell.
        ed.jump_to(at("after"));
        ed.key(Key::Up, &mut clip);
        ed.key(Key::Up, &mut clip);
        assert_eq!(ed.cursor(), h);
        // Shift selects across rows.
        ed.jump_to(at("ab"));
        ed.key(Key::ShiftDown, &mut clip);
        assert_eq!(selected(&ed), &text[at("ab")..f]);
        // A page landing in the table: in a cell.
        ed.set_page(2);
        ed.jump_to(0);
        ed.key(Key::PageDown, &mut clip);
        assert!(ed.cursor() == f, "{}", ed.cursor());
        // Not live: the line's start, a pipe.
        let (mut ed, mut clip) = editor(text);
        ed.key(Key::Down, &mut clip);
        assert_eq!(ed.cursor(), at("| ab"));
    }
}
