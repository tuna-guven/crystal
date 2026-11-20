//! Re-padding a live table's Markdown (steps TP1 to TP3): its pipes lined
//! up in the source too, as the Advanced Tables plugin does in Obsidian,
//! on Tab and when the cursor leaves the table.
//!
//! Pure: the text and a position in, the table's range, its padded text
//! and a position map out. This is the one place Crystal writes note text
//! the user did not type, so it refuses (`None`) rather than guess: a
//! table whose cells it cannot measure, a row with anything outside its
//! cells but pipes, spaces and tabs (the parser ignores cells past the
//! header's, so their text would be lost), a row of no cells; and the
//! padded text is parsed again, every cell's text must come out the same
//! and in the same order, or nothing is written. The padded text holds
//! note text: it lives in a `Zeroizing` string, never in `format!`.

use std::ops::Range;

use pulldown_cmark::Alignment;
use zeroize::Zeroizing;

use super::live::layout::Table;
use super::live::{self, cell_reach, line_end};
use super::view::measurable;
use crate::ui::grid::display_width;

/// The narrowest column, as the `|---|` line needs 3 dashes to read well.
const MIN_WIDTH: usize = 3;

/// A table's padded text, and where positions in it go.
pub struct Repad {
    /// The table's lines in the old text: from its first line's start to
    /// its last line's end, before its line break.
    pub range: Range<usize>,
    pub text: Zeroizing<String>,
    /// Each cell in the old text, from its text's start to its reach
    /// (`live::cell_reach`), its text's length, and its start in the new.
    cells: Vec<(Range<usize>, usize, usize)>,
}

impl Repad {
    /// Where `at` goes once the table is padded: in a cell (up to the
    /// spaces after its text), the same offset into its text, clamped to
    /// it; before the table, nowhere else; after it, shifted by the
    /// change in length; on a pipe or padding, the table's start.
    pub fn map(&self, at: usize) -> usize {
        let (start, end) = (self.range.start, self.range.end);
        if let Some((old, len, new)) = self
            .cells
            .iter()
            .find(|(old, ..)| old.contains(&at) || old.end == at)
        {
            return new + (at - old.start).min(*len);
        }
        if at < start {
            at
        } else if at >= end {
            at - end + start + self.text.len()
        } else {
            start
        }
    }
}

/// The table whose lines hold `at`, padded; `None` outside a table, for
/// a table refused (see the module's header), or one already padded.
pub fn repad(text: &str, at: usize) -> Option<Repad> {
    let layout = live::parse(text).layout;
    let line_start = |i: usize| layout.lines.get(i).map_or(text.len(), |l| l.markup.start);
    let table = layout.tables.iter().find(|table| {
        let end = table
            .lines
            .end
            .checked_sub(1)
            .map(|last| line_end(text, line_start(last)));
        end.is_some_and(|end| (line_start(table.lines.start)..=end).contains(&at))
    })?;
    // One line per row, and the `|---|` line second.
    if table.lines.len() != table.rows.len() + 1 || !measurable(text, table) {
        return None;
    }
    let widths = widths(text, table);
    let range = line_start(table.lines.start)..line_end(text, line_start(table.lines.end - 1));
    let mut out = Zeroizing::new(String::with_capacity(range.len() * 2));
    let mut cells = Vec::new();
    let mut rows = table.rows.iter();
    for (k, line) in table.lines.clone().enumerate() {
        let start = line_start(line);
        let end = line_end(text, start);
        if k > 0 {
            // The line break before it, as written (`\n` or `\r\n`).
            out.push_str(text.get(line_end(text, line_start(line - 1))..start)?);
        }
        if k == 1 {
            dashes(&text[start..end], table, &widths, &mut out)?;
            continue;
        }
        let (_, row) = rows.next()?;
        let (starts, began) = row_text(text, start..end, row, table, &widths, &mut out)?;
        for (cell, at) in row.iter().zip(starts) {
            let reach = cell.start..cell_reach(text, cell);
            cells.push((reach, cell.len(), range.start + began + at));
        }
    }
    if text.get(range.clone())? == out.as_str() || !same_cells(text, &range, &out, table) {
        return None;
    }
    Some(Repad {
        range,
        text: out,
        cells,
    })
}

