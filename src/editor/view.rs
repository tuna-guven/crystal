//! The editor's screen (decision 0045): the note's
//! name on the top row, the text below, a key line at the bottom.
//!
//! The text is drawn as written, never rendered. It wraps by grapheme: a
//! row ends where the next grapheme would not fit, or at a line break.
//! One column on the right stays free for the cursor at a row's end. A tab
//! advances to the next multiple of 4 columns. A control or BiDi character,
//! a grapheme of no width, or one that carries an invisible character
//! (`hides_something`) is drawn spelled out as a dimmed `<U+XXXX>`
//! (decision 0037): nothing hidden reaches the
//! terminal or disguises the text, and every cursor position is visible.
//! The terminal's own cursor stays hidden; the cursor is a reversed,
//! underlined cell, and the selection is reversed.
//!
//! A gutter left of the text shows the line numbers the viewer shows
//! (`model::lines`, decision 0055): dim, right-aligned, on the first row
//! of each line with text; none on blank lines or wrapped rows. It is as
//! wide as the last number, and left out if fewer than `MIN_TEXT` columns
//! would stay for the text. Its width and first number cost a pass over
//! the text and one over the text above the screen per frame: under 1 ms
//! for 8 MiB of long lines, about 18 ms each for 8 MiB of the shortest
//! (measured in a release build). Accepted, as above: real notes are far
//! smaller, and a kept count would need keeping in step with every edit.
//!
//! The mouse (decision 0073) finds the grapheme drawn at a cell by
//! walking the rows of the last frame again (`View::position_at`), the
//! same walk drawing does: no map of cells is kept.
//!
//! Scrolling works from the first row shown, so a key costs the rows on
//! screen and the logical line around the cursor, not the whole note. The
//! cursor's line is walked from its start on every key: linear in that
//! line, so typing in one line of megabytes is slow. Accepted: a cache of
//! row starts would fix it at the cost of state kept in step with every
//! edit, and real notes have short lines.

use std::collections::VecDeque;
use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use super::Editor;
use crate::model::lines;
use crate::ui::grid::{Color, Grid, Mods, Style, grapheme_width, hides_something, is_forbidden};
use crate::ui::layout::Rect;
use crate::ui::widgets::status_line;

/// The title bar: the note's name in bold on the terminal's background.
const TITLE: Style = Style::PLAIN.mods(Mods::BOLD);
const HINT: Style = Style::PLAIN.mods(Mods::DIM);
const NOTICE: Style = Style::PLAIN.fg(Color::Yellow);
const SELECTED: Style = Style::PLAIN.mods(Mods::REVERSED);
const CURSOR: Style = Style::PLAIN.mods(Mods::REVERSED.union(Mods::UNDERLINED));
const HINTS: &str = "Ctrl+S save · Ctrl+Q close · Ctrl+E view · Ctrl+C/X/V copy, cut, \
                     paste · Ctrl+Z/Y undo, redo";
const TAB_STOP: usize = 4;
/// Line numbers are shown only if this many columns stay for the text, as
/// in the viewer.
const MIN_TEXT: u16 = 20;
/// `<U+`, up to 6 hex digits, `>`.
const ESCAPE_LEN: usize = 10;

/// How one grapheme is drawn.
enum Shape<'a> {
    /// `\n`, or `\r\n` (shown as one line break, as the pager does).
    Break,
    Tab,
    Text(&'a str, usize),
    /// Spelled out, each character as `<U+XXXX>`.
    Escape,
}

fn shape(grapheme: &str) -> Shape<'_> {
    match grapheme {
        "\n" | "\r\n" => Shape::Break,
        "\t" => Shape::Tab,
        _ => match usize::from(grapheme_width(grapheme)) {
            width
                if width > 0 && !grapheme.contains(is_forbidden) && !hides_something(grapheme) =>
            {
                Shape::Text(grapheme, width)
            }
            _ => Shape::Escape,
        },
    }
}

/// `c` as `<U+XXXX>` (4 to 6 hex digits), written into `buf`: no
/// allocation and no format string for note text.
fn escape(c: char, buf: &mut [u8; ESCAPE_LEN]) -> &str {
    let code = u32::from(c);
    let digits = (32 - code.leading_zeros()).div_ceil(4).max(4) as usize;
    buf[..3].copy_from_slice(b"<U+");
    for i in 0..digits {
        let nibble = (code >> (4 * (digits - 1 - i))) & 0xf;
        buf[3 + i] = b"0123456789ABCDEF"[nibble as usize];
    }
    buf[3 + digits] = b'>';
    std::str::from_utf8(&buf[..digits + 4]).unwrap_or("?")
}

