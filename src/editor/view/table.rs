//! Boxed tables in live mode (steps LT2, LW2, LW3, TB1; their own file
//! since step RF4): a table is drawn as the viewer draws it, in a `┌─┬─┐`
//! box, its columns fitted to the screen, its cells wrapped inside them,
//! also while the cursor is in a cell's text, as in Obsidian.
//!
//! The box is made again every frame by `tables`, a step of `Look::new`:
//! its border rows become drawn rows before lines (`Drawn::Pieces`), each
//! of its lines a `Boxed` holding its cells' ranges per screen row. A
//! boxed line is drawn whole from those ranges (`draw_boxed`), the cursor
//! and the selection in its cells. The rule that keeps the cursor seen:
//! a position no cell holds (a hidden pipe, the padding, the `|---|`
//! line) has no place in the box, so with the cursor there, or a
//! selection reaching out of the table, the table is drawn as written
//! (`in_cells`). Which screen row holds the cursor is `Look::holds`'s to
//! say, for drawing and scrolling alike. A click lands on the grapheme
//! drawn under it (`click_boxed`). Every grapheme in a cell still goes
//! through `glyph::put`, so the checks for hidden characters hold in
//! cells too.

use std::ops::Range;

use pulldown_cmark::Alignment;
use unicode_segmentation::UnicodeSegmentation;

use super::glyph::{Shape, shape};
use super::look::{BORDER, BOTTOM, CONTINUED, Drawn, Editing, Hiding, Look};
use super::rows::{Row, RowStart};
use super::{drawn_style, put_grapheme};
use crate::editor::Editor;
use crate::editor::live::layout::{Layout, Piece, Table};
use crate::editor::live::{self, Run, Show, line_end};
use crate::md::render::{DASHES, QUOTE_BAR, fit_columns};
use crate::ui::grid::{Grid, Style, display_width};
use crate::ui::layout::Rect;
use crate::ui::wrap::{Span, wrap_each};

/// A boxed table's line (step LW2): one row drawn whole from its cells
/// (`draw_boxed`), its own wrapping inside them; the cursor in a cell
/// is drawn where its cell's text is (step TB1, `Boxed::spot`).
pub struct Boxed {
    /// The line's start and its end, before its line break.
    pub start: usize,
    pub end: usize,
    /// The prefix of its first row: the line's own, then for the `|---|`
    /// line its `├─┼─┤` (and no cells); of its other rows: the line's.
    first: Range<u32>,
    rest: Range<u32>,
    /// Each column's width and alignment.
    widths: Vec<u16>,
    aligns: Vec<Alignment>,
    /// Each screen row's text in each column (step LW3): its cells
    /// wrapped into their columns, without the spaces around them (empty
    /// where a cell has no more lines, or a short row lacks it). One row
    /// unless a cell wraps; none for the `|---|` line.
    rows: Vec<Vec<Range<usize>>>,
    /// Each cell's text, without the spaces around it (`Table::rows`):
    /// where the cursor may be; none for the `|---|` line.
    cells: Vec<Range<usize>>,
}

impl Boxed {
    /// Where the cursor at `at` is drawn in this line (step TB1): its
    /// screen row and its column, if `at` is in a cell's text (from its
    /// start to its end); on the last of that cell's wrapped rows that
    /// starts at or before `at` (a space a wrap dropped stays on the row
    /// before it). `None` on a pipe, padding, or the `|---|` line.
    pub fn spot(&self, at: usize) -> Option<(usize, usize)> {
        let c = self
            .cells
            .iter()
            .position(|cell| (cell.start..=cell.end).contains(&at))?;
        // A cell's further rows are never empty; an empty range there is
        // a shorter cell's filler.
        let k = (1..self.rows.len())
            .rev()
            .find(|&k| {
                let line = &self.rows[k][c];
                !line.is_empty() && line.start <= at
            })
            .unwrap_or(0);
        Some((k, c))
    }
}