/// Each column's widest cell as written, at least `MIN_WIDTH`.
fn widths(text: &str, table: &Table) -> Vec<usize> {
    let mut widths = vec![MIN_WIDTH; table.aligns.len()];
    for (_, cells) in &table.rows {
        for (width, cell) in widths.iter_mut().zip(cells) {
            *width = (*width).max(display_width(&text[cell.clone()]));
        }
    }
    widths
}

/// The leading `>`s, spaces and tabs of `line`.
fn lead(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t', '>']).len()]
}

/// The `|---|` line padded into `out`: its lead, then each column's dashes
/// with the alignment's colons. `None` if it holds anything else.
fn dashes(line: &str, table: &Table, widths: &[usize], out: &mut String) -> Option<()> {
    let lead = lead(line);
    if !line[lead.len()..].chars().all(|c| "|-: \t".contains(c)) {
        return None;
    }
    out.push_str(lead);
    out.push('|');
    for (align, &width) in table.aligns.iter().zip(widths) {
        let (left, right) = match align {
            Alignment::Left => (":", ""),
            Alignment::Right => ("", ":"),
            Alignment::Center => (":", ":"),
            Alignment::None => ("", ""),
        };
        out.push(' ');
        out.push_str(left);
        out.push_str(&"-".repeat(width - left.len() - right.len()));
        out.push_str(right);
        out.push_str(" |");
    }
    Some(())
}

/// One row padded into `out`: its own leading markup (before its first
/// cell, less the pipe and spaces there), then each cell padded by its
/// column's alignment, a missing one empty. Gives each cell's new start
/// and `out`'s length when the row began, both counted in `out`, or
/// `None` if the row has no cell or anything but pipes, spaces and tabs
/// outside its cells.
fn row_text(
    text: &str,
    line: Range<usize>,
    row: &[Range<usize>],
    table: &Table,
    widths: &[usize],
    out: &mut String,
) -> Option<(Vec<usize>, usize)> {
    // More cells than the header: the parser may keep them; padding to
    // the header's columns would drop their text.
    if row.len() > table.aligns.len() {
        return None;
    }
    let first = row.first()?;
    let before = text
        .get(line.start..first.start)?
        .trim_end_matches([' ', '\t']);
    let lead = before.strip_suffix('|').unwrap_or(before);
    // What lies between the lead, the cells and the line's end.
    let mut gaps = Vec::with_capacity(row.len() + 1);
    gaps.push(line.start + lead.len()..first.start);
    gaps.extend(row.windows(2).map(|pair| pair[0].end..pair[1].start));
    gaps.push(row.last()?.end..line.end);
    let clean = |gap: &Range<usize>| {
        text.get(gap.clone())
            .is_some_and(|gap| gap.chars().all(|c| matches!(c, '|' | ' ' | '\t')))
    };
    if !gaps.iter().all(clean) {
        return None;
    }
    let began = out.len();
    out.push_str(lead);
    out.push('|');
    let mut starts = Vec::with_capacity(row.len());
    for (c, (align, &width)) in table.aligns.iter().zip(widths).enumerate() {
        let cell = row.get(c).map_or("", |cell| &text[cell.clone()]);
        let pad = width.saturating_sub(display_width(cell));
        let left = match align {
            Alignment::Right => pad,
            Alignment::Center => pad / 2,
            Alignment::Left | Alignment::None => 0,
        };
        out.push(' ');
        out.push_str(&" ".repeat(left));
        if c < row.len() {
            starts.push(out.len() - began);
        }
        out.push_str(cell);
        out.push_str(&" ".repeat(pad - left));
        out.push_str(" |");
    }
    Some((starts, began))
}

