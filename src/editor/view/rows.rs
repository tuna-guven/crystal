//! The row walk (step RF2): how the editor's text is cut into screen
//! rows, one after another, and how scrolling and the mouse find a row.
//!
//! A screen row is one of two kinds:
//! - a row of text: a range of the note, from its start to where it
//!   wraps or its line ends (a boxed table's line is one row, whole);
//! - a drawn row: a row live mode shows that the note has no line for (the
//!   viewer's blank row, a table's border, a wrapped table row's further
//!   rows; `Look::drawn`). It holds no cursor position (a click there
//!   goes to the line below), except a wrapped table row's further row:
//!   the cursor in a cell is drawn on the row of the cell's text holding
//!   it (step TB1). Which row holds the cursor is `Look::holds`'s to say.
//!
//! Rows are found by walking forward from a known row start, never kept:
//! a cache would have to follow every edit. Every walk goes through the
//! `Rows` iterator, so drawing, scrolling, clicks and Ctrl+E's anchor all
//! see the same rows; a difference between them would put the cursor or
//! a click on the wrong row.
//!
//! `RowStart` names a row: a text row by its first byte, a drawn row by
//! the line it goes before and its place among that line's drawn rows.
//! It is a newtype so no other code does the bit arithmetic; a row start
//! is only made here (`text`, `drawn`, `next_drawn`).

use std::collections::VecDeque;
use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use super::glyph::{Shape, shape};
use super::look::Look;

/// A drawn row's start has this bit; a text row's never has, as notes
/// (at most 8 MiB) are far smaller.
const MARK: usize = 1 << (usize::BITS - 1);
/// A drawn row's place among its line's drawn rows counts in units of
/// this; the line is the part below it.
const ORDINAL: usize = 1 << 40;

/// Where a screen row starts (see the module comment).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowStart(usize);

impl RowStart {
    /// The text row starting at byte `offset` of the note.
    pub const fn text(offset: usize) -> Self {
        Self(offset)
    }

    /// The first drawn row before the line starting at `line`.
    pub const fn drawn(line: usize) -> Self {
        Self(MARK | line)
    }

    /// The line this row is on (a text row: its first byte), or the line
    /// it goes before (a drawn row).
    pub const fn line(self) -> usize {
        if self.is_drawn() {
            self.0 % ORDINAL
        } else {
            self.0
        }
    }

    /// Whether this is a drawn row.
    pub const fn is_drawn(self) -> bool {
        self.0 & MARK != 0
    }

    /// A drawn row's place among its line's drawn rows (from 0).
    pub const fn ordinal(self) -> Option<usize> {
        if self.is_drawn() {
            Some((self.0 & !MARK) / ORDINAL)
        } else {
            None
        }
    }

    /// The drawn row after this drawn one, before the same line (whether
    /// the line has it is `Look`'s to say).
    pub const fn next_drawn(self) -> Self {
        Self(self.0 + ORDINAL)
    }
}

/// One screen row.
pub struct Row {
    /// The bytes it shows (without its line break); empty for a drawn row.
    pub range: Range<usize>,
    /// Where the next row starts; `None` after the text's last row.
    pub next: Option<RowStart>,
    /// Ended by a line break or the end of the text, not by wrapping.
    pub ends_line: bool,
    /// Columns left of its text for live mode's prefix (0 when the prefix
    /// would leave no room).
    pub indent: usize,
    /// A drawn row (steps LV2, LT2, LW1): no range of the note; it holds
    /// no position, except a wrapped table row's further row, whose cells
    /// hold the cursor (step TB1, `Look::holds`).
    pub drawn: bool,
}

impl Row {
    /// Whether the cursor at `at` is drawn on this row. At a wrap the
    /// position belongs to the next row; at a line break, to this one.
    pub fn contains(&self, at: usize) -> bool {
        // The next row's start, without its mark (steps LV2, LT2). A row
        // ending the text is followed by no line, at most a table's
        // bottom border: the text's end is its own.
        let end = if self.ends_line {
            match self.next.map(RowStart::line) {
                Some(next) if next > self.range.end => next,
                _ => usize::MAX,
            }
        } else {
            self.range.end
        };
        self.range.start <= at && at < end
    }
}

