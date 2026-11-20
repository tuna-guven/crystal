//! The table cells around the editor's cursor in live mode (steps TB2 to
//! TB5): where the cursor goes in a table, so it moves through its cells'
//! text and never onto the hidden pipes or the padding between them.
//!
//! Pure: the text and a position in, positions out. The cells come from
//! the same parse the view draws from (`live::parse`, `Layout::tables`),
//! made again on a key, never kept, so nothing has to follow edits; the
//! parse runs only when the cursor's line holds a `|`, so other keys cost
//! nothing. Every cell the layout records is a place of its row (a short
//! row's missing cells are left out there). The `|---|` line has no
//! cells. A body row the parser continues without any `|`
//! (a lazy row) is not found from its own line: keys move there as on
//! any line.
//!
//! The editor applies what these functions say only in live mode (the
//! view boxes tables there), and still snaps every position to a grapheme
//! boundary; outside a table they say nothing and keys work as always.
//! Only Tab edits (steps TC1, TC2): the text it adds is pipes, spaces and
//! a line break, after a copy of the last line's leading `>`s, spaces
//! and tabs, never a cell's text.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use super::live::{self, cell_reach, line_end};

/// Where Left or Right goes from a position in a table (`Cells::step`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// One grapheme, as anywhere: the cursor stays inside its cell.
    Grapheme,
    /// To this position: a cell's start (going right) or end (left).
    To(usize),
    /// Out of the table: past its last line's end, or before its first
    /// line's start.
    Leave,
}

/// What Tab does in a table (`Cells::tab`, steps TB4 and TC1).
#[cfg_attr(test, derive(Debug))]
#[derive(PartialEq, Eq)]
pub enum Tab {
    /// Moves the cursor to this position: a cell's start.
    To(usize),
    /// Inserts `text` at `at` (empty cells, or a new row), then puts the
    /// cursor at `cursor`: the first new cell, in the edited text.
    Add {
        at: usize,
        text: String,
        cursor: usize,
    },
}

/// One cell: its text, without the spaces around it, whose ends are the
/// edges keys stop at, and how far the cursor still counts as in it
/// (`live::cell_reach`: a space typed at its end).
struct Cell {
    text: Range<usize>,
    reach: usize,
}

impl Cell {
    fn holds(&self, at: usize) -> bool {
        self.text.start <= at && at <= self.reach
    }
}

/// A table's cells, row by row (the header first), each in its line.
pub struct Cells {
    rows: Vec<Vec<Cell>>,
    /// Each row's line end, before its line break (as `rows`).
    ends: Vec<usize>,
    /// The table's columns: the header's cells.
    columns: usize,
    /// The table's first line's start and its last line's end, before
    /// its line break.
    pub start: usize,
    pub end: usize,
}

impl Cells {
    /// The table whose lines hold `at`, if `at`'s line holds a `|` and the
    /// parse finds a table there.
    pub fn around(text: &str, at: usize) -> Option<Self> {
        let at = at.min(text.len());
        let start = text.get(..at)?.rfind('\n').map_or(0, |i| i + 1);
        if !text[start..line_end(text, start)].contains('|') {
            return None;
        }
        let layout = live::parse(text).layout;
        let line_start = |i: usize| layout.lines.get(i).map_or(text.len(), |l| l.markup.start);
        layout.tables.iter().find_map(|table| {
            let first = line_start(table.lines.start);
            let end = line_end(text, line_start(table.lines.end.checked_sub(1)?));
            (first..=end).contains(&at).then(|| Self {
                ends: table
                    .rows
                    .iter()
                    .map(|&(line, _)| line_end(text, line_start(line)))
                    .collect(),
                columns: table.aligns.len(),
                rows: table
                    .rows
                    .iter()
                    .map(|(_, cells)| {
                        let cell = |range: &Range<usize>| Cell {
                            text: range.clone(),
                            reach: cell_reach(text, range),
                        };
                        cells.iter().map(cell).collect()
                    })
                    .collect(),
                start: first,
                end,
            })
        })
    }

    /// Every cell's text, in the text's order.
    fn all(&self) -> impl DoubleEndedIterator<Item = &Range<usize>> {
        self.rows.iter().flatten().map(|cell| &cell.text)
    }