/// Whether the padded table, parsed in place of the old one, has the same
/// cells' text in the same order (a short row's missing cells now empty).
fn same_cells(text: &str, range: &Range<usize>, padded: &str, table: &Table) -> bool {
    let mut edited = Zeroizing::new(String::with_capacity(text.len() + padded.len()));
    edited.push_str(&text[..range.start]);
    edited.push_str(padded);
    edited.push_str(&text[range.end..]);
    let layout = live::parse(&edited).layout;
    let Some(new) = layout.tables.iter().find(|new| {
        layout
            .lines
            .get(new.lines.start)
            .is_some_and(|line| line.markup.start == range.start)
    }) else {
        return false;
    };
    new.aligns == table.aligns
        && new.rows.len() == table.rows.len()
        && new
            .rows
            .iter()
            .zip(&table.rows)
            .all(|((_, new), (_, old))| {
                new.len() == table.aligns.len()
                    && (0..new.len()).all(|c| {
                        let old = old.get(c).map_or("", |cell| &text[cell.clone()]);
                        edited.get(new[c].clone()) == Some(old)
                    })
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text` with the table at `at` padded.
    fn padded(text: &str, at: usize) -> String {
        let repad = repad(text, at).expect("padded");
        [
            &text[..repad.range.start],
            &repad.text,
            &text[repad.range.end..],
        ]
        .concat()
    }

    /// Columns as wide as their widest cell (3 at least), the `|---|` line
    /// as dashes; text before and after the table kept; already padded:
    /// nothing to do.
    #[test]
    fn columns_line_up() {
        let text = "before\n\n|a|bbbb|\n|-|-|\n|cc|d|\n\nafter";
        let want = "before\n\n| a   | bbbb |\n| --- | ---- |\n| cc  | d    |\n\nafter";
        assert_eq!(padded(text, 9), want);
        assert!(repad(want, 9).is_none(), "already padded");
        assert!(repad(text, 0).is_none(), "not in a table");
    }

    /// Alignments: right-aligned cells padded on the left, centered on
    /// both sides, the colons kept; wide characters count two columns;
    /// an escaped `\|` stays in its cell; a short row's missing cells come
    /// in empty; CRLF line breaks are kept.
    #[test]
    fn alignments_widths_escapes_and_short_rows() {
        let text = "| a | b | c |\n|:-|-:|:-:|\n| 日本 | x \\| y |\n";
        let want = concat!(
            "| a    |      b |  c  |\n",
            "| :--- | -----: | :-: |\n",
            "| 日本 | x \\| y |     |\n",
        );
        assert_eq!(padded(text, 0), want);
        let crlf = "| a |\r\n|-|\r\n| bb |\r\n";
        assert_eq!(padded(crlf, 0), "| a   |\r\n| --- |\r\n| bb  |\r\n");
    }

    /// A table in a quote and in a list keeps each line's own leading
    /// markup.
    #[test]
    fn quotes_and_lists_keep_their_markup() {
        let quote = "> |a|b|\n> |-|-|\n> |c|d|";
        let want = "> | a   | b   |\n> | --- | --- |\n> | c   | d   |";
        assert_eq!(padded(quote, 0), want);
        let list = "- item\n\n  |a|\n  |-|\n  |c|";
        assert_eq!(padded(list, 9), "- item\n\n  | a   |\n  | --- |\n  | c   |");
    }

    /// Refused, the table left as written: extra cells past the header's
    /// (their text would be lost), a tab in a cell (not measurable).
    #[test]
    fn refuses_what_it_would_change() {
        assert!(repad("| a |\n|-|\n| b | lost |", 0).is_none());
        assert!(repad("| a |\n|-|\n| b\tc |", 0).is_none());
    }

    /// A position in a cell keeps its offset in the cell's text (up to its
    /// end); before the table it stays, after it it shifts.
    #[test]
    fn positions_follow_their_cells() {
        let text = "x\n\n|a|bc|\n|-|-|\n\ny";
        let repad = repad(text, 3).unwrap();
        let new = padded(text, 3);
        let at = |s: &str, t: &str| t.find(s).unwrap();
        assert_eq!(repad.map(at("bc", text) + 1), at("bc", &new) + 1);
        assert_eq!(repad.map(at("a|", text)), at("a ", &new));
        assert_eq!(repad.map(1), 1);
        assert_eq!(repad.map(text.len()), new.len());
    }
}