/// The rows from a row start on, each with its start, until the text's
/// last row. The one way rows are walked (see the module comment).
pub struct Rows<'a> {
    text: &'a str,
    width: usize,
    live: &'a Look,
    next: Option<RowStart>,
}

impl Iterator for Rows<'_> {
    type Item = (RowStart, Row);

    fn next(&mut self) -> Option<Self::Item> {
        let start = self.next?;
        let row = row_at(self.text, start, self.width, self.live);
        self.next = row.next;
        Some((start, row))
    }
}

/// The rows of `text` from `start` on, `width` columns wide, as `live`
/// draws them.
pub fn rows<'a>(text: &'a str, start: RowStart, width: usize, live: &'a Look) -> Rows<'a> {
    Rows {
        text,
        width,
        live,
        next: Some(start),
    }
}

/// The row starting at `start`, `width` columns wide. A grapheme wider
/// than a whole row gets a row of its own.
pub fn row_at(text: &str, start: RowStart, width: usize, live: &Look) -> Row {
    if start.is_drawn() {
        let line = start.line().min(text.len());
        // The next drawn row, else the line, unless past the text.
        let gone = line >= text.len() && !text.ends_with('\n');
        let next = live.next_drawn(start);
        return Row {
            range: line..line,
            next: next.or((!gone).then_some(RowStart::text(line))),
            ends_line: false,
            indent: 0,
            drawn: true,
        };
    }
    let start = start.line();
    // A boxed table's line: one row, whole (step LW2); its cells wrap
    // inside the box, onto drawn rows before the next line (step LW3),
    // which hold the cursor in a cell (step TB1, `Look::holds`).
    if let Some(boxed) = live.boxed_at(start) {
        let rest = &text[boxed.end..];
        let break_len = if rest.starts_with("\r\n") {
            2
        } else {
            usize::from(rest.starts_with('\n'))
        };
        let next = if break_len > 0 {
            live.next_row(boxed.end + break_len, text)
        } else {
            Some(live.first_row(text.len())).filter(|first| first.is_drawn())
        };
        return Row {
            range: start..boxed.end,
            next,
            ends_line: true,
            indent: 0,
            drawn: false,
        };
    }
    // Live mode's prefix takes its columns first, if text still fits.
    let indent = live.prefix(start).width();
    let indent = if indent < width { indent } else { 0 };
    let width = width - indent;
    let rest = text.get(start..).unwrap_or("");
    let mut column = 0;
    // Just after the last space or tab shown on this row: a row breaks
    // there, between words, as the viewer's (`ui::wrap`); only a word
    // longer than the row breaks inside it.
    let mut after_space = None;
    for (i, grapheme) in rest.grapheme_indices(true) {
        let at = start + i;
        if matches!(shape(grapheme), Shape::Break) {
            // A folded heading's row is followed by its section's end.
            let next = live.next_row(at + grapheme.len(), text);
            return Row {
                range: start..at,
                next,
                ends_line: true,
                indent,
                drawn: false,
            };
        }
        let w = live.width(at, grapheme, column);
        let space = w > 0 && matches!(grapheme, " " | "\t");
        // A space one column past the row stays at its end, as in the
        // viewer: the text area keeps that column for the cursor.
        let hangs = space && column + w <= width + 1;
        if column > 0 && column + w > width && !hangs {
            let cut = if space { None } else { after_space };
            let cut = cut.unwrap_or(at);
            return Row {
                range: start..cut,
                next: Some(RowStart::text(cut)),
                ends_line: false,
                indent,
                drawn: false,
            };
        }
        column += w;
        if space {
            after_space = Some(at + grapheme.len());
        }
    }
    // A boxed table's bottom border may follow the last line.
    let next =
        Some(live.first_row(text.len())).filter(|first| first.is_drawn() && !text.ends_with('\n'));
    Row {
        range: start..text.len(),
        next,
        ends_line: true,
        indent,
        drawn: false,
    }
}

/// The start of the line holding `at`.
pub fn line_start(text: &str, at: usize) -> usize {
    text.get(..at)
        .and_then(|head| head.rfind('\n'))
        .map_or(0, |i| i + 1)
}

