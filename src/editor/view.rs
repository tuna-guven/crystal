//! The editor's screen (decision 0045): the note's
//! name on the top row, the text below, a key line at the bottom.
//!
//! The text is drawn as written, never rendered, except in live mode
//! (`edit --live`, steps LE1 to LE7, LR1, LI2): there the text is drawn
//! as `live::parse` says, its markup hidden (zero columns) or replaced by
//! a fixed string, its text styled, except the elements the cursor
//! touches or the selection overlaps, drawn as written; each row starts
//! with the viewer's prefix for its line (`live::layout`), which stands
//! in for the line's leading markup unless the cursor is in it, and a
//! code line ends in its block's gray box; every grapheme shown still goes through the checks below. The
//! runs are made again on every frame, so a note over `LIVE_MAX` is drawn
//! as written. It wraps by grapheme: a
//! row ends where the next grapheme would not fit, or at a line break.
//! One column on the right stays free for the cursor at a row's end. A tab
//! advances to the next multiple of 4 columns. A control or BiDi character,
//! a grapheme of no width, or one that carries an invisible character
//! (`hides_something`) is drawn spelled out as a dimmed `<U+XXXX>`
//! (decision 0037): nothing hidden reaches the
//! terminal or disguises the text, and every cursor position is visible.
//! The terminal's own cursor stays hidden; the cursor is a reversed,
//! underlined cell, and the selection is reversed; a cursor just outside
//! the selection is only underlined.
//!
//! A gutter left of the text shows the line numbers the viewer shows
//! (`model::lines`, decision 0055): dim, right-aligned, on the first row
//! of each line with text; none on blank lines or wrapped rows. It is as
//! wide as the last number, and left out if fewer than `MIN_TEXT` columns
//! would stay for the text, and in live mode, as in the viewer. Its width and first number cost a pass over
//! the text and one over the text above the screen per frame: under 1 ms
//! for 8 MiB of long lines, about 18 ms each for 8 MiB of the shortest
//! (measured in a release build). Accepted, as above: real notes are far
//! smaller, and a kept count would need keeping in step with every edit.
//!
//! The list of link suggestions is drawn over the frame after it
//! (`suggestions.rs`).
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
//!
//! Its parts, one job each (steps RF1 to RF5): `glyph` one grapheme (its
//! width, the escapes of decision 0037); `rows` the row walk every
//! drawing, scroll and click goes through; `look` live mode's look of a
//! frame; `fold` folded sections; `table` boxed tables; `suggestions` the
//! link list. This file keeps `View`: the state between frames, drawing
//! a frame, scrolling, and the mouse.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

mod fold;
mod glyph;
mod look;
mod rows;
mod suggestions;
mod table;

use fold::shift;
use look::{Drawn, Look};
use rows::{Row, RowStart, line_start, row_start, rows, top_for};
use table::draw_boxed;

use super::Editor;
use super::live;
use super::suggest::Source;
use crate::md::render::CODE;
use crate::model::lines;
use crate::pager::{GUTTER, MARKER};
use crate::shell::keys::MouseEvent;
use crate::ui::grid::{Color, Grid, Mods, Style};
use crate::ui::layout::Rect;
use crate::ui::widgets::status_line;

/// The title bar: the note's name in bold on the terminal's background.
const TITLE: Style = Style::PLAIN.mods(Mods::BOLD);
const HINT: Style = Style::PLAIN.mods(Mods::DIM);
const NOTICE: Style = Style::PLAIN.fg(Color::Yellow);
const SELECTED: Style = Style::PLAIN.mods(Mods::REVERSED);
const CURSOR: Style = Style::PLAIN.mods(Mods::REVERSED.union(Mods::UNDERLINED));
/// The cursor just outside a selection (at its end after a drag, a double
/// or a triple click): underlined only, so the cell after the selection
/// does not look selected (decision 0073).
const CURSOR_BESIDE: Style = Style::PLAIN.mods(Mods::UNDERLINED);
const HINTS: &str = "Ctrl+S save · Ctrl+Q close · Ctrl+E view · Ctrl+C/X/V copy, cut, \
                     paste · Ctrl+Z/Y undo, redo";
/// The key line while a list of link suggestions is open: what it lists
/// and its keys (decisions 0045, 0075).
const NOTE_HINTS: &str = "Notes · Up/Down, wheel: choose · Enter, Tab, click: link · Esc close";
const HEADING_HINTS: &str =
    "Headings · Up/Down, wheel: choose · Enter, Tab, click: link · Esc close";
const BLOCK_HINTS: &str = "Blocks · Up/Down, wheel: choose · Enter, Tab, click: link (an ^id \
                           is added if missing) · Esc close";
/// Live mode draws a note only up to this size: the whole note is parsed
/// on every frame, about 62 ms for 1 MiB in a release build (before the
/// line layout, 370 ms for 8 MiB). A larger note is drawn as written.
pub const LIVE_MAX: usize = 1 << 20;
/// Line numbers are shown only if this many columns stay for the text, as
/// in the viewer.
const MIN_TEXT: u16 = 20;
/// What a mouse event in live mode was for (steps LF2, LF3).
#[derive(Debug, PartialEq, Eq)]
pub enum Click {
    /// The view took it: a heading's marker, or the mouse moving.
    Taken,
    /// A press on a link not drawn as written: its target, where it is
    /// written in the text, and whether it is a web link.
    Link { target: Range<usize>, web: bool },
    /// The editor's: the cursor, a selection, the wheel.
    Editor,
}

/// Indents are slices of this.
const SPACES: &str = "                                                                ";

/// The editor's scroll position.
#[derive(Default)]
pub struct View {
    /// Where the first row shown starts.
    top: RowStart,
    /// Where the last frame drew the text (right of the gutter); `None`
    /// before the first frame or when the screen had no room for it.
    area: Option<Rect>,
    /// The cell the last frame drew the cursor in, for the suggestion
    /// list; `None` when it was not drawn.
    cursor: Option<(u16, u16)>,
    /// Where the last frame drew the suggestion list, and the list entry
    /// on its first row, for the mouse; `None` when it was not drawn.
    list: Option<(Rect, usize)>,
    /// `edit --live`: markup styled on lines away from the cursor (steps
    /// LE1 to LE7).
    live: bool,
    /// Live mode's runs, from the last frame; empty when not live.
    look: Look,
    /// Folded headings, by their line's start (step LF1).
    folds: Vec<usize>,
    /// The heading the mouse is over, by its line's start (step LF2).
    hover: Option<usize>,
    /// The section headings the last frame drew: their row and line
    /// start, for the mouse.
    heading_rows: Vec<(u16, usize)>,
    /// The last press was the view's (a marker, a link): its drag and
    /// release are too.
    pressed: bool,
}

impl View {
    /// A view in live mode (`edit --live`).
    pub fn live() -> Self {
        Self {
            live: true,
            ..Self::default()
        }
    }

    /// The mouse in live mode (steps LF2, LF3), before the editor sees it:
    /// a motion report over a section heading's row shows its marker; a
    /// press in the gutter of that row folds or unfolds it; a press on a
    /// link not drawn as written is for the caller to follow. Anything
    /// else is the editor's.
    pub fn mouse(&mut self, text: &str, event: MouseEvent, column: u16, row: u16) -> Click {
        if !self.live {
            return Click::Editor;
        }
        let heading = self
            .heading_rows
            .iter()
            .find(|&&(y, _)| y == row)
            .map(|&(_, start)| start);
        match event {
            MouseEvent::Move => {
                self.hover = heading;
                Click::Taken
            }
            MouseEvent::Press => {
                let click = self.press(text, heading, column, row);
                self.pressed = click != Click::Editor;
                click
            }
            // The drag and release of a press the view took are its own,
            // so the editor neither selects nor clears the notice a link
            // left.
            MouseEvent::Drag | MouseEvent::Release if self.pressed => {
                self.pressed = event == MouseEvent::Drag;
                Click::Taken
            }
            _ => Click::Editor,
        }
    }

    /// A press: on a heading row's gutter it folds; on a link's text (not
    /// past its end, the row's end) it is the link's.
    fn press(&mut self, text: &str, heading: Option<usize>, column: u16, row: u16) -> Click {
        let in_gutter = self.area.is_some_and(|area| column < area.x);
        if let Some(start) = heading.filter(|_| in_gutter) {
            self.toggle_fold(start);
            return Click::Taken;
        }
        let at = self.position_at(text, column, row, false);
        match at.and_then(|at| self.look.link(at)) {
            Some(link) => Click::Link {
                target: link.target.clone(),
                web: link.web,
            },
            None => Click::Editor,
        }
    }

    /// Folds the section of the heading whose line starts at `heading`,
    /// or unfolds it (step LF1). Takes effect on the next frame, which
    /// drops it if no heading with lines to hide is there, or opens it if
    /// the cursor is inside.
    pub fn toggle_fold(&mut self, heading: usize) {
        match self.folds.iter().position(|&fold| fold == heading) {
            Some(i) => {
                self.folds.remove(i);
            }
            None => self.folds.push(heading),
        }
    }