    /// The text of the cell holding `at`: from its text's start to its
    /// reach (the spaces after it).
    pub fn cell_at(&self, at: usize) -> Option<&Range<usize>> {
        self.rows
            .iter()
            .flatten()
            .find(|cell| cell.holds(at))
            .map(|cell| &cell.text)
    }

    /// Where Left (`forward` false) or Right goes from `at`: inside a
    /// cell, by grapheme; at its edge, to the next cell's start or the
    /// previous cell's end; from a pipe, the padding or the `|---|` line,
    /// to the nearest cell that way; past the first or the last cell, out
    /// of the table.
    pub fn step(&self, at: usize, forward: bool) -> Step {
        if let Some(cell) = self.cell_at(at) {
            let inside = if forward {
                at < cell.end
            } else {
                at > cell.start
            };
            if inside {
                return Step::Grapheme;
            }
        }
        let next = if forward {
            self.all()
                .find(|cell| cell.start > at)
                .map(|cell| cell.start)
        } else {
            self.all()
                .rev()
                .find(|cell| cell.end < at)
                .map(|cell| cell.end)
        };
        next.map_or(Step::Leave, Step::To)
    }

    /// For `at` on a pipe, the padding or the `|---|` line: the nearest
    /// cell's edge that way (`step`), else the other way. Every table has
    /// a header with a cell, so there is one; `None` only for a table of
    /// no cells at all.
    pub fn cell_toward(&self, at: usize, forward: bool) -> Option<usize> {
        let way = |forward| match self.step(at, forward) {
            Step::To(to) => Some(to),
            _ => None,
        };
        way(forward).or_else(|| way(!forward))
    }

    /// Where Up (`down` false) or Down goes from `at` in a cell (step
    /// TB5): the same column's cell in the row above or below (that row's
    /// last cell if it is shorter), `goal` graphemes into its text, or as
    /// many as `at` is into its own, clamped to the text; with that goal,
    /// kept for the next row. `None` outside a cell, from the first row
    /// up or the last row down: the editor moves by line then, out of the
    /// table.
    pub fn vertical(
        &self,
        text: &str,
        at: usize,
        down: bool,
        goal: Option<usize>,
    ) -> Option<(usize, usize)> {
        let (r, c) = self.rows.iter().enumerate().find_map(|(r, row)| {
            let c = row.iter().position(|cell| cell.holds(at))?;
            Some((r, c))
        })?;
        let cell = &self.rows[r][c].text;
        let goal = goal.unwrap_or_else(|| {
            text.get(cell.start..at)
                .map_or(0, |s| s.graphemes(true).count())
        });
        let row = self
            .rows
            .get(if down { r + 1 } else { r.checked_sub(1)? })?;
        let target = &row.get(c).or(row.last())?.text;
        let offset: usize = text
            .get(target.clone())?
            .graphemes(true)
            .take(goal)
            .map(str::len)
            .sum();
        Some((target.start + offset, goal))
    }

    /// The position in a cell's text nearest `at` on `at`'s line, for a
    /// cursor that landed on a hidden pipe or the padding there (step
    /// TB5): `at` itself if a cell holds it, else the nearest cell's start
    /// or end. `None` on a line without cells (the `|---|` line).
    pub fn nearest(&self, text: &str, at: usize) -> Option<usize> {
        let start = text.get(..at)?.rfind('\n').map_or(0, |i| i + 1);
        let end = line_end(text, start);
        self.all()
            .filter(|cell| start <= cell.start && cell.end <= end)
            .map(|cell| at.clamp(cell.start, cell.end))
            .min_by_key(|&to| to.abs_diff(at))
    }