fn escape_width(grapheme: &str) -> usize {
    let mut buf = [0; ESCAPE_LEN];
    grapheme.chars().map(|c| escape(c, &mut buf).len()).sum()
}

/// The columns `grapheme` takes when it starts at `column`.
fn width_at(grapheme: &str, column: usize) -> usize {
    match shape(grapheme) {
        Shape::Break => 0,
        Shape::Tab => TAB_STOP - column % TAB_STOP,
        Shape::Text(_, width) => width,
        Shape::Escape => escape_width(grapheme),
    }
}

/// One screen row of text.
struct Row {
    /// The bytes it shows (without its line break).
    range: Range<usize>,
    /// Where the next row starts; `None` after the text's last row.
    next: Option<usize>,
    /// Ended by a line break or the end of the text, not by wrapping.
    ends_line: bool,
}

impl Row {
    /// Whether the cursor at `at` is drawn on this row. At a wrap the
    /// position belongs to the next row; at a line break, to this one.
    fn contains(&self, at: usize) -> bool {
        let end = if self.ends_line {
            self.next.unwrap_or(usize::MAX)
        } else {
            self.range.end
        };
        self.range.start <= at && at < end
    }
}

/// The row starting at `start` (a row start), `width` columns wide. A
/// grapheme wider than a whole row gets a row of its own.
fn row_at(text: &str, start: usize, width: usize) -> Row {
    let rest = text.get(start..).unwrap_or("");
    let mut column = 0;
    for (i, grapheme) in rest.grapheme_indices(true) {
        let at = start + i;
        if matches!(shape(grapheme), Shape::Break) {
            let next = at + grapheme.len();
            return Row {
                range: start..at,
                next: Some(next),
                ends_line: true,
            };
        }
        let w = width_at(grapheme, column);
        if column > 0 && column + w > width {
            return Row {
                range: start..at,
                next: Some(at),
                ends_line: false,
            };
        }
        column += w;
    }
    Row {
        range: start..text.len(),
        next: None,
        ends_line: true,
    }
}

fn line_start(text: &str, at: usize) -> usize {
    text.get(..at)
        .and_then(|head| head.rfind('\n'))
        .map_or(0, |i| i + 1)
}

/// The start of the row the cursor at `at` is drawn on.
fn row_start(text: &str, at: usize, width: usize) -> usize {
    let mut start = line_start(text, at);
    loop {
        let row = row_at(text, start, width);
        match row.next {
            Some(next) if !row.contains(at) => start = next,
            _ => return start,
        }
    }
}

/// The editor's scroll position.
#[derive(Default)]
pub struct View {
    /// Where the first row shown starts.
    top: usize,
    /// Where the last frame drew the text (right of the gutter); `None`
    /// before the first frame or when the screen had no room for it.
    area: Option<Rect>,
}

impl View {
    /// Draws `editor` on `grid`, with `name` on the top row, and tells the
    /// editor how many rows a page is.
    pub fn draw(&mut self, grid: &mut Grid, editor: &mut Editor, name: &str) {
        let area = grid.area();
        let (title, rest) = area.split_top(1);
        let (body, bottom) = rest.split_bottom(1);
        let rows = usize::from(body.height);
        editor.set_page(rows);

        // `[+]` stays visible however long the name.
        let unsaved = if editor.is_dirty() { "[+]" } else { "" };
        status_line(grid, title, name, unsaved, TITLE);
        match editor.notice() {
            Some(notice) => grid.put_str(bottom.x, bottom.y, notice, NOTICE, bottom.width),
            None => grid.put_str(bottom.x, bottom.y, HINTS, HINT, bottom.width),
        };
        if rows == 0 || body.width < 2 {
            self.area = None;
            return;
        }

        let text = editor.text();
        // The gutter: the last number's digits and a space.
        let digits = lines::count(text).max(1).ilog10() + 1;
        let gutter = u16::try_from(digits).unwrap_or(u16::MAX).saturating_add(1);
        let gutter = if body.width >= gutter.saturating_add(MIN_TEXT) {
            gutter
        } else {
            0
        };
        let area = Rect::new(body.x + gutter, body.y, body.width - gutter, body.height);
        let width = usize::from(area.width - 1);
        self.area = Some(area);
        self.scroll(text, editor.cursor(), width, rows);
        // The numbers given before the first row shown.
        let mut number = lines::count(&text[..line_start(text, self.top)]);
        let mut start = Some(self.top);
        for y in area.y..area.bottom() {
            let Some(row_start) = start else { break };
            let row = row_at(text, row_start, width);
            let starts_line = row_start == 0 || text.as_bytes()[row_start - 1] == b'\n';
            let line = text[row_start..].split_inclusive('\n').next().unwrap_or("");
            if starts_line && !lines::is_blank(line) {
                number += 1;
                if gutter > 0 {
                    let shown = format!("{number:>width$}", width = gutter as usize - 1);
                    grid.put_str(body.x, y, &shown, HINT, gutter - 1);
                }
            }
            draw_row(grid, Rect::new(area.x, y, area.width, 1), editor, &row);
            start = row.next;
        }
    }

