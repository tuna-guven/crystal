//! The table cells around the editor's cursor in live mode (steps TB2 to
//! TB5): where the cursor goes in a table, so it moves through its cells'
//! text and never onto the hidden pipes or the padding between them.
//!
//! Pure: the text and a position in, positions out. The cells come from
//! the same parse the view draws from (`live::parse`, `Layout::tables`),
//! made again on a key, never kept, so nothing has to follow edits; the
//! parse runs only when the cursor's line holds a `|`, so other keys cost
//! nothing. A cell counts only inside its own line: the parser places a
//! short row's missing cells past the line's break, where they are no
//! place of the row (the view's rule too, `view::table`). The `|---|`
//! line has no cells. A body row the parser continues without any `|`
//! (a lazy row) is not found from its own line: keys move there as on
//! any line.
//!
//! The editor applies what these functions say only in live mode (the
//! view boxes tables there), and still snaps every position to a grapheme
//! boundary; outside a table they say nothing and keys work as always.

use std::ops::Range;

use super::live::{self, line_end};

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

/// A table's cells, row by row (the header first), each in its line.
pub struct Cells {
    rows: Vec<Vec<Range<usize>>>,
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
                rows: table
                    .rows
                    .iter()
                    .map(|(line, cells)| {
                        let start = line_start(*line);
                        let end = line_end(text, start);
                        cells
                            .iter()
                            .filter(|cell| start <= cell.start && cell.end <= end)
                            .cloned()
                            .collect()
                    })
                    .collect(),
                start: first,
                end,
            })
        })
    }

    /// Every cell, in the text's order.
    fn all(&self) -> impl DoubleEndedIterator<Item = &Range<usize>> {
        self.rows.iter().flatten()
    }

    /// The cell whose text holds `at` (from its start to its end).
    pub fn cell_at(&self, at: usize) -> Option<&Range<usize>> {
        self.all()
            .find(|cell| (cell.start..=cell.end).contains(&at))
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
            .map(|row| row.iter().map(|cell| &text[cell.clone()]).collect())
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