/// Whether an edited table stays boxed (step TB1): the cursor is in the
/// text of one of its cells and a selection, if any, stays inside its
/// lines, from `start` to `end`. Anywhere else in it (a hidden pipe, the
/// padding, the `|---|` line) the cursor would not be drawn, and a
/// selection reaching out of it would show its hidden bytes as nothing,
/// so the table is drawn as written then, as before step TB1.
fn in_cells(table: &Table, editing: Editing, start: usize, end: usize) -> bool {
    let selection = editing.selection;
    let inside = selection.is_empty() || (start <= selection.start && selection.end <= end);
    inside
        && table
            .rows
            .iter()
            .flat_map(|(_, cells)| cells)
            .any(|cell| (cell.start..=cell.end).contains(&editing.cursor))
}

/// Whether every grapheme of the table's cells is drawn as wide as it is
/// measured (step TB1): a tab, or a character spelled out as `<U+XXXX>`
/// (decision 0037, `glyph::Shape`), is drawn wider than `drawn_width` and
/// `wrap_each` count it, so it would spill over its column and the next
/// `│` would cover it, and the cursor beside it. Such a table is drawn as
/// written, where the editor shows those characters as it does anywhere.
fn measurable(text: &str, table: &Table) -> bool {
    table.rows.iter().flat_map(|(_, cells)| cells).all(|cell| {
        text.get(cell.clone()).is_some_and(|cell| {
            cell.graphemes(true)
                .all(|grapheme| matches!(shape(grapheme), Shape::Text(..)))
        })
    })
}

/// Each column's widest cell as drawn by `runs`, as `live::parse` measures
/// `Table::widths`, but with markup revealed in a cell counted at its
/// written width (`look::revealed`), so the column fits what is drawn.
fn column_widths(text: &str, runs: &[Run], table: &Table) -> Vec<u16> {
    let mut widths = vec![0u16; table.aligns.len()];
    for (_, cells) in &table.rows {
        for (width, cell) in widths.iter_mut().zip(cells) {
            let drawn = u16::try_from(live::drawn_width(text, runs, cell));
            *width = (*width).max(drawn.unwrap_or(u16::MAX));
        }
    }
    widths
}

/// The columns of a table whose natural widths are `natural`, as the
/// viewer fits them in the screen's width (the cursor's column too: the
/// cursor in a boxed table is drawn inside its box), less the table's `prefix` and `│ ` before each
/// cell, ` ` after it, the closing `│` (step LW3); `None` if they do not
/// fit, or the table has no columns: it is drawn as written.
fn fit(prefix: &[Piece], natural: &[u16], width: usize) -> Option<Vec<u16>> {
    let screen = width + 1;
    let screen = match prefix.iter().map(|(s, _)| display_width(s)).sum::<usize>() {
        w if w >= screen => screen,
        w => screen - w,
    };
    let natural: Vec<usize> = natural.iter().map(|&w| usize::from(w)).collect();
    let fitted = screen
        .checked_sub(3 * natural.len() + 1)
        .and_then(|room| fit_columns(&natural, room));
    let fitted = fitted.filter(|_| !natural.is_empty())?;
    Some(
        fitted
            .iter()
            .map(|&w| u16::try_from(w).unwrap_or(u16::MAX))
            .collect(),
    )
}