/// The start of the row the cursor at `at` is drawn on; for a drawn row
/// (a kept scroll position), itself while it is still there, else its
/// line's row.
pub fn row_start(text: &str, at: RowStart, width: usize, live: &Look) -> RowStart {
    if at.is_drawn() {
        let line = at.line().min(text.len());
        if live.drawn_row(at).is_some() && text.is_char_boundary(line) {
            return at;
        }
        return row_start(text, RowStart::text(line), width, live);
    }
    let at = at.line();
    // In skipped lines: the next row drawn (no row holds `at` itself).
    if let Some(skip) = live.skipped_at(at) {
        return live
            .next_row(skip.start, text)
            .unwrap_or(RowStart::text(skip.start));
    }
    // Inside a folded section: its heading's row.
    let start = RowStart::text(
        live.folded_heading(at)
            .unwrap_or_else(|| line_start(text, at)),
    );
    // The first row of the line holding `at`, or its last.
    rows(text, start, width, live)
        .find(|(start, row)| live.holds(*start, row, at) || row.next.is_none())
        .map_or(start, |(start, _)| start)
}

/// The start of the row `rows - 1` rows above the cursor's row, or the
/// text's first row, so the cursor's row is the last one shown.
///
/// Walks each logical line forward once, keeping only the last row starts
/// it needs: on a line of a million rows, finding the row above by
/// walking from the line's start again for every screen row took seconds
/// per key (found by review).
pub fn top_for(text: &str, cursor: usize, width: usize, rows: usize, live: &Look) -> RowStart {
    let mut need = rows.max(1);
    let mut line = line_start(text, cursor);
    // On the cursor's line, the walk stops at the cursor's row; on the
    // lines above, at their end.
    let mut until = Some(cursor);
    loop {
        let mut last = VecDeque::with_capacity(need);
        // The cursor on a wrapped table cell's further row (step TB1) is
        // past its line's own row: those rows go before the next line.
        let further = until.is_some_and(|at| live.boxed_spot(at).is_some_and(|(_, k, _)| k > 0));
        // A line's rows start with its drawn rows, if it has any.
        for (start, row) in self::rows(text, live.first_row(line), width, live) {
            if last.len() == need {
                last.pop_front();
            }
            last.push_back(start);
            let holds = until.is_some_and(|at| live.holds(start, &row, at));
            if holds || (row.ends_line && !further) {
                break;
            }
        }
        let first = last.front().copied().unwrap_or(RowStart::text(line));
        if last.len() == need || line == 0 {
            return first;
        }
        need -= last.len();
        // `line - 1` is the `\n` ending the line above (also inside `\r\n`).
        line = line_start(text, line - 1);
        // Skipped lines have no row; a folded section's lines are its
        // heading's.
        while let Some(skip) = live.skipped_at(line) {
            if skip.start == 0 {
                return first;
            }
            line = line_start(text, skip.start - 1);
        }
        if let Some(heading) = live.folded_heading(line) {
            line = heading;
        }
        until = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The scroll's first version, kept as the reference: from the
    /// cursor's row, step to the row above `rows - 1` times, each found by
    /// walking its line from the start. Right, but quadratic on long lines.
    fn top_by_rows(text: &str, cursor: usize, width: usize, rows: usize) -> usize {
        let row = |at| row_start(text, RowStart::text(at), width, &Look::default()).line();
        let mut top = row(cursor);
        for _ in 1..rows {
            let Some(c) = text.get(..top).and_then(|head| head.chars().next_back()) else {
                break;
            };
            top = row(top - c.len_utf8());
        }
        top
    }

    proptest! {
        /// The single walk picks the same first row as the reference, on
        /// any text, cursor, width and height.
        #[test]
        fn top_for_matches_the_reference(
            text in "[ab \t\n\r\u{1b}\u{301}日😀]{0,80}",
            pick in any::<prop::sample::Index>(),
            width in 1usize..12,
            rows in 1usize..8,
        ) {
            let mut stops: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
            stops.push(text.len());
            let cursor = stops[pick.index(stops.len())];
            prop_assert_eq!(
                top_for(&text, cursor, width, rows, &Look::default()).line(),
                top_by_rows(&text, cursor, width, rows)
            );
        }
    }
}