    /// Where Tab (`forward`) or Shift+Tab goes from `at` (step TB4): the
    /// next or the previous cell's start, from a row's last cell to the
    /// next row's first and back; from a pipe, the padding or the `|---|`
    /// line, the nearest cell's start that way. As in Obsidian (step
    /// TC1), Tab in a short row's last cell adds its missing cells, and
    /// in the table's last cell a row; `None` from the first cell back,
    /// and from a pipe or the padding past the last cell.
    pub fn tab(&self, text: &str, at: usize, forward: bool) -> Option<Tab> {
        let here = self.rows.iter().enumerate().find_map(|(r, row)| {
            let c = row.iter().position(|cell| cell.holds(at))?;
            Some((r, c))
        });
        if let (Some((r, c)), true) = (here, forward) {
            let row = &self.rows[r];
            if c + 1 == row.len() && row.len() < self.columns {
                return Some(self.fill(text, r));
            }
        }
        let i = here.map(|(r, c)| self.rows[..r].iter().map(Vec::len).sum::<usize>() + c);
        let cell = match (i, forward) {
            (Some(i), true) => self.all().nth(i + 1),
            (Some(i), false) => self.all().nth(i.checked_sub(1)?),
            (None, true) => self.all().find(|cell| cell.start > at),
            (None, false) => self.all().rev().find(|cell| cell.end < at),
        };
        match (cell, i, forward) {
            (Some(cell), ..) => Some(Tab::To(cell.start)),
            (None, Some(_), true) => Some(self.new_row(text)),
            _ => None,
        }
    }

    /// Row `r`'s missing cells, `  |` each, after its closing `|`, or at
    /// its line's end after the closing `|` it lacks; the cursor in the first of them,
    /// between its spaces (`layout::trimmed`).
    fn fill(&self, text: &str, r: usize) -> Tab {
        let end = self.ends[r];
        let after = self.rows[r].last().map_or(end, |cell| cell.reach);
        // Right after the closing pipe, if the row has one (spaces after
        // it stay at the line's end), else at the line's end.
        let pipe = text.get(after..end).and_then(|tail| tail.rfind('|'));
        let closed = pipe.is_some();
        let at = pipe.map_or(end, |i| after + i + 1);
        let mut add = String::from(if closed { "" } else { " |" });
        let cursor = at + add.len() + 1;
        add.push_str(&"  |".repeat(self.columns - self.rows[r].len()));
        Tab::Add {
            at,
            text: add,
            cursor,
        }
    }