/// Step 5, the boxed tables (steps LT2, LW2, LW3, TB1): a table not
/// folded whose columns fit is boxed, unless the cursor or the selection
/// is in it in a way the box cannot show (`in_cells`).
/// Its border rows go into `drawn` (none in a fold or right after one),
/// their pieces into the layout's; each of its lines becomes a `Boxed`,
/// drawn whole from its cells. Returns them, sorted.
pub fn tables(
    layout: &mut Layout,
    runs: &[Run],
    text: &str,
    editing: Editing,
    width: usize,
    hiding: &Hiding,
    drawn: &mut Vec<(usize, u8, Drawn)>,
) -> Vec<Boxed> {
    let mut boxed = Vec::new();
    for table in &layout.tables {
        let lines = &layout.lines;
        let start_of = |i: usize| lines.get(i).map_or(text.len(), |l| l.markup.start);
        let Some(first) = lines.get(table.lines.start) else {
            continue;
        };
        let start = first.markup.start;
        // The line after the table, and where the table's last line ends.
        let next = start_of(table.lines.end);
        let end = if table.lines.end < lines.len() {
            next.saturating_sub(1)
        } else {
            next
        };
        let edited = editing.on_lines(start, end);
        // The line after the table, past skipped lines; none in a fold.
        let at = hiding.past_skipped(next);
        let after = hiding.open(at).then_some(at);
        // Edited: the last line's wrapped rows go before the line after
        // it, so without one (a fold right after) the cursor could be on
        // a row not drawn.
        if edited && (!in_cells(table, editing, start, end) || after.is_none()) {
            continue;
        }
        if !measurable(text, table) {
            continue;
        }
        let prefix = layout.prefix(&first.rest).to_vec();
        let natural = column_widths(text, runs, table);
        let Some(widths) = fit(&prefix, &natural, width).filter(|_| hiding.open(start)) else {
            continue;
        };
        let widths = &widths;
        let top = border(&mut layout.pieces, &prefix, widths, ["┌─", "─┬─", "─┐"]);
        drawn.push((start, BORDER, Drawn::Pieces(top)));
        // Between two body rows, as the viewer draws them.
        for &(row, _) in table.rows.iter().skip(2) {
            let inner = border(&mut layout.pieces, &prefix, widths, ["├─", "─┼─", "─┤"]);
            drawn.push((start_of(row), BORDER, Drawn::Pieces(inner)));
        }
        if after.is_some() {
            let bottom = border(&mut layout.pieces, &prefix, widths, ["└─", "─┴─", "─┘"]);
            drawn.push((at, BOTTOM, Drawn::Pieces(bottom)));
        }
        // Its lines (step LW2): the `|---|` line as `├─┼─┤`, a row as
        // its cells.
        for i in table.lines.clone() {
            let line = &lines[i];
            let cells = table.rows.iter().find(|(row, _)| *row == i);
            let mut first = layout.prefix(&line.first).to_vec();
            let end = line_end(text, line.markup.start);
            let rows = match cells {
                Some((_, cells)) => wrapped(text, runs, cells, widths, end),
                None => {
                    border_row(&mut first, widths, ["├─", "─┼─", "─┤"]);
                    Vec::new()
                }
            };
            // A row taller than one line: its other rows go before the
            // next line, ahead of its border (step LW3).
            let next = if i + 1 < table.lines.end {
                Some(start_of(i + 1))
            } else {
                after
            };
            for row in 1..rows.len() {
                if let Some(next) = next {
                    let row = Drawn::Cells {
                        boxed: boxed.len(),
                        row,
                    };
                    drawn.push((next, CONTINUED, row));
                }
            }
            boxed.push(Boxed {
                start: line.markup.start,
                end,
                first: store(&mut layout.pieces, &first),
                rest: line.rest.clone(),
                widths: widths.clone(),
                aligns: table.aligns.clone(),
                rows,
                cells: cells.map(|(_, cells)| cells.clone()).unwrap_or_default(),
            });
        }
    }
    boxed
}

/// A table row's cells wrapped into their columns `widths` wide (step
/// LW3), as the viewer wraps them (`wrap_each` over the text as drawn,
/// trailing spaces dropped): each screen row's text per column, as many
/// rows as the tallest cell has lines, at least one. `end` is the line's
/// end, where a missing cell's empty range goes.
fn wrapped(
    text: &str,
    runs: &[Run],
    cells: &[Range<usize>],
    widths: &[u16],
    end: usize,
) -> Vec<Vec<Range<usize>>> {
    let columns: Vec<Vec<Range<usize>>> = widths
        .iter()
        .enumerate()
        .map(|(c, &width)| {
            let cell = cells.get(c).cloned().unwrap_or(end..end);
            // An empty cell keeps its place, where the cursor is drawn in
            // it (step TB1).
            if cell.is_empty() {
                return vec![cell];
            }
            // The cell's shown parts, each with where it starts.
            let first = runs.partition_point(|run| run.range.end <= cell.start);
            let (mut starts, mut spans) = (Vec::new(), Vec::new());
            for run in runs[first..]
                .iter()
                .take_while(|run| run.range.start < cell.end)
            {
                if run.show == Show::Text {
                    let part = run.range.start.max(cell.start)..run.range.end.min(cell.end);
                    starts.push(part.start);
                    spans.push(Span {
                        text: &text[part],
                        style: run.style,
                    });
                }
            }
            let mut lines = Vec::new();
            wrap_each(&spans, width, |line| {
                let (Some(a), Some(b)) = (line.first(), line.last()) else {
                    lines.push(cell.end..cell.end);
                    return;
                };
                let start = starts[a.span] + a.range.start;
                let stop = starts[b.span] + b.range.end;
                lines.push(start..start + text[start..stop].trim_end_matches(' ').len());
            });
            lines
        })
        .collect();
    let height = columns.iter().map(Vec::len).max().unwrap_or(0).max(1);
    (0..height)
        .map(|k| {
            columns
                .iter()
                .map(|lines| lines.get(k).cloned().unwrap_or(end..end))
                .collect()
        })
        .collect()
}