    /// Whether this view was opened live (`edit --live`).
    pub fn is_live(&self) -> bool {
        self.live
    }

    /// Draws `editor` on `grid`, with `name` on the top row, and tells the
    /// editor how many rows a page is.
    pub fn draw(&mut self, grid: &mut Grid, editor: &mut Editor, name: &str) {
        let area = grid.area();
        let (title, rest) = area.split_top(1);
        let (body, bottom) = rest.split_bottom(1);
        let rows = usize::from(body.height);
        editor.set_page(rows);
        // Taken every frame, live or not, so they never pile up; the
        // folds move with them.
        for change in editor.take_changes() {
            shift(&mut self.folds, change);
        }
        self.cursor = None;
        self.list = None;

        let live = self.live && editor.text().len() <= LIVE_MAX;
        // `[+]` and `live` stay visible however long the name.
        let unsaved = match (live, editor.is_dirty()) {
            (true, true) => "live [+]",
            (true, false) => "live",
            (false, true) => "[+]",
            (false, false) => "",
        };
        status_line(grid, title, name, unsaved, TITLE);
        let hints = match editor.suggestions().map(|list| list.source) {
            None => HINTS,
            Some(Source::Notes) => NOTE_HINTS,
            Some(Source::Headings(_)) => HEADING_HINTS,
            Some(Source::Blocks(_)) => BLOCK_HINTS,
        };
        match editor.notice() {
            Some(notice) => grid.put_str(bottom.x, bottom.y, notice, NOTICE, bottom.width),
            None => grid.put_str(bottom.x, bottom.y, hints, HINT, bottom.width),
        };
        if rows == 0 || body.width < 2 {
            self.area = None;
            return;
        }

        let text = editor.text();
        // The gutter: the last number's digits and a space.
        let digits = lines::count(text).max(1).ilog10() + 1;
        let gutter = u16::try_from(digits).unwrap_or(u16::MAX).saturating_add(1);
        // Live mode shows no line numbers: it leaves the viewer's blank
        // gutter, so lines start in the viewer's columns.
        let gutter = if live {
            if body.width >= GUTTER.saturating_add(MIN_TEXT) {
                GUTTER
            } else {
                0
            }
        } else if body.width >= gutter.saturating_add(MIN_TEXT) {
            gutter
        } else {
            0
        };
        let area = Rect::new(body.x + gutter, body.y, body.width - gutter, body.height);
        let width = usize::from(area.width - 1);
        self.area = Some(area);
        self.look = Look::default();
        if live {
            let cursor = editor.cursor();
            let selection = editor.selection().unwrap_or(cursor..cursor);
            let parsed = live::parse(text);
            self.look = Look::new(parsed, text, cursor, &selection, &mut self.folds, width);
        }
        self.scroll(text, editor.cursor(), width, rows);
        // The numbers given before the first row shown.
        let mut number = lines::count(&text[..line_start(text, self.top.line())]);
        self.heading_rows.clear();
        let shown = self::rows(text, self.top, width, &self.look);
        for (y, (next, row)) in (area.y..area.bottom()).zip(shown) {
            // The viewer's added blank row: nothing to draw; a table's
            // border: its pieces.
            if let Some(&Drawn::Cells { boxed, row }) = self.look.drawn_row(next) {
                let row_area = Rect::new(area.x, y, area.width, 1);
                let boxed = &self.look.boxed[boxed];
                if let Some(x) = draw_boxed(grid, row_area, editor, &self.look, boxed, row) {
                    self.cursor = Some((x, y));
                }
                continue;
            }
            if row.drawn {
                let mut x = area.x;
                for (piece, style) in self.look.pieces(next) {
                    x += grid.put_str(x, y, piece, *style, area.right().saturating_sub(x));
                }
                continue;
            }
            let row_start = row.range.start;
            // A section heading's first row: its marker, while hovered.
            let headings = &self.look.layout.headings;
            if headings
                .binary_search_by_key(&row_start, |&(line, _)| line)
                .is_ok()
            {
                self.heading_rows.push((y, row_start));
                if gutter > 0 && self.hover == Some(row_start) {
                    let mark = if self.folds.contains(&row_start) {
                        ">"
                    } else {
                        "v"
                    };
                    grid.put_str(body.x, y, mark, MARKER, 1);
                }
            }
            let starts_line = row_start == 0 || text.as_bytes()[row_start - 1] == b'\n';
            let line = text[row_start..].split_inclusive('\n').next().unwrap_or("");
            if starts_line && !lines::is_blank(line) {
                number += 1;
                if gutter > 0 && !live {
                    let shown = format!("{number:>width$}", width = gutter as usize - 1);
                    grid.put_str(body.x, y, &shown, HINT, gutter - 1);
                }
            }
            let row_area = Rect::new(area.x, y, area.width, 1);
            if let Some(x) = draw_row(grid, row_area, editor, &row, &self.look) {
                self.cursor = Some((x, y));
            }
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
        let top = if text.is_char_boundary(self.top.line()) {
            row_start(text, self.top, width, &self.look)
        } else {
            RowStart::text(0)
        };
        // The row `y` is on; past the text's last row, the text's end.
        let Some((start, row)) = rows(text, top, width, &self.look).nth(usize::from(y - area.y))
        else {
            return Some(text.len());
        };
        // A boxed table's line: the cell under the click (step LW2).
        let x = usize::from(column.saturating_sub(area.x));
        if let Some(boxed) = self.look.boxed_at(row.range.start).filter(|_| !row.drawn) {
            return Some(self.look.click_boxed(text, boxed, 0, x));
        }
        // A wrapped table row's other rows (step LW3).
        if let Some(&Drawn::Cells { boxed, row }) = self.look.drawn_row(start) {
            return Some(self.look.click_boxed(text, &self.look.boxed[boxed], row, x));
        }
        // Live mode's prefix: a click there lands on the row's first
        // grapheme shown.
        let target = usize::from(column.saturating_sub(area.x)).saturating_sub(row.indent);
        // The column kept free on the right: the line's end, where the
        // cursor is drawn there.
        if target >= width - row.indent && row.ends_line {
            return Some(row.range.end);
        }
        let mut column = 0;
        let mut last = row.range.start;
        for (i, grapheme) in text[row.range.clone()].grapheme_indices(true) {
            last = row.range.start + i;
            column += self.look.width(last, grapheme, column);
            if target < column {
                return Some(last);
            }
        }
        Some(if row.ends_line { row.range.end } else { last })
    }

    /// The screen row, from the text's first (0), the line holding `at`
    /// starts on as last drawn; negative when it starts above the screen
    /// (the cursor on a wrapped row of a long line). For Ctrl+E: the
    /// preview puts the line on the same row. `None` before the first
    /// frame or when the line is not on screen.
    pub fn line_row(&self, text: &str, at: usize) -> Option<isize> {
        let area = self.area?;
        let width = usize::from(area.width - 1);
        if !text.is_char_boundary(self.top.line()) || !text.is_char_boundary(at) {
            return None;
        }
        let line = line_start(text, at);
        let height = usize::from(area.height);
        // On screen: the row the line starts on.
        let shown = rows(text, self.top, width, &self.look).take(height);
        if let Some(y) = shown
            .map(|(_, row)| row)
            .position(|row| !row.drawn && row.range.start == line)
        {
            return isize::try_from(y).ok();
        }
        // Above the screen: how many of its rows come before the first
        // one shown.
        rows(text, RowStart::text(line), width, &self.look)
            .take(height + 1)
            .position(|(start, _)| start == self.top)
            .filter(|&above| above > 0)
            .and_then(|above| isize::try_from(above).ok())
            .map(|above| -above)
    }

    /// Moves `top` so the cursor's row is on screen.
    fn scroll(&mut self, text: &str, cursor: usize, width: usize, rows: usize) {
        if !text.is_char_boundary(self.top.line()) {
            self.top = RowStart::text(0);
        }
        // An edit or a new width may have moved the row starts. At the
        // text's start, the rows before its first line show (a table's
        // top border).
        let live = &self.look;
        self.top = row_start(text, self.top, width, live);
        if self.top == RowStart::text(0) {
            self.top = live.first_row(0);
        }
        // On screen, or above the text's last row: nothing to do. The
        // first row shown may be a wrapped table row's further row, which
        // goes before the next line, so a cursor before `top`'s line can
        // still be on screen (step TB1).
        let above = cursor < self.top.line();
        for (start, row) in self::rows(text, self.top, width, live).take(rows) {
            if live.holds(start, &row, cursor) || (!above && row.next.is_none()) {
                return;
            }
        }
        if above {
            self.top = row_start(text, RowStart::text(cursor), width, live);
            return;
        }
        // Below the screen: the cursor's row becomes the last one shown.
        self.top = top_for(text, cursor, width, rows, live);
    }
}

/// Draws the grapheme at `at`, which starts `column` columns into its
/// row, as `live` says (`glyph::put`): its show and, on a web link, the
/// address the terminal opens. Returns the columns it took.
#[allow(clippy::too_many_arguments)]
fn put_grapheme(
    grid: &mut Grid,
    cell: (u16, u16, u16),
    text: &str,
    at: usize,
    grapheme: &str,
    column: usize,
    style: Style,
    live: &Look,
) -> usize {
    let link = live
        .link(at)
        .filter(|link| link.web)
        .and_then(|link| text.get(link.target.clone()));
    glyph::put(grid, cell, grapheme, column, style, live.at(at).0, link)
}

/// The style of the grapheme at `at`: the cursor's, the selection's, or
/// as `live` draws it. The cursor just outside the selection is only
/// underlined.
fn drawn_style(editor: &Editor, live: &Look, at: usize) -> Style {
    let selection = editor.selection().unwrap_or(0..0);
    match (at == editor.cursor(), selection.contains(&at)) {
        (true, false) if !selection.is_empty() => CURSOR_BESIDE,
        (true, _) => CURSOR,
        (false, true) => SELECTED,
        (false, false) => live.at(at).1,
    }
}

/// Draws one row: its graphemes, the selection, and the cursor (after the
/// last grapheme when it sits at the line's end), each grapheme as `live`
/// says. Returns the column the cursor was drawn at, if on this row.
fn draw_row(grid: &mut Grid, area: Rect, editor: &Editor, row: &Row, live: &Look) -> Option<u16> {
    // A boxed table's line: its first screen row, drawn whole from its
    // cells, the cursor in it if there (step TB1).
    if let Some(boxed) = live.boxed_at(row.range.start).filter(|_| !row.drawn) {
        return draw_boxed(grid, area, editor, live, boxed, 0);
    }
    // Live mode's prefix, then the text right of it.
    let prefix = live.prefix(row.range.start);
    let code = if row.indent > 0 {
        let mut x = area.x;
        let spaces = &SPACES[..usize::from(prefix.spaces).min(SPACES.len())];
        x += grid.put_str(x, area.y, spaces, Style::PLAIN, area.width);
        for (piece, style) in prefix.pieces {
            x += grid.put_str(x, area.y, piece, *style, area.right().saturating_sub(x));
        }
        prefix.code
    } else {
        None
    };
    let indent = u16::try_from(row.indent).unwrap_or(u16::MAX);
    let area = Rect::new(area.x + indent, area.y, area.width - indent, area.height);
    let mut cursor = None;
    let text = editor.text();
    let style_at = |at: usize| drawn_style(editor, live, at);
    let mut column = 0;
    for (i, grapheme) in text[row.range.clone()].grapheme_indices(true) {
        let at = row.range.start + i;
        let style = style_at(at);
        let x = area
            .x
            .saturating_add(u16::try_from(column).unwrap_or(u16::MAX));
        let room = area.width.saturating_sub(x - area.x);
        if at == editor.cursor() {
            cursor = Some(x);
        }
        let room = (x, area.y, room);
        column += put_grapheme(grid, room, text, at, grapheme, column, style, live);
    }
    // A code line's gray box, to one column past its block's widest line.
    if let Some(widest) = code {
        let end = usize::from(widest) + 1;
        if column < end && column < usize::from(area.width) {
            let x = area.x + u16::try_from(column).unwrap_or(u16::MAX);
            let fill = (end - column).min(usize::from(area.width) - column);
            let fill = u16::try_from(fill).unwrap_or(u16::MAX);
            grid.fill(Rect::new(x, area.y, fill, 1), CODE);
        }
    }
    // A folded heading: ` …` after it, as in the viewer.
    if row.ends_line && live.folded_end(row.range.end) {
        let x = area.x + u16::try_from(column).unwrap_or(u16::MAX);
        let room = area.right().saturating_sub(x);
        grid.put_str(x, area.y, " …", Style::PLAIN.mods(Mods::DIM), room);
    }
    // The line's end: the cursor there, or a selected line break.
    if row.ends_line {
        let style = style_at(row.range.end);
        let shows_break = row.next.is_some() && style == SELECTED;
        if style == CURSOR || style == CURSOR_BESIDE || shows_break {
            // The column kept free on the right holds it, unless a single
            // grapheme wider than the row filled the row: then the last
            // cell, so the cursor is never off screen.
            let column = u16::try_from(column).unwrap_or(u16::MAX);
            let x = area.x + column.min(area.width - 1);
            grid.put_str(x, area.y, " ", style, 1);
            if style != SELECTED {
                cursor = Some(x);
            }
        }
    }
    cursor
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::keys::{Key, MouseEvent};
    use crate::ui::clipboard::Clipboard;
    use crate::ui::grid::{is_invisible, is_pictographic};
    use proptest::prelude::*;

    pub(super) fn rows(grid: &Grid) -> Vec<String> {
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
    pub(super) fn cursor_cell(grid: &Grid) -> Option<(u16, u16)> {
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

    pub(super) fn drawn(ed: &mut Editor, view: &mut View, width: u16, height: u16) -> Grid {
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
        // Just after the selection, the cursor is only underlined: `c`
        // does not look selected. At its start, inside it, it is the
        // selected, underlined cell.
        assert_eq!(grid.cell(0, 2).unwrap().style(), CURSOR_BESIDE);
        let mut back = Editor::new("ab\ncd").unwrap();
        for key in [Key::End, Key::ShiftLeft, Key::ShiftLeft] {
            back.key(key, &mut clip);
        }
        let grid = drawn(&mut back, &mut View::default(), 10, 4);
        assert_eq!(grid.cell(0, 1).unwrap().style(), CURSOR);
        assert_eq!(grid.cell(1, 1).unwrap().style(), SELECTED);
        ed.key(Key::End, &mut clip);
        ed.key(Key::Char('!'), &mut clip);
        let grid = drawn(&mut ed, &mut View::default(), 10, 4);
        assert_eq!(rows(&grid)[0], "work/p [+]", "[+] kept, the name cut");
        assert_eq!(cursor_cell(&grid), Some((3, 2)), "after the last character");
        // Live mode says so in the title (step LE1).
        let grid = drawn(&mut ed, &mut View::live(), 20, 4);
        assert_eq!(rows(&grid)[0], "work/plan   live [+]");
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

    /// The style the cell (`x`, `y`) is drawn in.
    fn style_at(grid: &Grid, x: u16, y: u16) -> Style {
        grid.cell(x, y).unwrap().style()
    }

    /// Live mode (steps LE3, LR1): markup is hidden and styled except in
    /// the element the cursor is in (here `**bold**`, the cursor in its
    /// fourth column) and those a selection touches. No line numbers when
    /// live; as written, they are back.
    #[test]
    fn live_mode_styles_lines_away_from_the_cursor() {
        let mut clip = Clipboard::default();
        let mut ed = Editor::new("# Title\n**bold** and [[Note|alias]]\nlast").unwrap();
        for key in [Key::Down, Key::Down, Key::End] {
            ed.key(key, &mut clip);
        }
        let grid = drawn(&mut ed, &mut View::live(), 40, 6);
        let (icon, heading) = crate::md::render::heading_mark(pulldown_cmark::HeadingLevel::H1);
        // The viewer's blank row after the heading (step LV2).
        assert_eq!(
            rows(&grid)[1..5],
            [
                format!("  {icon}Title"),
                String::new(),
                "    bold and alias".into(),
                "    last".into()
            ]
        );
        assert_eq!(style_at(&grid, 4, 1), heading, "the heading's T");
        assert!(style_at(&grid, 4, 3).mods.contains(Mods::BOLD), "b");
        assert_eq!(
            style_at(&grid, 13, 3),
            crate::md::render::LINK,
            "a of alias"
        );
        // The cursor on the second line: it is drawn as written.
        ed.key(Key::Up, &mut clip);
        let grid = drawn(&mut ed, &mut View::live(), 40, 6);
        assert_eq!(rows(&grid)[3], "    **bold** and alias");
        assert_eq!(rows(&grid)[1], format!("  {icon}Title"));
        // A selection from there into the heading's text: the bold text
        // as written; the heading stays styled (its `# ` is not selected),
        // the link the selection does not reach too.
        ed.key(Key::ShiftUp, &mut clip);
        let grid = drawn(&mut ed, &mut View::live(), 40, 6);
        assert_eq!(
            rows(&grid)[1..5],
            [
                format!("  {icon}Title"),
                String::new(),
                "    **bold** and alias".into(),
                "    last".into()
            ]
        );
        // Not live: everything as written.
        let grid = drawn(&mut ed, &mut View::default(), 40, 5);
        assert_eq!(rows(&grid)[3], "3 last");
        assert_eq!(rows(&grid)[1], "1 # Title");
    }

    /// Live rows wrap by the width shown, not the width written.
    #[test]
    fn live_rows_wrap_by_what_is_shown() {
        let mut clip = Clipboard::default();
        let mut ed = Editor::new("**aaaa** bbbb\nx").unwrap();
        ed.key(Key::Down, &mut clip);
        // 10 columns of text and the cursor's (too narrow for a gutter).
        let grid = drawn(&mut ed, &mut View::live(), 11, 5);
        assert_eq!(rows(&grid)[1..3], ["aaaa bbbb", "x"]);
        // As written, the first line needs two rows.
        let grid = drawn(&mut ed, &mut View::default(), 11, 5);
        assert_eq!(rows(&grid)[1..4], ["**aaaa** b", "bbb", "x"]);
    }

    proptest! {
        /// Live mode, any Markdown-ish text, cursor and screen: no panic,
        /// the cursor drawn exactly once, and no cell holds a control
        /// character.
        #[test]
        fn live_mode_always_draws_the_cursor(
            // Fences, blank-line runs and underlines too (step LV1);
            // tables and their `|---|` lines (step LT2).
            text in "([*_`#\\[\\]()|>~!\\\\ a\n\t-]|\r\n|é|日|\u{202e}|\u{0}|```\n|\n\n|==\n|\\|a\\|b\\|\n\\|-\\|-\\|\n|\\|---\\|\n){0,60}",
            width in 3u16..30,
            height in 3u16..8,
            moves in prop::collection::vec(0u8..7, 0..12),
        ) {
            let mut clip = Clipboard::default();
            let mut ed = Editor::new(&text).unwrap();
            // As the session opens a live editor: keys move through
            // table cells (steps TB3, TB4).
            ed.set_live(true);
            for m in moves {
                let key = match m {
                    0 => Key::Up,
                    1 => Key::Down,
                    2 => Key::Left,
                    3 => Key::Right,
                    4 => Key::ShiftDown,
                    5 => Key::Tab,
                    _ => Key::BackTab,
                };
                ed.key(key, &mut clip);
            }
            // Every section folded (step LF1): one the cursor is in opens.
            let mut view = View::live();
            for (heading, _) in live::parse(ed.text()).layout.headings {
                view.toggle_fold(heading);
            }
            drawn(&mut ed, &mut view, width, height);
            ed.key(Key::Up, &mut clip);
            let grid = drawn(&mut ed, &mut view, width, height);
            // The cursor: underlined cells in the default color, on one
            // row (a link is underlined too, but cyan).
            let cursor: Vec<(u16, u16)> = (0..height)
                .flat_map(|y| (0..width).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let style = grid.cell(x, y).unwrap().style();
                    style.mods.contains(Mods::UNDERLINED) && style.fg == Color::Default
                })
                .collect();
            prop_assert!(!cursor.is_empty());
            prop_assert!(cursor.iter().all(|&(_, y)| y == cursor[0].1));
            for y in 0..height {
                for x in 0..width {
                    let symbol = grid.cell(x, y).unwrap().symbol();
                    prop_assert!(!symbol.chars().any(char::is_control));
                }
            }
        }
    }

    /// Live mode (steps LE5, LR1): a click on a styled line lands on the
    /// grapheme drawn under it, and a heading's icon on the heading's
    /// start; a click beside an element leaves it styled.
    #[test]
    fn live_clicks_land_on_what_is_drawn() {
        let mut clip = Clipboard::default();
        let mut ed = Editor::new("# Title\n**bold** word\nlast").unwrap();
        for key in [Key::Down, Key::Down, Key::End] {
            ed.key(key, &mut clip);
        }
        let mut view = View::live();
        let grid = drawn(&mut ed, &mut view, 40, 6);
        // Row 2 is the viewer's blank row after the heading (step LV2).
        assert_eq!(rows(&grid)[2..4], ["", "    bold word"]);
        let at = |view: &View, x, y| view.position_at(ed.text(), x, y, false);
        // `w` of word, after the gutter, the section's indent and `bold `.
        assert_eq!(at(&view, 9, 3), Some(17));
        // `b` of bold, past the hidden `**`.
        assert_eq!(at(&view, 4, 3), Some(10));
        // The blank row: the start of the line below.
        assert_eq!(at(&view, 6, 2), Some(8));
        // The gutter and both cells of the icon: its first letter, past
        // the hidden `# `.
        assert_eq!(at(&view, 0, 1), Some(2));
        assert_eq!(at(&view, 2, 1), Some(2));
        assert_eq!(at(&view, 3, 1), Some(2));
        assert_eq!(at(&view, 5, 1), Some(3));
        ed.mouse(MouseEvent::Press, Some(17), std::time::Instant::now());
        assert_eq!(ed.cursor(), 17);
        let grid = drawn(&mut ed, &mut view, 40, 6);
        assert_eq!(rows(&grid)[3], "    bold word");
        assert_eq!(cursor_cell(&grid), Some((9, 3)));
        // On `bold`: that element is drawn as written.
        ed.mouse(MouseEvent::Press, Some(10), std::time::Instant::now());
        let grid = drawn(&mut ed, &mut view, 40, 6);
        assert_eq!(rows(&grid)[3], "    **bold** word");
    }

    /// Reveal by word (step LR1): the cursor just after `**b**` shows it as
    /// written; a space typed there styles it again. A heading stays
    /// styled while the cursor is in its text; its `## ` shows only while
    /// the cursor is in it, and its text stays green then too. A list
    /// marker shows when the cursor is on it.
    #[test]
    fn live_reveals_only_the_element_under_the_cursor() {
        let mut clip = Clipboard::default();
        let mut ed = Editor::new("").unwrap();
        let mut view = View::live();
        for c in "a **b**".chars() {
            ed.key(Key::Char(c), &mut clip);
        }
        assert_eq!(rows(&drawn(&mut ed, &mut view, 30, 4))[1], "  a **b**");
        ed.key(Key::Char(' '), &mut clip);
        assert_eq!(rows(&drawn(&mut ed, &mut view, 30, 4))[1], "  a b");
        // A heading, a list item: the cursor at the item's text.
        let mut ed = Editor::new("## Head\n- item").unwrap();
        for key in [Key::Down, Key::End] {
            ed.key(key, &mut clip);
        }
        let (icon, green) = crate::md::render::heading_mark(pulldown_cmark::HeadingLevel::H2);
        let grid = drawn(&mut ed, &mut view, 30, 5);
        // As in the viewer: the gutter, the heading 2 columns in, its
        // section 4, a blank row between them (step LV2).
        assert_eq!(
            rows(&grid)[1..4],
            [
                format!("    {icon}Head"),
                String::new(),
                "      • item".into()
            ]
        );
        // In the heading's text: still styled.
        ed.key(Key::Up, &mut clip);
        let grid = drawn(&mut ed, &mut view, 30, 5);
        assert_eq!(rows(&grid)[1], format!("    {icon}Head"));
        assert_eq!(style_at(&grid, 6, 1), green, "H");
        // In its `## `: shown as written, the text still green.
        ed.key(Key::Home, &mut clip);
        let grid = drawn(&mut ed, &mut view, 30, 5);
        assert_eq!(rows(&grid)[1], "    ## Head");
        assert_eq!(style_at(&grid, 7, 1), green, "H");
        // The cursor at the marker: `-` shows.
        for key in [Key::Down, Key::Home] {
            ed.key(key, &mut clip);
        }
        assert_eq!(rows(&drawn(&mut ed, &mut view, 30, 5))[3], "      - item");
        // Typing a heading: green as it is typed, its `## ` hidden.
        let mut ed = Editor::new("").unwrap();
        for c in "## Sub".chars() {
            ed.key(Key::Char(c), &mut clip);
        }
        let grid = drawn(&mut ed, &mut view, 30, 5);
        assert_eq!(rows(&grid)[1], format!("    {icon}Sub"));
        assert_eq!(style_at(&grid, 6, 1), green, "S");
    }

    /// Crystal's clipboard in live mode (step LE5): copying takes the
    /// Markdown as written, pasting puts it back unchanged.
    #[test]
    fn live_copy_and_paste_keep_the_markdown() {
        let text = "# Title\n**bold** [[Note|alias]]\n> quote\n";
        let mut clip = Clipboard::default();
        let mut ed = Editor::new(text).unwrap();
        let mut view = View::live();
        drawn(&mut ed, &mut view, 40, 6);
        for key in [Key::Down, Key::ShiftEnd, Key::Ctrl('c')] {
            ed.key(key, &mut clip);
        }
        assert_eq!(clip.get(), Some("**bold** [[Note|alias]]"));
        for key in [Key::Ctrl('a'), Key::Ctrl('c')] {
            ed.key(key, &mut clip);
        }
        assert_eq!(clip.get(), Some(text));
        let mut pasted = Editor::new("").unwrap();
        drawn(&mut pasted, &mut View::live(), 40, 6);
        pasted.key(Key::Ctrl('v'), &mut clip);
        assert_eq!(pasted.text(), text);
    }

    /// Above `LIVE_MAX` live mode draws as written and the title drops
    /// `live` (step LE5); at the limit it is still live.
    #[test]
    fn live_mode_stops_above_the_size_limit() {
        let mut clip = Clipboard::default();
        let mut text = "**b**\n".to_owned();
        text.push_str(&"x".repeat(LIVE_MAX - text.len()));
        let mut ed = Editor::new(&text).unwrap();
        ed.key(Key::Down, &mut clip);
        let mut view = View::live();
        let grid = drawn(&mut ed, &mut view, 40, 4);
        let shown = rows(&grid);
        assert!(shown[0].ends_with(" live"));
        assert_eq!(shown[1], "  b", "no line numbers when live: a blank gutter");
        ed.key(Key::Char('x'), &mut clip);
        let grid = drawn(&mut ed, &mut view, 40, 4);
        let shown = rows(&grid);
        assert!(shown[0].ends_with(" [+]") && !shown[0].contains("live"));
        assert_eq!(shown[1], "1 **b**");
    }

    /// The viewer's layout in live mode (step LI2): a quote's bar on its
    /// wrapped rows too, a bullet, a code block as a gray box as wide as
    /// its widest line plus a column each side (its fences without a row), and
    /// a click on a prefix landing on the row's first character.
    #[test]
    fn live_lines_are_laid_out_as_in_the_viewer() {
        let mut clip = Clipboard::default();
        let text = "> quote that wraps here\n- item\n\n```\ncode\nlonger\n```\nend";
        let mut ed = Editor::new(text).unwrap();
        for _ in 0..7 {
            ed.key(Key::Down, &mut clip);
        }
        let mut view = View::live();
        // 15 columns of text and the cursor's.
        let grid = drawn(&mut ed, &mut view, 16, 12);
        assert_eq!(
            rows(&grid)[1..11],
            [
                "│ quote that wr",
                "│ aps here",
                // The viewer's blank rows between blocks (step LV2).
                "",
                "• item",
                "",
                " code",
                " longer",
                "",
                "end",
                ""
            ]
        );
        assert_eq!(style_at(&grid, 0, 2), crate::md::render::QUOTE_BAR);
        assert!(style_at(&grid, 0, 4).mods.contains(Mods::DIM), "• dimmed");
        // The box: columns 0 to 7 of the code's rows; its fences have no
        // row while the cursor is not in the block (step LV1).
        for y in 6..=7 {
            for x in 0..8 {
                let bg = style_at(&grid, x, y).bg;
                assert_eq!(bg, crate::md::render::CODE.bg, "({x}, {y})");
            }
            assert_eq!(style_at(&grid, 8, y).bg, Color::Default, "(8, {y})");
        }
        // A click on the bar: the quote's first letter.
        assert_eq!(view.position_at(ed.text(), 0, 1, false), Some(2));
        // The cursor in a line's markup shows it as written.
        ed.mouse(MouseEvent::Press, Some(24), std::time::Instant::now());
        let grid = drawn(&mut ed, &mut view, 16, 12);
        assert_eq!(rows(&grid)[4], "- item");
    }

    /// Where `word` starts on `grid`: its first cell, if drawn.
    fn find(grid: &Grid, word: &str) -> Option<(u16, u16)> {
        for y in 0..grid.height() {
            let mut row = String::new();
            let mut xs = Vec::new();
            for x in 0..grid.width() {
                let symbol = grid.cell(x, y).unwrap().symbol();
                xs.extend(std::iter::repeat_n(x, symbol.len()));
                row.push_str(symbol);
            }
            if let Some(i) = row.find(word) {
                return Some((xs[i], y));
            }
        }
        None
    }

    /// Same as the viewer (step LI3): every line of a note with headings,
    /// nested lists, a lazy line, quotes, a list in a quote, numbers and a
    /// code block starts its text in the column the viewer draws it in.
    /// Rows may differ (the editor keeps the note's own blank lines).
    #[test]
    fn live_lines_start_where_the_viewer_draws_them() {
        let text = "# Alpha\nbravo text\n## Charlie\ndelta para\n\n- echo item\n  - \
                    foxtrot nested\n    golf lazy\n\n> hotel quote\n> - india listed\n\n\
                    1. juliet first\n2. kilo second\n\n```\nlima code\n```\n\nmike end";
        let words = [
            "Alpha", "bravo", "Charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
            "juliet", "kilo", "lima", "mike",
        ];
        let page = crate::md::render::render(text, crate::md::render::Kind::Markdown, 60);
        let mut viewer = Grid::new(60, 40);
        page.draw(&mut viewer, Rect::new(0, 0, 60, 40), 0);
        // Live, the cursor at the end of the last line, plain text.
        let mut clip = Clipboard::default();
        let mut ed = Editor::new(text).unwrap();
        for _ in 0..30 {
            ed.key(Key::Down, &mut clip);
        }
        ed.key(Key::End, &mut clip);
        let live = drawn(&mut ed, &mut View::live(), 61, 40);
        for word in words {
            // The page drawn alone: the pager puts its gutter left of it.
            let seen = find(&viewer, word).map(|(x, _)| x + GUTTER);
            let shown = find(&live, word).map(|(x, _)| x);
            assert!(seen.is_some(), "{word} not in the viewer");
            assert_eq!(shown, seen, "{word}");
        }
    }

    /// Folds (step LF1): a folded section's lines are not drawn and its
    /// heading ends in ` …`; a section reaching the text's end folds too;
    /// a line typed above moves the fold; the cursor entering opens it.
    #[test]
    fn live_folds_hide_sections_until_the_cursor_enters() {
        let mut clip = Clipboard::default();
        let (h1, _) = crate::md::render::heading_mark(pulldown_cmark::HeadingLevel::H1);
        let mut ed = Editor::new("# A\nbody1\nbody2\n# B\nafter").unwrap();
        // The cursor in `A`'s text, so the heading stays styled.
        for key in [Key::Right, Key::Right] {
            ed.key(key, &mut clip);
        }
        let mut view = View::live();
        view.toggle_fold(0);
        let grid = drawn(&mut ed, &mut view, 30, 9);
        // As in the viewer: no blank row between a folded heading and the
        // next; one after `B` before its text (step LV2).
        assert_eq!(
            rows(&grid)[1..6],
            [
                format!("  {h1}A …"),
                format!("  {h1}B"),
                String::new(),
                "    after".into(),
                String::new()
            ]
        );
        // A line typed above the heading: the fold moves with it.
        for key in [
            Key::Home,
            Key::Char('z'),
            Key::Enter,
            Key::Right,
            Key::Right,
        ] {
            ed.key(key, &mut clip);
        }
        let grid = drawn(&mut ed, &mut view, 30, 9);
        assert_eq!(
            rows(&grid)[1..4],
            ["  z".to_owned(), String::new(), format!("  {h1}A …")]
        );
        // Down into the section: it opens.
        ed.key(Key::Down, &mut clip);
        let grid = drawn(&mut ed, &mut view, 30, 9);
        assert_eq!(
            rows(&grid)[3..7],
            [
                format!("  {h1}A"),
                String::new(),
                "    body1".into(),
                "    body2".into()
            ]
        );
        // The last section, reaching the text's end.
        let mut ed = Editor::new("# A\nx\n# B\ny\nz").unwrap();
        for key in [Key::Right, Key::Right] {
            ed.key(key, &mut clip);
        }
        let mut view = View::live();
        view.toggle_fold(6);
        let grid = drawn(&mut ed, &mut view, 30, 9);
        assert_eq!(
            rows(&grid)[1..7],
            [
                format!("  {h1}A"),
                String::new(),
                "    x".into(),
                String::new(),
                format!("  {h1}B …"),
                String::new()
            ]
        );
    }

    /// Heading markers (step LF2): over a section heading's row the
    /// viewer's marker shows in the gutter, `v` open and `>` folded; a
    /// press on the gutter folds or unfolds; a press on the text is the
    /// editor's; the plain editor takes no mouse here.
    #[test]
    fn live_heading_markers_fold_by_click() {
        let mut clip = Clipboard::default();
        let (h1, _) = crate::md::render::heading_mark(pulldown_cmark::HeadingLevel::H1);
        let mut ed = Editor::new("# A\nx\n# B\ny").unwrap();
        for key in [Key::Right, Key::Right] {
            ed.key(key, &mut clip);
        }
        let mut view = View::live();
        drawn(&mut ed, &mut view, 30, 9);
        assert_eq!(view.mouse(ed.text(), MouseEvent::Move, 9, 1), Click::Taken);
        let grid = drawn(&mut ed, &mut view, 30, 9);
        assert_eq!(rows(&grid)[1], format!("v {h1}A"));
        assert_eq!(style_at(&grid, 0, 1), crate::pager::MARKER);
        // Off the heading: no marker.
        assert_eq!(view.mouse(ed.text(), MouseEvent::Move, 9, 2), Click::Taken);
        assert_eq!(
            rows(&drawn(&mut ed, &mut view, 30, 9))[1],
            format!("  {h1}A")
        );
        // A press on the gutter of `# B`'s row (row 5: blank rows as in
        // the viewer, step LV2) folds it.
        assert_eq!(view.mouse(ed.text(), MouseEvent::Move, 0, 5), Click::Taken);
        assert_eq!(view.mouse(ed.text(), MouseEvent::Press, 0, 5), Click::Taken);
        let grid = drawn(&mut ed, &mut view, 30, 9);
        assert_eq!(rows(&grid)[5..7], [format!("> {h1}B …"), String::new()]);
        // Again: unfolded.
        assert_eq!(view.mouse(ed.text(), MouseEvent::Press, 1, 5), Click::Taken);
        let grid = drawn(&mut ed, &mut view, 30, 9);
        assert_eq!(
            rows(&grid)[5..8],
            [format!("v {h1}B"), String::new(), "    y".into()]
        );
        // A press on the heading's text, or on a row without a heading:
        // the editor's.
        assert_eq!(
            view.mouse(ed.text(), MouseEvent::Press, 5, 5),
            Click::Editor
        );
        assert_eq!(
            view.mouse(ed.text(), MouseEvent::Press, 0, 2),
            Click::Editor
        );
        // Not live: nothing here.
        let mut plain = View::default();
        drawn(&mut ed, &mut plain, 30, 6);
        assert_eq!(
            plain.mouse(ed.text(), MouseEvent::Move, 0, 1),
            Click::Editor
        );
        assert_eq!(
            plain.mouse(ed.text(), MouseEvent::Press, 0, 1),
            Click::Editor
        );
    }

    /// Links in live mode (step LF3): a press on a link's text is for the
    /// caller to follow, an internal or a web one; on a link drawn as
    /// written (the cursor in it) it is the editor's; a web link's cells
    /// carry its address for the terminal.
    #[test]
    fn live_link_clicks_are_for_following() {
        let mut clip = Clipboard::default();
        let text = "a [[Note#Head|alias]] b [site](https://example.org)\nlast";
        let mut ed = Editor::new(text).unwrap();
        ed.key(Key::Down, &mut clip);
        let mut view = View::live();
        let grid = drawn(&mut ed, &mut view, 40, 4);
        assert_eq!(rows(&grid)[1], "  a alias b site");
        // `alias` at columns 4 to 8, `site` at 12 to 15.
        let wiki = view.mouse(ed.text(), MouseEvent::Press, 5, 1);
        let Click::Link { target, web: false } = wiki else {
            panic!("{wiki:?}");
        };
        assert_eq!(&text[target], "Note#Head");
        let site = view.mouse(ed.text(), MouseEvent::Press, 13, 1);
        assert!(matches!(site, Click::Link { web: true, .. }), "{site:?}");
        // Its release is the view's too, so the editor keeps any notice.
        assert_eq!(
            view.mouse(ed.text(), MouseEvent::Release, 13, 1),
            Click::Taken
        );
        assert_eq!(
            view.mouse(ed.text(), MouseEvent::Release, 13, 1),
            Click::Editor
        );
        // The web link's cells carry its address; the wiki link's none.
        let link = grid.cell(13, 1).unwrap().link();
        assert_eq!(grid.link_target(link), Some("https://example.org"));
        assert_eq!(grid.cell(5, 1).unwrap().link(), 0);
        // Beside a link, and on a link the cursor is in: the editor's.
        assert_eq!(
            view.mouse(ed.text(), MouseEvent::Press, 10, 1),
            Click::Editor
        );
        ed.jump_to(5);
        drawn(&mut ed, &mut view, 40, 4);
        assert_eq!(
            view.mouse(ed.text(), MouseEvent::Press, 5, 1),
            Click::Editor
        );
    }

    /// Rows as in the viewer (step LV1): blank lines before the first
    /// block, after another blank line or after the last block, a Setext
    /// underline and a code block's fences get no row while not edited;
    /// the fences come back while the cursor is in the block, a blank
    /// line while the cursor is on it.
    #[test]
    fn live_skips_lines_the_viewer_has_no_row_for() {
        let mut clip = Clipboard::default();
        let (h1, _) = crate::md::render::heading_mark(pulldown_cmark::HeadingLevel::H1);
        let text = "\n\nA\n\n\n\nB\n\nTitle\n=====\n\n```\ncode\n```\nend\n\n";
        let mut ed = Editor::new(text).unwrap();
        ed.jump_to(text.find("end").unwrap());
        let mut view = View::live();
        let grid = drawn(&mut ed, &mut view, 30, 12);
        assert_eq!(
            rows(&grid)[1..10],
            [
                "  A".to_owned(),
                String::new(),
                "  B".into(),
                String::new(),
                format!("  {h1}Title"),
                String::new(),
                "     code".into(),
                // The viewer's blank row after a code block (step LV2).
                String::new(),
                "    end".into()
            ]
        );
        // Up into the code block: its fences show.
        ed.key(Key::Up, &mut clip);
        let grid = drawn(&mut ed, &mut view, 30, 12);
        let fences = rows(&grid).iter().filter(|r| r.trim() == "```").count();
        assert_eq!(fences, 2);
        // On the third blank line between A and B: it shows.
        ed.jump_to(text.find("A").unwrap() + 3);
        let grid = drawn(&mut ed, &mut view, 30, 12);
        assert_eq!(rows(&grid)[1..5], ["  A", "", "", "  B"]);
    }

    /// The viewer's blank rows while scrolling (step LV2): scrolled to the
    /// bottom, then up line by line, the rows shown are the viewer's, and
    /// the cursor is drawn on its line, never on a blank row.
    #[test]
    fn live_gap_rows_scroll_like_rows() {
        let mut clip = Clipboard::default();
        let (h1, _) = crate::md::render::heading_mark(pulldown_cmark::HeadingLevel::H1);
        let mut ed = Editor::new("a\n# H\nb\nc\nd").unwrap();
        for _ in 0..4 {
            ed.key(Key::Down, &mut clip);
        }
        let mut view = View::live();
        // Three rows of text.
        let shown = |ed: &mut Editor, view: &mut View| {
            let grid = drawn(ed, view, 30, 5);
            (
                rows(&grid)[1..4].to_vec(),
                cursor_cell(&grid).map(|(_, y)| y),
            )
        };
        assert_eq!(
            shown(&mut ed, &mut view),
            (
                vec!["    b".into(), "    c".into(), "    d".into()],
                Some(3)
            )
        );
        for _ in 0..3 {
            ed.key(Key::Up, &mut clip);
        }
        // Column 1 of `# H` is in its `# `: drawn as written.
        assert_eq!(
            shown(&mut ed, &mut view),
            (vec!["  # H".into(), String::new(), "    b".into()], Some(1))
        );
        ed.key(Key::Up, &mut clip);
        assert_eq!(
            shown(&mut ed, &mut view),
            (
                vec!["  a".into(), String::new(), format!("  {h1}H")],
                Some(1)
            )
        );
    }

    /// A table the cursor is not in gets the viewer's border rows (step
    /// LT2) and its lines boxed (step LT3): above it, between two body rows, below it (before a blank
    /// line, at the text's end); a click on one goes to the line below;
    /// the cursor entering the table, or a screen too narrow for the box,
    /// leaves it as written.
    #[test]
    fn live_tables_get_border_rows() {
        let mut clip = Clipboard::default();
        let body = |grid: &Grid, rows_shown: usize| rows(grid)[1..=rows_shown].to_vec();
        let text = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n\nend";
        let mut ed = Editor::new(text).unwrap();
        for _ in 0..5 {
            ed.key(Key::Down, &mut clip);
        }
        let mut view = View::live();
        let grid = drawn(&mut ed, &mut view, 30, 12);
        assert_eq!(
            body(&grid, 9),
            [
                "  ┌───┬───┐",
                "  │ a │ b │",
                "  ├───┼───┤",
                "  │ 1 │ 2 │",
                "  ├───┼───┤",
                "  │ 3 │ 4 │",
                "  └───┴───┘",
                "",
                "  end"
            ]
        );
        assert_eq!(view.position_at(text, 4, 1, false), Some(0));
        assert_eq!(view.position_at(text, 4, 7, false), Some(40));
        // The cursor in the table: as written.
        ed.key(Key::Up, &mut clip);
        ed.key(Key::Up, &mut clip);
        let grid = drawn(&mut ed, &mut view, 30, 12);
        assert_eq!(
            body(&grid, 3),
            ["  | a | b |", "  |---|---|", "  | 1 | 2 |"]
        );
        // Too narrow for the box: as written.
        let mut ed = Editor::new(text).unwrap();
        for _ in 0..5 {
            ed.key(Key::Down, &mut clip);
        }
        // The box takes 9 columns: as in the viewer, it fits 9 (the
        // cursor's column too); in 8, a column would keep fewer than 3.
        let grid = drawn(&mut ed, &mut View::live(), 9, 12);
        assert_eq!(body(&grid, 1), ["┌───┬───┐"]);
        let grid = drawn(&mut ed, &mut View::live(), 8, 12);
        assert_eq!(body(&grid, 1), ["| a | b"]);

        // After a heading (the viewer's blank row first), at the text's
        // end with or without a line break.
        let h1 = crate::md::render::heading_mark(pulldown_cmark::HeadingLevel::H1).0;
        for text in ["# H\n| a |\n|---|\n| 1 |", "# H\n| a |\n|---|\n| 1 |\n"] {
            let mut ed = Editor::new(text).unwrap();
            ed.key(Key::End, &mut clip);
            let grid = drawn(&mut ed, &mut View::live(), 30, 12);
            assert_eq!(
                body(&grid, 8),
                [
                    format!("  {h1}H").as_str(),
                    "",
                    "    ┌───┐",
                    "    │ a │",
                    "    ├───┤",
                    "    │ 1 │",
                    "    └───┘",
                    ""
                ],
                "{text:?}"
            );
        }
    }

    /// Boxed lines (step LT3): cells padded by their column's alignment
    /// between `│`s, styled, wide characters counted by columns, a short
    /// row's missing cell drawn empty, in a quote; a table without outer
    /// pipes; a click on padding lands at the cell's text's end.
    #[test]
    fn live_table_lines_are_boxed() {
        let mut clip = Clipboard::default();
        let text = "> | Name | Qty | Note |\n> |:--|--:|:-:|\n> | **apple** | 3 |\n\
                    > | 日本 | 120 | x |\n\nend";
        let mut ed = Editor::new(text).unwrap();
        for _ in 0..5 {
            ed.key(Key::Down, &mut clip);
        }
        let mut view = View::live();
        let grid = drawn(&mut ed, &mut view, 40, 12);
        assert_eq!(
            rows(&grid)[1..=9],
            [
                "  │ ┌───────┬─────┬──────┐",
                "  │ │ Name  │ Qty │ Note │",
                "  │ ├───────┼─────┼──────┤",
                "  │ │ apple │   3 │      │",
                "  │ ├───────┼─────┼──────┤",
                "  │ │ 日本  │ 120 │  x   │",
                "  │ └───────┴─────┴──────┘",
                "",
                "  end"
            ]
        );
        let (x, y) = find(&grid, "apple").unwrap();
        assert!(style_at(&grid, x, y).mods.contains(Mods::BOLD));
        // Right after `Name`: its padding, so its text's end. A click on
        // a cell's text lands there, in any column (step LW2); on the
        // `├─┼─┤` line, at that line's start.
        assert_eq!(view.position_at(text, 10, 2, false), Some(8));
        for word in ["120", "x", "日本"] {
            let (x, y) = find(&grid, word).unwrap();
            assert_eq!(
                view.position_at(text, x, y, false),
                text.find(word),
                "{word}"
            );
        }
        assert_eq!(view.position_at(text, 12, 3, false), text.find("> |:"));

        // Padding wider than `SPACES` (64 columns): the ` │ ` after it
        // stays at its column.
        let wide = format!("| a | b |\n|---|---|\n| {} | c |\n\nend", "x".repeat(70));
        let mut ed = Editor::new(&wide).unwrap();
        for _ in 0..4 {
            ed.key(Key::Down, &mut clip);
        }
        let grid = drawn(&mut ed, &mut View::live(), 90, 12);
        assert_eq!(rows(&grid)[2], format!("  │ a{} │ b │", " ".repeat(69)));

        let mut ed = Editor::new("a | b\n--|--\n1 | 2\n\nend").unwrap();
        for _ in 0..4 {
            ed.key(Key::Down, &mut clip);
        }
        let grid = drawn(&mut ed, &mut View::live(), 40, 12);
        assert_eq!(
            rows(&grid)[1..=5],
            [
                "  ┌───┬───┐",
                "  │ a │ b │",
                "  ├───┼───┤",
                "  │ 1 │ 2 │",
                "  └───┴───┘"
            ]
        );
    }

    /// Ctrl+E's anchor: the screen row the cursor's line starts on, as
    /// last drawn; negative when a long line starts above the screen.
    #[test]
    fn line_row_is_where_the_line_starts() {
        let mut clip = Clipboard::default();
        let text = "a\nb\nc\nd\ne\nf\ng";
        let mut ed = Editor::new(text).unwrap();
        for _ in 0..6 {
            ed.key(Key::Down, &mut clip);
        }
        let mut view = View::default();
        assert_eq!(view.line_row(text, 0), None, "before a frame");
        drawn(&mut ed, &mut view, 30, 5);
        assert_eq!(view.line_row(text, ed.cursor()), Some(2));
        assert_eq!(view.line_row(text, 8), Some(0));
        assert_eq!(view.line_row(text, 0), None, "off screen");
        // 100 columns in rows of 19: the cursor's row is the sixth, the
        // last of three shown.
        let long = "x".repeat(100);
        let mut ed = Editor::new(&long).unwrap();
        ed.jump_to(100);
        let mut view = View::default();
        drawn(&mut ed, &mut view, 22, 5);
        assert_eq!(view.line_row(&long, 100), Some(-3));
    }

    /// A table wider than the screen (step LW3): rows as the viewer draws
    /// them, its columns narrowed and its cells wrapped inside them; the
    /// view scrolls over a row's extra rows like any other; a click on
    /// one lands on the word drawn there.
    #[test]
    fn live_wide_tables_wrap_cells_as_the_viewer() {
        let text = "| Situation | What to do |\n|---|:-:|\n| A check fails | Fix it on \
                    **main**, commit, and redo section three. Nothing is tagged yet. |\n\
                    | Hashes differ | Nothing was signed. |\n\nend";
        // The viewer's page 40 wide; the editor's text area too, right of
        // the gutter.
        let page = crate::md::render::render(text, crate::md::render::Kind::Markdown, 40);
        let mut viewer = Grid::new(40, 20);
        page.draw(&mut viewer, Rect::new(0, 0, 40, 20), 0);
        let mut ed = Editor::new(text).unwrap();
        ed.jump_to(text.len());
        let mut view = View::live();
        let live = drawn(&mut ed, &mut view, 42, 22);
        let seen: Vec<String> = rows(&viewer).into_iter().take(13).collect();
        let shown: Vec<String> = rows(&live)[1..14]
            .iter()
            .map(|row| row.get(2..).unwrap_or("").to_owned())
            .collect();
        assert_eq!(shown, seen);
        assert!(seen.iter().any(|row| row.contains("tagged")), "{seen:#?}");
        // A click on a word in a cell's third line.
        let (x, y) = find(&live, "tagged").unwrap();
        assert_eq!(view.position_at(text, x, y, false), text.find("tagged"));
        // Fewer rows: the last ones, whatever row the top is.
        for height in 4..14usize {
            let grid = drawn(&mut ed, &mut View::live(), 42, height as u16 + 2);
            let tail = &rows(&live)[14 - height..14];
            assert_eq!(rows(&grid)[1..=height], *tail, "{height}");
        }
    }

    /// Typing inside a boxed table (step TB1): the table stays boxed while
    /// the cursor is in a cell's text, drawn on the cell's character or
    /// right after its text, in an empty cell at its column's start; a
    /// selection inside the table is reversed in its cells. On a hidden
    /// pipe, or with a selection reaching out of the table, it is drawn as
    /// written.
    #[test]
    fn live_tables_stay_boxed_while_edited() {
        let text = "| a | bb |  |\n|---|---|---|\n| c | d | e |\n\nend";
        let empty = live::parse(text).layout.tables[0].rows[0].1[2].start;
        let bb = text.find("bb").unwrap();
        let mut clip = Clipboard::default();
        let mut ed = Editor::new(text).unwrap();
        let mut view = View::live();
        ed.jump_to(text.len());
        let boxed = rows(&drawn(&mut ed, &mut view, 30, 10));
        assert!(boxed[1].contains('┌'), "{boxed:#?}");
        let (x, y) = find(&drawn(&mut ed, &mut view, 30, 10), "bb").unwrap();
        // On a character, after the text, in the empty cell.
        for (at, cell) in [
            (bb + 1, (x + 1, y)),
            (bb + 2, (x + 2, y)),
            (empty, (x + 5, y)),
        ] {
            ed.jump_to(at);
            let grid = drawn(&mut ed, &mut view, 30, 10);
            assert_eq!(rows(&grid), boxed, "{at}");
            assert_eq!(cursor_cell(&grid), Some(cell), "{at}");
        }
        // A click in the empty cell lands in it.
        assert_eq!(view.position_at(text, x + 5, y, false), Some(empty));
        // A selection inside a cell: reversed there, still boxed.
        ed.jump_to(bb);
        ed.key(Key::ShiftRight, &mut clip);
        let grid = drawn(&mut ed, &mut view, 30, 10);
        assert_eq!(rows(&grid), boxed);
        assert!(style_at(&grid, x, y).mods.contains(Mods::REVERSED));
        // Reaching out of the table, or the cursor on a hidden pipe: as
        // written.
        ed.key(Key::ShiftDown, &mut clip);
        ed.key(Key::ShiftDown, &mut clip);
        ed.key(Key::ShiftDown, &mut clip);
        let grid = drawn(&mut ed, &mut view, 30, 10);
        assert!(!rows(&grid).concat().contains('┌'), "{:#?}", rows(&grid));
        ed.jump_to(0);
        let grid = drawn(&mut ed, &mut view, 30, 10);
        assert!(rows(&grid)[1].contains("| a | bb |"), "{:#?}", rows(&grid));
        // A cell drawing wider than measured (a tab, a spelled-out
        // character; found by the property test): as written, cursor or
        // not.
        for cell in ["a\tb", "\u{202e}x"] {
            let text = format!("| {cell} | b |\n|---|---|\n| c | d |\n\nend");
            let mut ed = Editor::new(&text).unwrap();
            ed.jump_to(text.len());
            let grid = drawn(&mut ed, &mut View::live(), 30, 10);
            assert!(!rows(&grid).concat().contains('┌'), "{:#?}", rows(&grid));
        }
    }

    /// A short row's missing cell is no place in the row (steps TB1, TB5;
    /// found by the property test): the parser puts it past the row's
    /// line break, and the layout leaves it out, so the cursor there is on
    /// the next line, drawn once, there.
    #[test]
    fn live_missing_cells_hold_no_cursor() {
        let text = "|a|b|\n|-|-|\né(é\r\n\nend";
        let next = text.find("\r\n").unwrap() + 2;
        assert_eq!(live::parse(text).layout.tables[0].rows[1].1.len(), 1);
        let mut ed = Editor::new(text).unwrap();
        ed.jump_to(next);
        let grid = drawn(&mut ed, &mut View::live(), 11, 7);
        let (_, y) = cursor_cell(&grid).expect("drawn once");
        assert_eq!(rows(&grid)[usize::from(y)], "", "{:#?}", rows(&grid));
    }

    /// A wrapped cell holds the cursor on each of its rows (step TB1): it is
    /// drawn where the character is, the table as when not edited, and
    /// scrolling keeps that row on screen at any height.
    #[test]
    fn live_wrapped_cells_hold_the_cursor() {
        let text = "| Situation | What to do |\n|---|:-:|\n| A check fails | Fix it on \
                    **main**, commit, and redo section three. Nothing is tagged yet. |\n\
                    | Hashes differ | Nothing was signed. |\n\nend";
        let tagged = text.find("tagged").unwrap();
        let mut ed = Editor::new(text).unwrap();
        ed.jump_to(text.len());
        let reference = rows(&drawn(&mut ed, &mut View::live(), 42, 22));
        ed.jump_to(tagged);
        let mut view = View::live();
        let grid = drawn(&mut ed, &mut view, 42, 22);
        assert_eq!(rows(&grid), reference);
        assert_eq!(cursor_cell(&grid), find(&grid, "tagged"));
        // Every height: the cursor's row shown, `tagged` on it.
        for height in 3..14u16 {
            let grid = drawn(&mut ed, &mut View::live(), 42, height + 2);
            let (x, y) = cursor_cell(&grid).unwrap_or_else(|| panic!("{height}"));
            assert_eq!(find(&grid, "tagged"), Some((x, y)), "{height}");
        }
        // Moving down a row keeps it shown as well.
        let mut clip = Clipboard::default();
        let mut view = View::live();
        drawn(&mut ed, &mut view, 42, 6);
        ed.jump_to(text.find("signed").unwrap());
        let grid = drawn(&mut ed, &mut view, 42, 6);
        assert_eq!(cursor_cell(&grid), find(&grid, "signed"));
        ed.key(Key::Left, &mut clip);
        let grid = drawn(&mut ed, &mut view, 42, 6);
        assert!(cursor_cell(&grid).is_some());
    }

    /// Markup revealed in a cell (step TB1) counts at its written width:
    /// its column widens and every row of the box stays aligned.
    #[test]
    fn live_revealed_markup_widens_its_column() {
        let text = "| **b** | c |\n|---|---|\n| d | e |\n\nend";
        let mut ed = Editor::new(text).unwrap();
        ed.jump_to(text.find('b').unwrap());
        let grid = drawn(&mut ed, &mut View::live(), 30, 10);
        let shown = rows(&grid);
        let table: Vec<&String> = shown
            .iter()
            .filter(|row| row.contains(['│', '┌', '├', '└']))
            .collect();
        assert!(
            table.iter().any(|row| row.contains("│ **b** │ c │")),
            "{shown:#?}"
        );
        let widths: Vec<usize> = table.iter().map(|row| row.chars().count()).collect();
        assert!(widths.windows(2).all(|w| w[0] == w[1]), "{shown:#?}");
    }

    /// Border rows scroll like rows (step LT2): the cursor's row last on
    /// screen counts them; the cursor entering the table drops them.
    #[test]
    fn live_border_rows_scroll_like_rows() {
        let mut clip = Clipboard::default();
        let mut ed = Editor::new("| a |\n|---|\n| 1 |\n\nend").unwrap();
        for _ in 0..4 {
            ed.key(Key::Down, &mut clip);
        }
        let mut view = View::live();
        let shown = |ed: &mut Editor, view: &mut View| {
            let grid = drawn(ed, view, 30, 5);
            (
                rows(&grid)[1..4].to_vec(),
                cursor_cell(&grid).map(|(_, y)| y),
            )
        };
        assert_eq!(
            shown(&mut ed, &mut view),
            (
                vec!["  └───┘".into(), String::new(), "  end".into()],
                Some(3)
            )
        );
        ed.key(Key::Up, &mut clip);
        assert_eq!(
            shown(&mut ed, &mut view),
            (
                vec!["  └───┘".into(), String::new(), "  end".into()],
                Some(2)
            )
        );
        ed.key(Key::Up, &mut clip);
        assert_eq!(
            shown(&mut ed, &mut view),
            (
                vec!["  | 1 |".into(), String::new(), "  end".into()],
                Some(1)
            )
        );
    }

    /// Same rows as the viewer (step LV3): a note shaped like a real one
    /// (a heading, a list, a paragraph, a fenced code block, inline code,
    /// subheadings, a link, a paragraph right before a list, a table,
    /// step LT4): every word
    /// is drawn in the viewer's row and column, the cursor away from them.
    #[test]
    fn live_rows_are_the_viewers() {
        let text = "# Heading\n\n- Item one\n- Item two\n\nThis is a **normal** paragraph \
                    with *italic* alpha:\n\n```rust\npub enum bravo {\n    charlie,\n}\n```\n\n\
                    Look delta `another` echo.\n\n## Subheading\n\n### Subsub\n\nThis is \
                    foxtrot [Monero](https://getmonero.org)\nI am golf testing:\n- \
                    [[hotel#^3y5wrv]]\n\n| Name | Qty |\n|:--|--:|\n| **india** | 3 |\n\
                    | juliet | 120 |\n\nmike end";
        let words = [
            "Heading",
            "one",
            "two",
            "normal",
            "alpha",
            "bravo",
            "charlie",
            "delta",
            "another",
            "Subheading",
            "Subsub",
            "foxtrot",
            "Monero",
            "golf",
            "hotel",
            // A boxed table (steps LT1 to LT3): its borders, cells padded
            // and aligned.
            "┌─",
            "Qty",
            "├─",
            "india",
            "  3 │",
            "juliet",
            "120",
            "└─",
            "mike",
        ];
        let page = crate::md::render::render(text, crate::md::render::Kind::Markdown, 70);
        let mut viewer = Grid::new(70, 50);
        page.draw(&mut viewer, Rect::new(0, 0, 70, 50), 0);
        let mut ed = Editor::new(text).unwrap();
        ed.jump_to(text.len());
        let live = drawn(&mut ed, &mut View::live(), 73, 52);
        for word in words {
            // The page alone: the pager puts its gutter left of it and
            // its title above.
            let seen = find(&viewer, word).map(|(x, y)| (x + GUTTER, y + 1));
            assert!(seen.is_some(), "{word} not in the viewer");
            assert_eq!(find(&live, word), seen, "{word}");
        }
    }

    /// Typing in a code block (live mode): its code stays yellow on gray,
    /// its fences show in the box, in the code style; the box widens as
    /// a line grows. Inline code being edited keeps the code style too.
    #[test]
    fn live_code_stays_styled_while_typed() {
        let mut clip = Clipboard::default();
        let mut ed = Editor::new("```\nab\n```\n\n`x` y").unwrap();
        // The cursor at the closing fence: in the block, off the code.
        ed.jump_to(7);
        let mut view = View::live();
        let grid = drawn(&mut ed, &mut view, 30, 8);
        // Rows: the fence, the code, the fence; the gutter's 2 columns
        // and the box's gray column first.
        assert_eq!(rows(&grid)[1..4], ["   ```", "   ab", "   ```"]);
        assert_eq!(style_at(&grid, 3, 1), CODE, "the opening fence");
        assert_eq!(style_at(&grid, 4, 2), CODE, "b");
        // The box: the gray column, `ab`, and one more.
        for x in [2, 5] {
            assert_eq!(style_at(&grid, x, 2).bg, CODE.bg, "({x}, 2)");
        }
        assert_eq!(style_at(&grid, 6, 2).bg, Color::Default);
        // A longer line: the box grows with it, the fences' rows too.
        for key in [Key::Up, Key::End, Key::Char('c'), Key::Char('d')] {
            ed.key(key, &mut clip);
        }
        let grid = drawn(&mut ed, &mut view, 30, 8);
        assert_eq!(rows(&grid)[2], "   abcd");
        assert_eq!(style_at(&grid, 6, 2), CODE, "d");
        assert_eq!(style_at(&grid, 7, 1).bg, CODE.bg, "the fence's row");
        assert_eq!(style_at(&grid, 8, 1).bg, Color::Default);
        // Inline code being edited: its backticks show, in the code style.
        ed.jump_to(ed.text().find("`x`").unwrap() + 1);
        let grid = drawn(&mut ed, &mut view, 30, 8);
        let row = rows(&grid).iter().position(|r| r.contains("`x`")).unwrap();
        let y = u16::try_from(row).unwrap();
        assert_eq!(style_at(&grid, 2, y), CODE, "the backtick");
    }
}