    /// Where the grapheme drawn at the cell (`column`, `row`) in the last
    /// frame starts in `text`, for the mouse (decision 0073): left of the
    /// text (the gutter) is its row's start; right of it, the row's end
    /// (on a wrapped row, its last grapheme, so the cursor stays on that
    /// row); below the text, the text's end. A row above or below the text
    /// area (the title and key lines) is `None`, unless `clamp`, which
    /// takes the first or last row shown: a drag stops at the screen's
    /// edge. `None` before the first frame. Always a char boundary in
    /// `text`, even when `text` changed since that frame.
    pub fn position_at(&self, text: &str, column: u16, row: u16, clamp: bool) -> Option<usize> {
        let area = self.area?;
        if !clamp && !(area.y..area.bottom()).contains(&row) {
            return None;
        }
        let y = row.clamp(area.y, area.bottom() - 1);
        let width = usize::from(area.width - 1);
        let top = if text.is_char_boundary(self.top) {
            row_start(text, self.top, width)
        } else {
            0
        };
        let mut start = top;
        for _ in area.y..y {
            match row_at(text, start, width).next {
                Some(next) => start = next,
                None => return Some(text.len()),
            }
        }
        let row = row_at(text, start, width);
        let target = usize::from(column.saturating_sub(area.x));
        // The column kept free on the right: the line's end, where the
        // cursor is drawn there.
        if target >= width && row.ends_line {
            return Some(row.range.end);
        }
        let mut column = 0;
        let mut last = row.range.start;
        for (i, grapheme) in text[row.range.clone()].grapheme_indices(true) {
            last = row.range.start + i;
            column += width_at(grapheme, column);
            if target < column {
                return Some(last);
            }
        }
        Some(if row.ends_line { row.range.end } else { last })
    }

    /// Moves `top` so the cursor's row is on screen.
    fn scroll(&mut self, text: &str, cursor: usize, width: usize, rows: usize) {
        if !text.is_char_boundary(self.top) {
            self.top = 0;
        }
        // An edit or a new width may have moved the row starts.
        self.top = row_start(text, self.top, width);
        if cursor < self.top {
            self.top = row_start(text, cursor, width);
            return;
        }
        let mut start = self.top;
        for _ in 0..rows {
            let row = row_at(text, start, width);
            if row.contains(cursor) {
                return;
            }
            match row.next {
                Some(next) => start = next,
                None => return,
            }
        }
        // Below the screen: the cursor's row becomes the last one shown.
        self.top = top_for(text, cursor, width, rows);
    }
}

/// The start of the row `rows - 1` rows above the cursor's row, or the
/// text's first row, so the cursor's row is the last one shown.
///
/// Walks each logical line forward once, keeping only the last row starts
/// it needs: on a line of a million rows, finding the row above by
/// walking from the line's start again for every screen row took seconds
/// per key (found by review).
fn top_for(text: &str, cursor: usize, width: usize, rows: usize) -> usize {
    let mut need = rows.max(1);
    let mut line = line_start(text, cursor);
    // On the cursor's line, the walk stops at the cursor's row; on the
    // lines above, at their end.
    let mut until = Some(cursor);
    loop {
        let mut last = VecDeque::with_capacity(need);
        let mut start = line;
        loop {
            if last.len() == need {
                last.pop_front();
            }
            last.push_back(start);
            let row = row_at(text, start, width);
            let done = row.ends_line || until.is_some_and(|at| row.contains(at));
            match row.next {
                Some(next) if !done => start = next,
                _ => break,
            }
        }
        let first = last.front().copied().unwrap_or(line);
        if last.len() == need || line == 0 {
            return first;
        }
        need -= last.len();
        // `line - 1` is the `\n` ending the line above (also inside `\r\n`).
        line = line_start(text, line - 1);
        until = None;
    }
}