    /// A row of empty cells after the table's last line, behind a copy of
    /// that line's leading `>`s, spaces and tabs (a quote's or a list's
    /// markup) so it stays in the same container; the cursor in its
    /// first cell.
    fn new_row(&self, text: &str) -> Tab {
        let last = text
            .get(..self.end)
            .map_or(0, |s| s.rfind('\n').map_or(0, |i| i + 1));
        let lead = text.get(last..self.end).unwrap_or("");
        let lead = &lead[..lead.len() - lead.trim_start_matches([' ', '\t', '>']).len()];
        let mut add = String::from("\n");
        add.push_str(lead);
        add.push('|');
        let cursor = self.end + add.len() + 1;
        add.push_str(&"  |".repeat(self.columns));
        Tab::Add {
            at: self.end,
            text: add,
            cursor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cells as text, row by row.
    fn cells(text: &str, at: usize) -> Vec<Vec<&str>> {
        let found = Cells::around(text, at).unwrap();
        found
            .rows
            .iter()
            .map(|row| row.iter().map(|cell| &text[cell.text.clone()]).collect())
            .collect()
    }

    /// Header and body rows in order, each cell's text without its
    /// spaces, an escaped `\|` inside a cell, a short row without its
    /// missing cell, the `|---|` line without cells; found from any line.
    /// A blank line ends the table (a line right after it would be one
    /// more row).
    #[test]
    fn cells_of_a_table_row_by_row() {
        let text = "| a | b \\| c |\n|---|---|\n| d |\n| e | f |\n\nafter |";
        for line in ["| a", "|---", "| d", "| e"] {
            let at = text.find(line).unwrap();
            assert_eq!(
                cells(text, at),
                [vec!["a", "b \\| c"], vec!["d"], vec!["e", "f"]],
                "{line}"
            );
        }
        let found = Cells::around(text, 0).unwrap();
        assert_eq!(
            (found.start, found.end),
            (0, text.find("\n\nafter").unwrap())
        );
        // A `|` outside a table, a line without one, not a table at all.
        assert!(Cells::around(text, text.len()).is_none());
        assert!(Cells::around("a | b\nc", 0).is_none());
        assert!(Cells::around("| a | b |\n|---|---|\n\nplain", 22).is_none());
    }

    /// Tab and Shift+Tab: every cell's start in order, row to row, and
    /// back; nothing before the first cell; from pipes and the `|---|`
    /// line the nearest cell that way.
    #[test]
    fn tabs_go_cell_to_cell() {
        let text = "| ab | c |\n|---|---|\n| d |  |\n| f | g |";
        let found = Cells::around(text, 0).unwrap();
        let at = |s: &str| text.find(s).unwrap();
        let tab = |at, forward| found.tab(text, at, forward);
        let empty = found.rows[1][1].text.start;
        let starts = [at("ab"), at("c |"), at("d |"), empty, at("f |"), at("g |")];
        for pair in starts.windows(2) {
            assert_eq!(tab(pair[0], true), Some(Tab::To(pair[1])));
            assert_eq!(tab(pair[1], false), Some(Tab::To(pair[0])));
        }
        // From the middle or the end of a cell too.
        assert_eq!(tab(at("ab") + 2, true), Some(Tab::To(at("c |"))));
        assert_eq!(tab(at("ab") + 1, false), None);
        assert_eq!(tab(0, true), Some(Tab::To(at("ab"))));
        assert_eq!(tab(at("|---"), true), Some(Tab::To(at("d |"))));
        assert_eq!(tab(at("|---"), false), Some(Tab::To(at("c |"))));
        // From the closing pipe, past the last cell: nothing.
        assert_eq!(tab(text.len(), true), None);
    }

    /// `text` after Tab at `at`'s `Tab::Add`, and the new cursor, which
    /// is in a cell of the edited text's table.
    fn added(text: &str, at: usize) -> (String, usize) {
        let found = Cells::around(text, at).unwrap();
        let Some(Tab::Add {
            at,
            text: add,
            cursor,
        }) = found.tab(text, at, true)
        else {
            panic!("no Tab::Add");
        };
        let mut edited = text.to_owned();
        edited.insert_str(at, &add);
        let cells = Cells::around(&edited, cursor).unwrap();
        assert!(cells.cell_at(cursor).is_some(), "{edited:?} {cursor}");
        (edited, cursor)
    }

    /// As in Obsidian (step TC1): Tab in a short row's last cell adds its
    /// missing cells (and the closing pipe it lacks) and goes to the
    /// first; in the table's last cell it adds a row of empty cells, in
    /// the last line's quote or list, and goes to its first cell; the
    /// row is the table's (found from the cursor, as many cells as the
    /// header).
    #[test]
    fn tab_fills_a_short_row_and_adds_a_row() {
        let head = "| a | b | c |\n|---|---|---|\n";
        for (row, filled) in [
            ("| x", "| x |  |  |"),
            ("| x |", "| x |  |  |"),
            ("| x | y |  ", "| x | y |  |  "),
        ] {
            let text = [head, row, "\n\nafter"].concat();
            let last = text.rfind(['x', 'y']).unwrap() + 1;
            let (edited, cursor) = added(&text, last);
            assert_eq!(edited, [head, filled, "\n\nafter"].concat());
            let first_new = if row.contains('y') { 2 } else { 1 };
            let cells = Cells::around(&edited, cursor).unwrap();
            assert_eq!(cells.rows[1].len(), 3);
            assert_eq!(cells.rows[1][first_new].text.start, cursor);
        }
        for (text, row) in [
            ("| a | b |\n|---|---|\n| x | y |\n\nafter", "\n|  |  |"),
            ("| a | b |\n|---|---|", "\n|  |  |"),
            ("> | a |\n> |---|\n> | x |\n", "\n> |  |"),
            ("- item\n\n  | a |\n  |---|\n  | x |", "\n  |  |"),
        ] {
            let end = Cells::around(text, text.find('a').unwrap()).unwrap().end;
            let last = text[..end].rfind(char::is_alphabetic).unwrap() + 1;
            let (edited, cursor) = added(text, last);
            assert_eq!(
                edited,
                [&text[..end], row, &text[end..]].concat(),
                "{text:?}"
            );
            let cells = Cells::around(&edited, cursor).unwrap();
            let new = cells.rows.last().unwrap();
            assert_eq!((new.len(), new[0].text.start), (cells.columns, cursor));
        }
    }

    /// Up and Down: the same column's cell a row away (the `|---|` line
    /// has none), the row's last cell when it is shorter, the offset kept
    /// as a goal across a short cell; none past the first or last row.
    /// A cursor on a pipe or padding: the nearest cell on its line.
    #[test]
    fn up_and_down_go_row_to_row() {
        let text = "| ab | cde |\n|---|---|\n| f | g |\n| hij |";
        let found = Cells::around(text, 0).unwrap();
        let at = |s: &str| text.find(s).unwrap();
        let (c, g, h) = (at("cde"), at("g |"), at("hij"));
        assert_eq!(found.vertical(text, c + 2, true, None), Some((g + 1, 2)));
        assert_eq!(found.vertical(text, g + 1, true, Some(2)), Some((h + 2, 2)));
        assert_eq!(
            found.vertical(text, g + 1, false, Some(2)),
            Some((c + 2, 2))
        );
        assert_eq!(found.vertical(text, h, true, None), None);
        assert_eq!(found.vertical(text, at("ab"), false, None), None);
        assert_eq!(found.vertical(text, 0, true, None), None, "on a pipe");
        // The nearest cell on the line.
        assert_eq!(found.nearest(text, 0), Some(at("ab")));
        assert_eq!(found.nearest(text, at("ab") + 3), Some(at("ab") + 2));
        assert_eq!(found.nearest(text, at(" |\n|---")), Some(c + 3));
        assert_eq!(found.nearest(text, at("|---")), None);
    }

    /// Tables in a quote, in a list item and with `\r\n` breaks.
    #[test]
    fn cells_in_quotes_lists_and_windows_lines() {
        let quote = "> | a | b |\n> |---|---|\n> | c | d |\n";
        assert_eq!(cells(quote, 0), [vec!["a", "b"], vec!["c", "d"]]);
        let list = "- item\n\n  | a | b |\n  |---|---|\n  | c | d |\n";
        assert_eq!(cells(list, 9), [vec!["a", "b"], vec!["c", "d"]]);
        let windows = "| a | b |\r\n|---|---|\r\n| c | d |\r\n";
        assert_eq!(cells(windows, 0), [vec!["a", "b"], vec!["c", "d"]]);
        let found = Cells::around(windows, 0).unwrap();
        assert_eq!(found.end, windows.len() - 2);
    }

    /// Left and Right: by grapheme inside a cell, cell to cell at its
    /// edges, from a pipe or padding to the nearest cell, out at the ends.
    #[test]
    fn steps_through_cells() {
        let text = "| ab | c |\n|---|---|\n| d | e |";
        let found = Cells::around(text, 0).unwrap();
        let at = |s: &str| text.find(s).unwrap();
        assert_eq!(found.step(at("ab"), true), Step::Grapheme);
        assert_eq!(found.step(at("ab") + 2, true), Step::To(at("c")));
        assert_eq!(found.step(at("c"), false), Step::To(at("ab") + 2));
        assert_eq!(found.step(at("c") + 1, true), Step::To(at("d")));
        assert_eq!(found.step(at("d"), false), Step::To(at("c") + 1));
        // From pipes, padding and the `|---|` line.
        assert_eq!(found.step(0, true), Step::To(at("ab")));
        assert_eq!(found.step(at("|---"), true), Step::To(at("d")));
        assert_eq!(found.step(at("|---"), false), Step::To(at("c") + 1));
        // Out at both ends.
        assert_eq!(found.step(at("ab"), false), Step::Leave);
        assert_eq!(found.step(at("e") + 1, true), Step::Leave);
        assert_eq!(found.step(text.len(), true), Step::Leave);
        // Empty cells hold one position.
        let text = "| a |  | b |\n|---|---|---|";
        let found = Cells::around(text, 0).unwrap();
        let Step::To(empty) = found.step(text.find('a').unwrap() + 1, true) else {
            panic!("no step");
        };
        assert_eq!(found.cell_at(empty), Some(&(empty..empty)));
        assert_eq!(found.step(empty, true), Step::To(text.find('b').unwrap()));
    }
}