/// Adds a table border's pieces to `pieces` (step LT2): `prefix`, then
/// `left`, each column's dashes with `mid` between them, and `right`, in
/// the viewer's gray. Returns their range.
fn border(
    pieces: &mut Vec<Piece>,
    prefix: &[Piece],
    widths: &[u16],
    ends: [&'static str; 3],
) -> Range<u32> {
    let mut row = prefix.to_vec();
    border_row(&mut row, widths, ends);
    store(pieces, &row)
}

/// Adds a border's `left`, each column's dashes with `mid` between them,
/// and `right` to `row`, in the viewer's gray.
fn border_row(row: &mut Vec<Piece>, widths: &[u16], [left, mid, right]: [&'static str; 3]) {
    for (c, &width) in widths.iter().enumerate() {
        row.push((if c == 0 { left } else { mid }, QUOTE_BAR));
        repeat(row, DASHES, width, QUOTE_BAR);
    }
    row.push((right, QUOTE_BAR));
}

/// Adds `row` to `pieces`; returns its range.
fn store(pieces: &mut Vec<Piece>, row: &[Piece]) -> Range<u32> {
    let start = u32::try_from(pieces.len()).unwrap_or(u32::MAX);
    pieces.extend_from_slice(row);
    start..u32::try_from(pieces.len()).unwrap_or(u32::MAX)
}

/// Adds `count` characters of `fill` (all the same, one column each) to
/// `row`, in slices of it.
fn repeat(row: &mut Vec<Piece>, fill: &'static str, count: u16, style: Style) {
    let most = fill.chars().count().max(1);
    let size = fill.len() / most;
    let mut count = usize::from(count);
    while count > 0 {
        let n = count.min(most);
        row.push((&fill[..n * size], style));
        count -= n;
    }
}

/// The padding before and after a cell `drawn` columns wide in column
/// `c` of `boxed`, by the column's alignment, as the viewer pads it.
fn pads(boxed: &Boxed, c: usize, drawn: usize) -> (u16, u16) {
    let width = boxed.widths.get(c).copied().unwrap_or(0);
    let room = width.saturating_sub(u16::try_from(drawn).unwrap_or(u16::MAX));
    let before = match boxed.aligns.get(c) {
        Some(Alignment::Right) => room,
        Some(Alignment::Center) => room / 2,
        _ => 0,
    };
    (before, room - before)
}

/// Draws screen row `row` of a boxed table's line in `area`'s row (steps
/// LW2, LW3): its prefix, then each cell's text on that row between
/// `│`s, padded by its column's alignment, its text as `live` draws it
/// (styled, markup hidden), the selection and the cursor as on any row
/// (step TB1); the `|---|` line is its prefix alone (its `├─┼─┤`).
/// Returns the column the cursor was drawn at, if on this row.
pub fn draw_boxed(
    grid: &mut Grid,
    area: Rect,
    editor: &Editor,
    live: &Look,
    boxed: &Boxed,
    row: usize,
) -> Option<u16> {
    let text = editor.text();
    let cursor = editor.cursor();
    let spot = boxed.spot(cursor);
    let mut drawn_at = None;
    let mut after_text = None;
    let y = area.y;
    let mut x = area.x;
    let room = |x: u16| area.right().saturating_sub(x);
    let prefix = if row == 0 { &boxed.first } else { &boxed.rest };
    for (piece, style) in live.layout.prefix(prefix) {
        x += grid.put_str(x, y, piece, *style, room(x));
    }
    // The `|---|` line has no cells: its prefix is its `├─┼─┤`.
    let cells = boxed.rows.get(row)?;
    for (c, cell) in cells.iter().enumerate() {
        x += grid.put_str(x, y, if c == 0 { "│ " } else { " │ " }, QUOTE_BAR, room(x));
        let (before, _) = pads(boxed, c, live::drawn_width(text, &live.runs, cell));
        let start = x;
        x = x.saturating_add(before);
        let mut column = 0;
        for (i, grapheme) in text[cell.clone()].grapheme_indices(true) {
            let at = cell.start + i;
            let style = drawn_style(editor, live, at);
            let here = x.saturating_add(u16::try_from(column).unwrap_or(u16::MAX));
            if at == cursor {
                drawn_at = Some(here);
            }
            column += put_grapheme(
                grid,
                (here, y, room(here)),
                text,
                at,
                grapheme,
                column,
                style,
                live,
            );
        }
        // The cursor after this row's text in its cell (the cell's end,
        // or a space a wrap dropped): right of the text, at most on the
        // space after the column (the first of the next ` │`), never on a
        // `│`; drawn once the separators are, so none covers it.
        if spot == Some((row, c)) && drawn_at.is_none() {
            let room_left = usize::from(boxed.widths[c].saturating_sub(before));
            let column = (column + cursor.saturating_sub(cell.end)).min(room_left);
            drawn_at = Some(x.saturating_add(u16::try_from(column).unwrap_or(u16::MAX)));
            after_text = drawn_at;
        }
        x = start.saturating_add(boxed.widths[c]);
    }
    grid.put_str(x, y, " │", QUOTE_BAR, room(x));
    if let Some(here) = after_text {
        let style = drawn_style(editor, live, cursor);
        grid.put_str(here, y, " ", style, room(here).min(1));
    }
    drawn_at
}

impl Look {
    /// The boxed line, screen row and column the cursor at `at` is drawn
    /// at (step TB1), if `at` is in a boxed table's cell.
    pub fn boxed_spot(&self, at: usize) -> Option<(usize, usize, usize)> {
        let b = self
            .boxed
            .partition_point(|boxed| boxed.start <= at)
            .checked_sub(1)?;
        let (k, c) = self.boxed[b].spot(at)?;
        Some((b, k, c))
    }

    /// Whether the cursor at `at` is drawn on `row`, which starts at
    /// `start` (step TB1): in a boxed table's cell, on the screen row of
    /// the cell's text holding it (the line's own row, or one of its
    /// wrapped rows, which are drawn rows before the next line);
    /// elsewhere as `Row::contains` says. Every walk that looks for the
    /// cursor's row asks this, so drawing and scrolling agree.
    pub fn holds(&self, start: RowStart, row: &Row, at: usize) -> bool {
        match self.boxed_spot(at) {
            Some((b, 0, _)) => !row.drawn && row.range.start == self.boxed[b].start,
            Some((b, k, _)) => matches!(
                self.drawn_row(start),
                Some(&Drawn::Cells { boxed, row }) if boxed == b && row == k
            ),
            None => row.contains(at),
        }
    }

    /// Where a click `x` columns into a boxed table line lands (step LW2):
    /// on the grapheme drawn there in the cell under it, else at that
    /// cell's text's end; on the `|---|` line, its start.
    pub fn click_boxed(&self, text: &str, boxed: &Boxed, row: usize, x: usize) -> usize {
        let Some(cells) = boxed.rows.get(row) else {
            return boxed.start;
        };
        let prefix: usize = self
            .layout
            .prefix(&boxed.first)
            .iter()
            .map(|(s, _)| display_width(s))
            .sum();
        let mut left = prefix;
        for (c, cell) in cells.iter().enumerate() {
            // `│ ` or ` │ ` before it: 2 columns of the first, 1 of the
            // others belong to the cell before.
            let width = usize::from(boxed.widths[c]);
            let span = width + if c == 0 { 2 } else { 3 };
            if x < left + span || c + 1 == cells.len() {
                let (before, _) = pads(boxed, c, live::drawn_width(text, &self.runs, cell));
                let start = left + span - width + usize::from(before);
                let mut column = 0;
                for (i, grapheme) in text[cell.clone()].grapheme_indices(true) {
                    let at = cell.start + i;
                    column += self.width(at, grapheme, column);
                    if x < start + column {
                        return at;
                    }
                }
                return cell.end;
            }
            left += span;
        }
        boxed.start
    }
}