/// Draws one row: its graphemes, the selection, and the cursor (after the
/// last grapheme when it sits at the line's end).
fn draw_row(grid: &mut Grid, area: Rect, editor: &Editor, row: &Row) {
    let text = editor.text();
    let selection = editor.selection().unwrap_or(0..0);
    let style_at = |at: usize| {
        if at == editor.cursor() {
            CURSOR
        } else if selection.contains(&at) {
            SELECTED
        } else {
            Style::PLAIN
        }
    };
    let mut column = 0;
    let mut buf = [0; ESCAPE_LEN];
    for (i, grapheme) in text[row.range.clone()].grapheme_indices(true) {
        let at = row.range.start + i;
        let style = style_at(at);
        let x = area
            .x
            .saturating_add(u16::try_from(column).unwrap_or(u16::MAX));
        let room = area.width.saturating_sub(x - area.x);
        match shape(grapheme) {
            Shape::Text(g, _) => {
                grid.put_str(x, area.y, g, style, room);
            }
            Shape::Tab => {
                let w = width_at(grapheme, column);
                grid.put_str(x, area.y, &"    "[..w], style, room);
            }
            Shape::Escape => {
                let dim = style.mods(style.mods.union(Mods::DIM));
                let mut x = x;
                for c in grapheme.chars() {
                    let room = area.width.saturating_sub(x - area.x);
                    x += grid.put_str(x, area.y, escape(c, &mut buf), dim, room);
                }
            }
            Shape::Break => {}
        }
        column += width_at(grapheme, column);
    }
    // The line's end: the cursor there, or a selected line break.
    if row.ends_line {
        let style = style_at(row.range.end);
        let shows_break = row.next.is_some() && style == SELECTED;
        if style == CURSOR || shows_break {
            // The column kept free on the right holds it, unless a single
            // grapheme wider than the row filled the row: then the last
            // cell, so the cursor is never off screen.
            let column = u16::try_from(column).unwrap_or(u16::MAX);
            let x = area.x + column.min(area.width - 1);
            grid.put_str(x, area.y, " ", style, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::keys::{Key, MouseEvent};
    use crate::ui::clipboard::Clipboard;
    use crate::ui::grid::{is_invisible, is_pictographic};
    use proptest::prelude::*;

    fn rows(grid: &Grid) -> Vec<String> {
        (0..grid.height())
            .map(|y| {
                let row: String = (0..grid.width())
                    .map(|x| grid.cell(x, y).unwrap().symbol())
                    .collect();
                row.trim_end().to_owned()
            })
            .collect()
    }

    /// Where the cursor is drawn: one run of underlined cells on one row
    /// (only the cursor is underlined; on a hidden character it covers
    /// the whole spelled-out `<U+XXXX>`, also dimmed). Its first cell, or
    /// `None` if it is missing or drawn in more than one place.
    fn cursor_cell(grid: &Grid) -> Option<(u16, u16)> {
        let cells: Vec<(u16, u16)> = (0..grid.height())
            .flat_map(|y| (0..grid.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let mods = grid.cell(x, y).unwrap().style().mods;
                mods.contains(Mods::UNDERLINED)
            })
            .collect();
        let &(x, y) = cells.first()?;
        let one_run = cells
            .iter()
            .enumerate()
            .all(|(i, &(cx, cy))| cy == y && usize::from(cx) == usize::from(x) + i);
        one_run.then_some((x, y))
    }

    fn drawn(ed: &mut Editor, view: &mut View, width: u16, height: u16) -> Grid {
        let mut grid = Grid::new(width, height);
        view.draw(&mut grid, ed, "work/plan");
        grid
    }

    /// Cells map back to the grapheme drawn there (decision 0073): the
    /// gutter to the row's start, a tab's or a wide character's every
    /// cell to it, a spelled-out character's every cell to it, past a
    /// line's end to its end, below the text to the text's end; the title
    /// and key lines to nothing unless clamped.
    #[test]
    fn cells_map_to_what_is_drawn_there() {
        let mut ed = Editor::new("a\tb\n日x\n\u{202e}z").unwrap();
        let mut view = View::default();
        assert_eq!(
            view.position_at(ed.text(), 2, 1, true),
            None,
            "no frame yet"
        );
        // 30 by 6: the title on row 0, text on rows 1 to 4 right of the
        // gutter's `1 `, the key line on row 5.
        drawn(&mut ed, &mut view, 30, 6);
        let at = |x, y| view.position_at(ed.text(), x, y, false);
        let cases = [
            ((0, 1), 0),  // gutter
            ((2, 1), 0),  // a
            ((3, 1), 1),  // tab, first cell
            ((5, 1), 1),  // tab, last cell
            ((6, 1), 2),  // b
            ((7, 1), 3),  // past the line's end
            ((3, 2), 4),  // 日, second cell
            ((4, 2), 7),  // x
            ((29, 2), 8), // the far right
            ((9, 2), 8),
            ((2, 3), 9),   // <U+202E>, first cell
            ((9, 3), 9),   // <U+202E>, last cell
            ((10, 3), 12), // z
            ((2, 4), 13),  // below the text
        ];
        for ((x, y), want) in cases {
            assert_eq!(at(x, y), Some(want), "cell {x},{y}");
        }
        assert_eq!(at(2, 0), None, "title");
        assert_eq!(at(2, 5), None, "key line");
        assert_eq!(view.position_at(ed.text(), 2, 0, true), Some(0));
        assert_eq!(view.position_at(ed.text(), 2, 5, true), Some(13));
    }

    /// Past a wrapped row's end is its last grapheme, so a click keeps the
    /// cursor on that row; past the last row's end is the text's end.
    #[test]
    fn past_a_wrapped_row_is_its_last_grapheme() {
        // 6 columns, no gutter: 5 for the text and one kept free.
        let mut ed = Editor::new("abcdef").unwrap();
        let mut view = View::default();
        drawn(&mut ed, &mut view, 6, 4);
        assert_eq!(rows(&drawn(&mut ed, &mut view, 6, 4))[1..3], ["abcde", "f"]);
        let at = |x, y| view.position_at(ed.text(), x, y, false);
        assert_eq!(at(4, 1), Some(4));
        assert_eq!(at(5, 1), Some(4));
        assert_eq!(at(1, 2), Some(6));
        assert_eq!(at(5, 2), Some(6));
    }

    /// The gutter: the viewer's numbers (blank lines neither numbered nor
    /// counted), right-aligned to the last one, none on a wrapped row;
    /// counted on from the text above when scrolled; left out when fewer
    /// than `MIN_TEXT` columns would stay for the text.
    #[test]
    fn gutter_shows_line_numbers() {
        let words = "word ".repeat(6);
        let text = format!("# T\n\n  \n{words}\n{}", "x\n".repeat(8));
        let mut ed = Editor::new(&text).unwrap();
        let mut view = View::default();
        let shown = rows(&drawn(&mut ed, &mut view, 24, 8));
        assert_eq!(
            shown[1..7],
            [
                " 1 # T",
                "",
                "",
                " 2 word word word word",
                "   word word",
                " 3 x"
            ]
        );
        // The cursor at the end: the screen shows the last rows, and their
        // numbers go on from the lines above it.
        let mut clip = Clipboard::default();
        for _ in 0..20 {
            ed.key(Key::Down, &mut clip);
        }
        let shown = rows(&drawn(&mut ed, &mut view, 24, 8));
        assert_eq!(shown[1..7], [" 6 x", " 7 x", " 8 x", " 9 x", "10 x", ""]);
        // 21 columns leave 18 for the text with the gutter: none.
        let mut ed = Editor::new(&text).unwrap();
        let shown = rows(&drawn(&mut ed, &mut View::default(), 21, 4));
        assert_eq!(shown[1], "# T");
    }

    #[test]
    fn wraps_by_grapheme_and_expands_tabs() {
        let mut ed = Editor::new("abcdef\n\tx\n日本語").unwrap();
        // 6 columns: 5 for text, one kept for the cursor.
        let grid = drawn(&mut ed, &mut View::default(), 6, 7);
        assert_eq!(
            rows(&grid)[..6],
            ["work/p", "abcde", "f", "    x", "日本", "語"]
        );
        assert_eq!(cursor_cell(&grid), Some((0, 1)));
    }

    #[test]
    fn hidden_characters_are_spelled_out_dimmed() {
        let mut ed = Editor::new("a\u{202e}b\u{1b}[2J\u{200b}").unwrap();
        let grid = drawn(&mut ed, &mut View::default(), 40, 3);
        assert_eq!(rows(&grid)[1], "1 a<U+202E>b<U+001B>[2J<U+200B>");
        let dimmed = grid.cell(4, 1).unwrap().style().mods;
        assert!(dimmed.contains(Mods::DIM));
    }

    /// Invisible characters riding on a visible grapheme are spelled out
    /// with it; the joiners and selector that emoji need are not.
    #[test]
    fn invisible_characters_inside_a_grapheme_are_spelled_out() {
        let shown = |text: &str| {
            let mut ed = Editor::new(text).unwrap();
            let grid = drawn(&mut ed, &mut View::default(), 80, 3);
            // After the gutter's `1 `.
            rows(&grid)[1].strip_prefix("1 ").unwrap().to_owned()
        };
        // A tag character, a variation selector, a joiner after a letter,
        // the combining grapheme joiner, a trailing joiner after an emoji.
        assert_eq!(shown("a\u{E0041}"), "<U+0061><U+E0041>");
        assert_eq!(shown("x\u{FE01}"), "<U+0078><U+FE01>");
        assert_eq!(shown("x\u{FE0F}"), "<U+0078><U+FE0F>");
        assert_eq!(shown("a\u{200D}"), "<U+0061><U+200D>");
        assert_eq!(shown("e\u{34F}"), "<U+0065><U+034F>");
        assert_eq!(shown("😀\u{200D}"), "<U+1F600><U+200D>");
        // Hidden bytes in an emoji (variation selectors supplement).
        assert_eq!(shown("😀\u{E0101}"), "<U+1F600><U+E0101>");
        // A long payload is clipped at the row's end, not hidden.
        let long = format!("a{}", "\u{E0041}".repeat(1000));
        assert!(shown(&long).starts_with("<U+0061><U+E0041><U+E0041>"));
        // Hangul fillers draw as blanks: spelled out even alone.
        assert_eq!(shown("a\u{3164}b"), "a<U+3164>b");
        assert_eq!(shown("a\u{FFA0}b"), "a<U+FFA0>b");
        // Emoji stay emoji; accents stay accents.
        for kept in [
            "👨\u{200D}💻",
            "❤\u{FE0F}",
            "1\u{FE0F}\u{20E3}",
            "🏳\u{FE0F}\u{200D}🌈",
            "e\u{301}",
        ] {
            assert_eq!(shown(kept), kept, "{kept:?}");
        }
    }

    #[test]
    fn shows_the_selection_the_cursor_at_a_line_end_and_unsaved_changes() {
        let mut ed = Editor::new("ab\ncd").unwrap();
        let mut clip = Clipboard::default();
        for key in [Key::ShiftRight, Key::ShiftRight, Key::ShiftRight] {
            ed.key(key, &mut clip);
        }
        let grid = drawn(&mut ed, &mut View::default(), 10, 4);
        assert_eq!(grid.cell(0, 1).unwrap().style(), SELECTED);
        assert_eq!(
            grid.cell(2, 1).unwrap().style(),
            SELECTED,
            "the selected line break"
        );
        assert_eq!(cursor_cell(&grid), Some((0, 2)));
        ed.key(Key::End, &mut clip);
        ed.key(Key::Char('!'), &mut clip);
        let grid = drawn(&mut ed, &mut View::default(), 10, 4);
        assert_eq!(rows(&grid)[0], "work/p [+]", "[+] kept, the name cut");
        assert_eq!(cursor_cell(&grid), Some((3, 2)), "after the last character");
    }

    #[test]
    fn scrolls_to_keep_the_cursor_on_screen() {
        let text: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let mut ed = Editor::new(&text).unwrap();
        let mut clip = Clipboard::default();
        let mut view = View::default();
        // 3 text rows between the title and the key line.
        drawn(&mut ed, &mut view, 20, 5);
        for _ in 0..10 {
            ed.key(Key::Down, &mut clip);
        }
        let grid = drawn(&mut ed, &mut view, 20, 5);
        assert_eq!(rows(&grid)[1..4], ["line 8", "line 9", "line 10"]);
        assert_eq!(cursor_cell(&grid), Some((0, 3)));
        for _ in 0..9 {
            ed.key(Key::Up, &mut clip);
        }
        let grid = drawn(&mut ed, &mut view, 20, 5);
        assert_eq!(rows(&grid)[1], "line 1");
        assert_eq!(cursor_cell(&grid), Some((0, 1)));
    }

    #[test]
    fn notices_replace_the_key_line() {
        let mut ed = Editor::new("").unwrap();
        ed.key(Key::Ctrl('z'), &mut Clipboard::default());
        let grid = drawn(&mut ed, &mut View::default(), 40, 3);
        assert_eq!(rows(&grid)[2], "Nothing to undo.");
    }

    /// The scroll's first version, kept as the reference: from the
    /// cursor's row, step to the row above `rows - 1` times, each found by
    /// walking its line from the start. Right, but quadratic on long lines.
    fn top_by_rows(text: &str, cursor: usize, width: usize, rows: usize) -> usize {
        let mut top = row_start(text, cursor, width);
        for _ in 1..rows {
            let Some(c) = text.get(..top).and_then(|head| head.chars().next_back()) else {
                break;
            };
            top = row_start(text, top - c.len_utf8(), width);
        }
        top
    }

    /// Found by review: End on a 256 KiB line took 7.7 s in a debug build
    /// (14 s on 8 MiB in release), as every screen row walked the line
    /// again. One walk takes well under a second; the bound leaves room
    /// for a slow machine and still fails the old way.
    #[test]
    fn scrolling_down_a_long_line_walks_it_once() {
        let mut ed = Editor::new(&"a".repeat(256 * 1024)).unwrap();
        let mut clip = Clipboard::default();
        let mut view = View::default();
        drawn(&mut ed, &mut view, 80, 52);
        let started = std::time::Instant::now();
        ed.key(Key::End, &mut clip);
        let grid = drawn(&mut ed, &mut view, 80, 52);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        // 77 characters a row: the gutter's `1 `, then one column kept
        // for the cursor.
        let column = u16::try_from(2 + 256 * 1024 % 77).unwrap();
        assert_eq!(
            cursor_cell(&grid),
            Some((column, 50)),
            "after the text, on the last text row"
        );
    }

    proptest! {
        /// The single walk picks the same first row as the reference, on
        /// any text, cursor, width and height.
        #[test]
        fn top_for_matches_the_reference(
            text in "[ab\t\n\r\u{1b}\u{301}日😀]{0,80}",
            pick in any::<prop::sample::Index>(),
            width in 1usize..12,
            rows in 1usize..8,
        ) {
            let mut stops: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
            stops.push(text.len());
            let cursor = stops[pick.index(stops.len())];
            prop_assert_eq!(
                top_for(&text, cursor, width, rows),
                top_by_rows(&text, cursor, width, rows)
            );
        }

        /// Any text, any keys, any screen size: the cursor is drawn once and
        /// on a text row, and no cell holds a character the grid had to
        /// replace (everything hidden was spelled out first).
        #[test]
        fn the_cursor_is_always_drawn_once(
            text in "[ab\t\n\r\u{1b}\u{202e}\u{200b}\u{301}日😀]{0,60}",
            moves in prop::collection::vec(0u8..6, 0..40),
            width in 2u16..30,
            height in 3u16..12,
        ) {
            let mut ed = Editor::new(&text).unwrap();
            let mut clip = Clipboard::default();
            let mut view = View::default();
            for m in moves {
                let key = match m {
                    0 => Key::Left,
                    1 => Key::Right,
                    2 => Key::Up,
                    3 => Key::Down,
                    4 => Key::PageDown,
                    _ => Key::Char('x'),
                };
                ed.key(key, &mut clip);
                let grid = drawn(&mut ed, &mut view, width, height);
                let cell = cursor_cell(&grid);
                prop_assert!(cell.is_some_and(|(_, y)| y >= 1 && y < height - 1), "{cell:?}");
                for y in 1..height - 1 {
                    for x in 0..width {
                        prop_assert_ne!(grid.cell(x, y).unwrap().symbol(), "?");
                    }
                }
            }
        }

        /// Any text, screen size and scroll: every cell maps to a char
        /// boundary inside the text, a press there leaves the cursor on a
        /// grapheme boundary, and the cell the cursor is drawn on maps back
        /// to the cursor (decision 0073).
        #[test]
        fn every_cell_maps_inside_the_text(
            text in "[ab\t\n\r\u{1b}\u{202e}\u{200b}\u{301}日😀]{0,60}",
            downs in 0usize..20,
            width in 2u16..30,
            height in 3u16..12,
        ) {
            let mut ed = Editor::new(&text).unwrap();
            let mut clip = Clipboard::default();
            let mut view = View::default();
            for _ in 0..downs {
                ed.key(Key::Down, &mut clip);
            }
            let grid = drawn(&mut ed, &mut view, width, height);
            let (cx, cy) = cursor_cell(&grid).unwrap();
            prop_assert_eq!(view.position_at(&text, cx, cy, false), Some(ed.cursor()));
            let mut bounds: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
            bounds.push(text.len());
            for y in 0..height {
                for x in 0..width {
                    let Some(at) = view.position_at(&text, x, y, true) else {
                        prop_assert!(false, "a clamped cell maps");
                        continue;
                    };
                    prop_assert!(text.is_char_boundary(at), "{at}");
                    let mut ed = Editor::new(&text).unwrap();
                    ed.mouse(MouseEvent::Press, Some(at), std::time::Instant::now());
                    prop_assert!(bounds.contains(&ed.cursor()), "{}", ed.cursor());
                }
            }
        }

        /// Any presses (double and triple clicks too), drags, releases and
        /// wheel steps on any cells, also
        /// after the text changed under the last frame: the cursor stays on a
        /// grapheme boundary and the selection inside the text.
        #[test]
        fn any_mouse_keeps_cursor_and_selection_valid(
            text in "[ab\t\n\r\u{202e}\u{301}日😀]{0,60}",
            events in prop::collection::vec((0u8..6, 0u16..40, 0u16..14, 0u64..600), 0..40),
            width in 2u16..30,
            height in 3u16..12,
        ) {
            let mut ed = Editor::new(&text).unwrap();
            let mut clip = Clipboard::default();
            let mut view = View::default();
            drawn(&mut ed, &mut view, width, height);
            // Gaps of 0 to 600 ms: double and triple clicks happen.
            let mut now = std::time::Instant::now();
            for (kind, x, y, gap) in events {
                now += std::time::Duration::from_millis(gap);
                let event = match kind {
                    0 => MouseEvent::Press,
                    1 | 2 => MouseEvent::Drag,
                    3 => MouseEvent::Release,
                    4 => MouseEvent::WheelDown,
                    _ => {
                        // Typing between frames moves the text under them.
                        ed.key(Key::Char('日'), &mut clip);
                        continue;
                    }
                };
                let at = view.position_at(ed.text(), x, y, event != MouseEvent::Press);
                ed.mouse(event, at, now);
                let text = ed.text();
                let on_boundary = |at: usize| {
                    at == text.len() || text.grapheme_indices(true).any(|(i, _)| i == at)
                };
                prop_assert!(on_boundary(ed.cursor()));
                if let Some(range) = ed.selection() {
                    prop_assert!(range.end <= text.len());
                    prop_assert!(on_boundary(range.start) && on_boundary(range.end));
                }
                if kind == 2 {
                    drawn(&mut ed, &mut view, width, height);
                }
            }
        }

        /// No cell shows an invisible character: each is spelled out,
        /// except a joiner or U+FE0F inside a cell that starts an emoji
        /// (or is a keycap).
        #[test]
        fn no_cell_shows_an_invisible_character(
            text in "[ae\u{301}\u{E0041}\u{E0101}\u{200B}\u{200C}\u{200D}\u{FE01}\u{FE0F}\
                      \u{34F}\u{3164}\u{FFA0}\u{115F}\u{1160}\u{2060}\u{FEFF}1\u{20E3}\
                      😀👨💻❤\n]{0,40}",
        ) {
            let mut ed = Editor::new(&text).unwrap();
            // Wide and tall enough that every row of the text is shown.
            let grid = drawn(&mut ed, &mut View::default(), 120, 120);
            for y in 1..grid.height() - 1 {
                for x in 0..grid.width() {
                    let cell = grid.cell(x, y).unwrap().symbol();
                    let emoji = cell.starts_with(is_pictographic) || cell.ends_with('\u{20E3}');
                    for c in cell.chars().filter(|&c| is_invisible(c)) {
                        prop_assert!(
                            emoji && matches!(c, '\u{200D}' | '\u{FE0F}'),
                            "U+{:04X} shown in {:?}",
                            u32::from(c),
                            cell
                        );
                    }
                }
            }
        }
    }
}
