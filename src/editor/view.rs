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
//! The list of link suggestions (`suggest`) is drawn over the frame
//! after it (`View::draw_suggestions`): a gray box of at most
//! `suggest::ROWS` names under the cursor's row, or above it when there
//! is more room there, starting at the cursor's column and moved left to
//! fit, at most half the screen wide. Folders are dimmed, the highlight is
//! reversed, a name too long ends in `…`. Names go through
//! `Grid::put_str` like the text, so the grid checks every character.
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
use super::buffer::Change;
use super::live::layout::{Layout, Piece};
use super::live::{self, Link, Parsed, Run, Show};
use super::suggest::{ROWS, Source};
use crate::md::render::{CODE, DASHES, QUOTE_BAR};
use crate::model::lines;
use crate::notes::Notes;
use crate::pager::{GUTTER, MARKER};
use crate::shell::keys::MouseEvent;
use crate::ui::grid::{
    Color, Grid, MAX_CELL_BYTES, Mods, Style, display_width, grapheme_width, hides_something,
    is_forbidden,
};
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
const TAB_STOP: usize = 4;
/// Live mode draws a note only up to this size: the whole note is parsed
/// on every frame, about 62 ms for 1 MiB in a release build (before the
/// line layout, 370 ms for 8 MiB). A larger note is drawn as written.
pub const LIVE_MAX: usize = 1 << 20;
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
            // Longer than a grid cell holds, it would be drawn `?`, one
            // column, while laid out as `width` (found by the long tests).
            width
                if width > 0
                    && grapheme.len() <= MAX_CELL_BYTES
                    && !grapheme.contains(is_forbidden)
                    && !hides_something(grapheme) =>
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

/// How live mode draws the text (steps LE1 to LE7, LR1, LI2): the runs
/// and the line layout of the last frame (`live::parse`), the bytes drawn
/// as written (sorted, merged) and the lines' leading markup hidden behind
/// their prefix (sorted). Not live: all empty, so every grapheme is drawn
/// as written, with no prefix.
#[derive(Default)]
struct Live {
    runs: Vec<Run>,
    raw: Vec<Range<usize>>,
    hidden: Vec<Range<usize>>,
    layout: Layout,
    /// The folded sections (step LF1), in order.
    folds: Vec<Fold>,
    /// The links a click follows (step LF3), in order.
    links: Vec<Link>,
    /// Whole lines not drawn while not edited, as the viewer gives them
    /// no row (step LV1): fences, Setext underlines, a blank line after
    /// another, before the first block or after the last. Sorted, each
    /// from a line's start to the next line's.
    skips: Vec<Range<usize>>,
    /// The empty line after a final line break is not drawn either.
    skip_end: bool,
    /// The lines the viewer's added blank row goes before (step LV2),
    /// sorted: each a line drawn, past any skipped ones.
    gaps: Vec<usize>,
    /// The boxed tables' border rows (step LT2), sorted by the line they
    /// go before: a bottom border, past any skipped lines (the text's end
    /// after the last line), and a top or inner border; each with its
    /// pieces in `layout.pieces`, the line's prefix first.
    bottoms: Vec<(usize, Range<u32>)>,
    borders: Vec<(usize, Range<u32>)>,
    /// Code blocks and inline code, sorted (`Parsed::code`).
    code: Vec<Range<usize>>,
}

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

/// A folded section: its heading's line and the lines it hides.
struct Fold {
    /// The heading line's start, and where its line break starts.
    heading: usize,
    end: usize,
    /// From the line after the heading to the next heading of its level
    /// or above, or the text's end.
    hidden: Range<usize>,
}

/// The folds kept by heading line start, moved by `change`: one at or
/// after the change moves with it, one inside what it removed is dropped.
fn shift(folds: &mut Vec<usize>, change: Change) {
    folds.retain_mut(|fold| {
        if *fold < change.at {
            true
        } else if *fold >= change.at + change.removed {
            *fold = *fold - change.removed + change.inserted;
            true
        } else {
            false
        }
    });
}

/// A row start's two top bits mark a row the viewer draws before the line
/// starting at the rest, a row with no text of the note (steps LV2, LT2).
/// Notes are far smaller, so no offset has them. Before one line they come
/// in this order: a table's bottom border, the viewer's blank row, a
/// table's top or inner border.
const MARK: usize = 3 << (usize::BITS - 2);
const BOTTOM: usize = 1 << (usize::BITS - 2);
const GAP: usize = 2 << (usize::BITS - 2);
const BORDER: usize = MARK;

/// Indents are slices of this.
const SPACES: &str = "                                                                ";

/// What is drawn left of a row's text in live mode.
struct Prefix<'a> {
    /// Spaces first: the section's indent before a line whose markup is
    /// drawn as written.
    spaces: u16,
    pieces: &'a [Piece],
    /// A code line: its block's widest line, for the gray box.
    code: Option<u16>,
}

impl Prefix<'_> {
    const NONE: Prefix<'static> = Prefix {
        spaces: 0,
        pieces: &[],
        code: None,
    };

    fn width(&self) -> usize {
        usize::from(self.spaces)
            + self
                .pieces
                .iter()
                .map(|(s, _)| display_width(s))
                .sum::<usize>()
    }
}

/// The lines live mode does not draw (step LV1), as `Live::skips` and
/// `Live::skip_end` say. One the cursor is on, the selection touches or
/// a revealed element covers (a code block's fence while the cursor is
/// in it) is drawn, so the cursor is never hidden.
fn skipped(
    layout: &Layout,
    text: &str,
    cursor: usize,
    selection: &Range<usize>,
    raw: &[Range<usize>],
) -> (Vec<Range<usize>>, bool) {
    let lines = &layout.lines;
    // Line `i`: its start, its end (its line break), the next line's start.
    let bounds = |i: usize| {
        let start = lines[i].markup.start;
        let next = lines.get(i + 1).map_or(text.len(), |l| l.markup.start);
        let end = if next > start && i + 1 < lines.len() {
            next - 1
        } else {
            next
        };
        (start, end, next)
    };
    let blank = |i: usize| {
        let (start, end, _) = bounds(i);
        lines[i].code.is_none() && text.get(start..end).is_some_and(|l| l.trim().is_empty())
    };
    let last_block = (0..lines.len())
        .rev()
        .find(|&i| !blank(i) && !lines[i].skip);
    let mut skips = Vec::new();
    let mut skip_end = false;
    let mut seen = false;
    let mut after_blank = false;
    for (i, line) in lines.iter().enumerate() {
        let (start, end, next) = bounds(i);
        // The cursor on the line, or the selection over any of it.
        let edited = (start..=end).contains(&cursor)
            || (!selection.is_empty() && selection.start <= end && start < selection.end);
        let skip = if line.skip {
            !overlaps(raw, &(start..end.max(start + 1)))
        } else if blank(i) {
            // Only the first blank line between two blocks has a row.
            let extra = !seen || after_blank || last_block.is_none_or(|last| i > last);
            if !extra || edited {
                after_blank = true;
            }
            extra
        } else {
            seen = true;
            after_blank = false;
            false
        };
        if skip && !edited {
            if next > start {
                skips.push(start..next);
            } else {
                // The empty line after a final line break.
                skip_end = true;
            }
        }
    }
    (skips, skip_end)
}

/// Adds a table border's pieces to `pieces` (step LT2): `prefix`, then
/// `left`, each column's dashes with `mid` between them, and `right`, in
/// the viewer's gray. Returns their range.
fn border(
    pieces: &mut Vec<Piece>,
    prefix: &[Piece],
    widths: &[u16],
    [left, mid, right]: [&'static str; 3],
) -> Range<u32> {
    let start = u32::try_from(pieces.len()).unwrap_or(u32::MAX);
    pieces.extend_from_slice(prefix);
    // `DASHES` has 3 bytes a dash.
    let most = DASHES.len() / 3;
    for (c, &width) in widths.iter().enumerate() {
        pieces.push((if c == 0 { left } else { mid }, QUOTE_BAR));
        let mut width = usize::from(width);
        while width > 0 {
            let n = width.min(most);
            pieces.push((&DASHES[..3 * n], QUOTE_BAR));
            width -= n;
        }
    }
    pieces.push((right, QUOTE_BAR));
    start..u32::try_from(pieces.len()).unwrap_or(u32::MAX)
}

/// Whether `range` overlaps one of the sorted, disjoint `ranges`.
fn overlaps(ranges: &[Range<usize>], range: &Range<usize>) -> bool {
    let i = ranges.partition_point(|r| r.end <= range.start);
    ranges.get(i).is_some_and(|r| r.start < range.end)
}

/// Sorts `ranges` and joins those that overlap or touch.
fn merged(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match out.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => out.push(range),
        }
    }
    out
}

impl Live {
    /// Live mode's look of `parsed` with the cursor at `cursor`. Drawn as
    /// written: the elements the cursor is in or beside (both ends count,
    /// so the cursor just after `**b**` reveals it) or the selection
    /// overlaps, and a line's markup when the cursor is in it, the
    /// selection or a revealed element (a heading, a code block) overlaps
    /// it. Every other line's markup is hidden. A table the cursor and
    /// the selection are not in is boxed if its box fits `width` columns
    /// (step LT2).
    fn new(
        parsed: Parsed,
        text: &str,
        cursor: usize,
        selection: &Range<usize>,
        kept: &mut Vec<usize>,
        width: usize,
    ) -> Self {
        let selected = |r: &Range<usize>| selection.start < r.end && r.start < selection.end;
        let elements = parsed
            .elements
            .iter()
            .filter(|e| (e.start..=e.end).contains(&cursor) || selected(e))
            .cloned()
            .collect();
        let elements = merged(elements);
        let mut raw = elements.clone();
        // The cursor or the selection in a fenced code block shows its
        // fences; its code stays styled, so it is typed in yellow on gray.
        for (block, fences) in &parsed.fenced {
            if (block.start..=block.end).contains(&cursor) || selected(block) {
                raw.extend(fences.iter().filter(|f| !f.is_empty()).cloned());
            }
        }
        let mut hidden = Vec::new();
        for line in &parsed.layout.lines {
            let markup = &line.markup;
            if markup.is_empty() {
                continue;
            }
            if markup.contains(&cursor) || selected(markup) || overlaps(&elements, markup) {
                raw.push(markup.clone());
            } else {
                hidden.push(markup.clone());
            }
        }
        // Folds: only on a heading that opens a section and has lines to
        // hide; one the cursor or the selection is in opens, for good.
        let headings = &parsed.layout.headings;
        let mut folds = Vec::new();
        kept.retain(|&start| {
            let Ok(i) = headings.binary_search_by_key(&start, |&(line, _)| line) else {
                return false;
            };
            let level = headings[i].1;
            let Some(newline) = text[start..].find('\n').map(|n| start + n) else {
                return false;
            };
            let end = if text[..newline].ends_with('\r') {
                newline - 1
            } else {
                newline
            };
            let section_end = headings[i + 1..]
                .iter()
                .find(|&&(_, l)| l <= level)
                .map_or(text.len(), |&(line, _)| line);
            let hidden = newline + 1..section_end;
            let entered = hidden.contains(&cursor)
                || (hidden.end == text.len() && cursor == text.len())
                || selected(&hidden);
            if hidden.is_empty() || entered {
                return false;
            }
            folds.push(Fold {
                heading: start,
                end,
                hidden,
            });
            true
        });
        let raw = merged(raw);
        let (skips, skip_end) = skipped(&parsed.layout, text, cursor, selection, &raw);
        // A gap before a skipped line (a fence) goes before the next line
        // drawn; none inside a folded section or past the text.
        let mut gaps: Vec<usize> = Vec::new();
        for line in parsed.layout.lines.iter().filter(|line| line.gap) {
            let mut at = line.markup.start;
            while let Some(skip) = skips.iter().find(|r| r.start == at) {
                at = skip.end;
            }
            // In a fold, or right after one: the viewer's fold hides the
            // blank row before the next heading too.
            let folded = folds
                .iter()
                .any(|f| f.hidden.contains(&at) || f.hidden.end == at);
            if at < text.len() && !folded && gaps.last() != Some(&at) {
                gaps.push(at);
            }
        }
        // Boxed tables: their border rows, none in a fold or right after
        // one (step LT2).
        let mut layout = parsed.layout;
        let (mut bottoms, mut borders) = (Vec::new(), Vec::new());
        let open = |at: usize| {
            !folds
                .iter()
                .any(|f| f.hidden.contains(&at) || f.hidden.end == at)
        };
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
            let edited = (start..=end).contains(&cursor)
                || (!selection.is_empty() && selection.start <= end && start < selection.end);
            let prefix = layout.prefix(&first.rest).to_vec();
            // `│ ` before each cell, ` ` after it, and the closing `│`.
            let boxed = prefix.iter().map(|(s, _)| display_width(s)).sum::<usize>()
                + table
                    .widths
                    .iter()
                    .map(|&w| usize::from(w) + 3)
                    .sum::<usize>()
                + 1;
            if edited || table.widths.is_empty() || boxed > width || !open(start) {
                continue;
            }
            let pieces = &mut layout.pieces;
            let top = border(pieces, &prefix, &table.widths, ["┌─", "─┬─", "─┐"]);
            borders.push((start, top));
            // Between two body rows, as the viewer draws them.
            for &(row, _) in table.rows.iter().skip(2) {
                let inner = border(pieces, &prefix, &table.widths, ["├─", "─┼─", "─┤"]);
                borders.push((start_of(row), inner));
            }
            let mut at = next;
            while let Some(skip) = skips.iter().find(|r| r.start == at) {
                at = skip.end;
            }
            if open(at) {
                let bottom = border(pieces, &prefix, &table.widths, ["└─", "─┴─", "─┘"]);
                bottoms.push((at, bottom));
            }
        }
        // A link drawn as written is being edited: a click there places
        // the cursor.
        let links = parsed
            .links
            .into_iter()
            .filter(|link| !overlaps(&raw, &link.range))
            .collect();
        Self {
            runs: parsed.runs,
            raw,
            hidden,
            layout,
            folds,
            links,
            skips,
            skip_end,
            gaps,
            bottoms,
            borders,
            code: parsed.code,
        }
    }

    /// Whether the viewer's added blank row goes before the line at `at`.
    fn gap_before(&self, at: usize) -> bool {
        self.gaps.binary_search(&at).is_ok()
    }

    /// Where the next row starts after a line break that ends at `at`:
    /// past any folded section and skipped lines; `None` past the text.
    /// The text's end after a line break is the empty last line, drawn
    /// unless skipped; after a fold reaching it, it is the fold's.
    fn next_row(&self, mut at: usize, text: &str) -> Option<usize> {
        let len = text.len();
        let mut folded = false;
        loop {
            if let Some(end) = self.skip(at) {
                at = end;
                folded = true;
            } else if let Some(range) = self.skips.iter().find(|r| r.start == at) {
                at = range.end;
                folded = false;
            } else {
                break;
            }
        }
        let gone = at >= len && (folded || self.skip_end || !text.ends_with('\n'));
        // The rows before the line first, if it has any; a table's bottom
        // border comes even after the last line.
        let first = self.first_row(at);
        (!gone || first & MARK != 0).then_some(first)
    }

    /// The first row of the line at `line`: the first marked row before
    /// it (step LT2), or the line itself.
    fn first_row(&self, line: usize) -> usize {
        self.after(line, 0).unwrap_or(line)
    }

    /// The marked row after the one marked `mark` before the line at
    /// `line` (0: the first), if any.
    fn after(&self, line: usize, mark: usize) -> Option<usize> {
        [BOTTOM, GAP, BORDER]
            .into_iter()
            .find(|&m| m > mark && self.marked(line | m))
            .map(|m| line | m)
    }

    /// Whether the marked row `at` is there.
    fn marked(&self, at: usize) -> bool {
        match at & MARK {
            GAP => self.gap_before(at & !MARK),
            0 => false,
            _ => self.border(at).is_some(),
        }
    }

    /// A border row's pieces: the bottom border, or the top or inner
    /// border, before the line of the row start `at`.
    fn border(&self, at: usize) -> Option<&[Piece]> {
        let rows = match at & MARK {
            BOTTOM => &self.bottoms,
            BORDER => &self.borders,
            _ => return None,
        };
        let i = rows.binary_search_by_key(&(at & !MARK), |r| r.0).ok()?;
        Some(self.layout.prefix(&rows[i].1))
    }

    /// The skipped lines `at` is in, if any.
    fn skipped_at(&self, at: usize) -> Option<&Range<usize>> {
        let i = self.skips.partition_point(|r| r.end <= at);
        self.skips.get(i).filter(|r| r.start <= at)
    }

    /// The link drawn at `at`, if any.
    fn link(&self, at: usize) -> Option<&Link> {
        let i = self.links.partition_point(|link| link.range.end <= at);
        self.links.get(i).filter(|link| link.range.start <= at)
    }

    /// The end of the folded section starting at `at`, if one does.
    fn skip(&self, at: usize) -> Option<usize> {
        self.folds
            .iter()
            .find(|fold| fold.hidden.start == at)
            .map(|fold| fold.hidden.end)
    }

    /// The heading line of the folded section `at` is in, if any.
    fn folded_heading(&self, at: usize) -> Option<usize> {
        self.folds
            .iter()
            .find(|fold| fold.hidden.contains(&at))
            .map(|fold| fold.heading)
    }

    /// Whether a folded heading's line ends at `end`.
    fn folded_end(&self, end: usize) -> bool {
        self.folds.iter().any(|fold| fold.end == end)
    }

    /// The prefix of the row starting at `start`: a line's first row gets
    /// its first prefix, or only its section's indent while its markup is
    /// drawn as written; a wrapped row its rest prefix.
    fn prefix(&self, start: usize) -> Prefix<'_> {
        let lines = &self.layout.lines;
        let i = lines.partition_point(|line| line.markup.start <= start);
        let Some(line) = i.checked_sub(1).and_then(|i| lines.get(i)) else {
            return Prefix::NONE;
        };
        if start != line.markup.start {
            return Prefix {
                spaces: 0,
                pieces: self.layout.prefix(&line.rest),
                code: line.code,
            };
        }
        if !line.markup.is_empty() && !overlaps(&self.hidden, &line.markup) {
            return Prefix {
                spaces: line.section,
                pieces: &[],
                code: None,
            };
        }
        Prefix {
            spaces: 0,
            pieces: self.layout.prefix(&line.first),
            code: line.code,
        }
    }

    /// How the grapheme starting at `at` is drawn, and its style. A fixed
    /// string is drawn once, at its run's start; the rest of the run is
    /// hidden.
    fn at(&self, at: usize) -> (Show, Style) {
        if overlaps(&self.raw, &(at..at + 1)) {
            // Code drawn as written keeps the code style (a fence, inline
            // code being edited).
            let code = overlaps(&self.code, &(at..at + 1));
            return (Show::Text, if code { CODE } else { Style::PLAIN });
        }
        if overlaps(&self.hidden, &(at..at + 1)) {
            return (Show::Hidden, Style::PLAIN);
        }
        let i = self.runs.partition_point(|run| run.range.end <= at);
        match self.runs.get(i) {
            Some(run) if run.range.start <= at => match run.show {
                Show::Fixed(_) if at != run.range.start => (Show::Hidden, run.style),
                show => (show, run.style),
            },
            _ => (Show::Text, Style::PLAIN),
        }
    }

    /// The columns the grapheme at `at` takes when it starts at `column`.
    fn width(&self, at: usize, grapheme: &str, column: usize) -> usize {
        match self.at(at).0 {
            Show::Text => width_at(grapheme, column),
            Show::Hidden => 0,
            Show::Fixed(fixed) => display_width(fixed),
        }
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
    /// Columns left of its text for live mode's prefix (0 when the prefix
    /// would leave no room).
    indent: usize,
    /// A marked row (steps LV2, LT2), its mark: the viewer's blank row
    /// or a table's border, no text of the note; it holds no position, a
    /// click there goes to the line below. 0 for a row of text.
    mark: usize,
}

impl Row {
    /// Whether the cursor at `at` is drawn on this row. At a wrap the
    /// position belongs to the next row; at a line break, to this one.
    fn contains(&self, at: usize) -> bool {
        // The next row's start, without its mark (steps LV2, LT2). A row
        // ending the text is followed by no line, at most a table's
        // bottom border: the text's end is its own.
        let end = if self.ends_line {
            match self.next.map(|next| next & !MARK) {
                Some(next) if next > self.range.end => next,
                _ => usize::MAX,
            }
        } else {
            self.range.end
        };
        self.range.start <= at && at < end
    }
}

/// The row starting at `start` (a row start), `width` columns wide. A
/// grapheme wider than a whole row gets a row of its own.
fn row_at(text: &str, start: usize, width: usize, live: &Live) -> Row {
    if start & MARK != 0 {
        let line = (start & !MARK).min(text.len());
        // The next marked row, else the line, unless past the text.
        let gone = line >= text.len() && (live.skip_end || !text.ends_with('\n'));
        let next = live.after(line, start & MARK);
        return Row {
            range: line..line,
            next: next.or((!gone).then_some(line)),
            ends_line: false,
            indent: 0,
            mark: start & MARK,
        };
    }
    // Live mode's prefix takes its columns first, if text still fits.
    let indent = live.prefix(start).width();
    let indent = if indent < width { indent } else { 0 };
    let width = width - indent;
    let rest = text.get(start..).unwrap_or("");
    let mut column = 0;
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
                mark: 0,
            };
        }
        let w = live.width(at, grapheme, column);
        if column > 0 && column + w > width {
            return Row {
                range: start..at,
                next: Some(at),
                ends_line: false,
                indent,
                mark: 0,
            };
        }
        column += w;
    }
    // A boxed table's bottom border may follow the last line.
    let next = live.after(text.len(), 0).filter(|_| !text.ends_with('\n'));
    Row {
        range: start..text.len(),
        next,
        ends_line: true,
        indent,
        mark: 0,
    }
}

fn line_start(text: &str, at: usize) -> usize {
    text.get(..at)
        .and_then(|head| head.rfind('\n'))
        .map_or(0, |i| i + 1)
}

/// The start of the row the cursor at `at` is drawn on.
fn row_start(text: &str, at: usize, width: usize, live: &Live) -> usize {
    // A marked row (a kept scroll position): itself while it is there.
    if at & MARK != 0 {
        let line = at & !MARK;
        if live.marked(at) && text.is_char_boundary(line.min(text.len())) {
            return at;
        }
        return row_start(text, line.min(text.len()), width, live);
    }
    // Inside a folded section: its heading's row.
    // In skipped lines: the next row drawn (no row holds `at` itself).
    if let Some(skip) = live.skipped_at(at) {
        return live.next_row(skip.start, text).unwrap_or(skip.start);
    }
    let mut start = live
        .folded_heading(at)
        .unwrap_or_else(|| line_start(text, at));
    loop {
        let row = row_at(text, start, width, live);
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
    look: Live,
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
        self.look = Live::default();
        if live {
            let cursor = editor.cursor();
            let selection = editor.selection().unwrap_or(cursor..cursor);
            let parsed = live::parse(text);
            self.look = Live::new(parsed, text, cursor, &selection, &mut self.folds, width);
        }
        self.scroll(text, editor.cursor(), width, rows);
        // The numbers given before the first row shown.
        let mut number = lines::count(&text[..line_start(text, self.top & !MARK)]);
        let mut start = Some(self.top);
        self.heading_rows.clear();
        for y in area.y..area.bottom() {
            let Some(next) = start else { break };
            let row = row_at(text, next, width, &self.look);
            start = row.next;
            // The viewer's added blank row: nothing to draw; a table's
            // border: its pieces.
            if row.mark != 0 {
                let mut x = area.x;
                for (piece, style) in self.look.border(next).unwrap_or(&[]) {
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

    /// Draws the editor's suggestion list over the frame `draw` just made
    /// (see the module comment), its names from `notes`. Nothing when the
    /// list is closed, the cursor was not drawn, or there is no room.
    pub fn draw_suggestions(&mut self, grid: &mut Grid, editor: &Editor, notes: &Notes) {
        /// The suggestion list's rows: the gray of code blocks in the viewer.
        const LIST: Style = Style::PLAIN.bg(Color::Indexed(236));
        const LIST_FOLDER: Style = LIST.mods(Mods::DIM);
        const LIST_HIGHLIGHT: Style = LIST.mods(Mods::REVERSED);
        const LIST_HIGHLIGHT_FOLDER: Style = LIST.mods(Mods::REVERSED.union(Mods::DIM));
        /// The narrowest list: a space each side and two columns, so a
        /// name cut short still shows a character and `…`.
        const LIST_MIN_WIDTH: u16 = 4;
        let (Some(list), Some(area), Some((cursor_x, cursor_y))) =
            (editor.suggestions(), self.area, self.cursor)
        else {
            return;
        };
        let screen = grid.area();
        // Rows below the cursor's row and above it, inside the text area.
        let below = area.bottom().saturating_sub(cursor_y + 1);
        let above = cursor_y.saturating_sub(area.y);
        let wanted = u16::try_from(list.entries.len().saturating_sub(list.top).min(ROWS))
            .unwrap_or(u16::MAX);
        let (rows, y) = if below >= wanted || below >= above {
            (wanted.min(below), cursor_y + 1)
        } else {
            (wanted.min(above), cursor_y - wanted.min(above))
        };
        if rows == 0 {
            return;
        }
        // The first row shown keeps the highlight in view when fewer
        // than `ROWS` rows fit.
        let first = list
            .top
            .max((list.highlight + 1).saturating_sub(usize::from(rows)));
        // Each row's text from the editor: a dimmed part (a folder, `# `),
        // the text, and a dimmed end (a block's ` ^id`). An entry without one stays a blank row, so rows
        // and `suggestion_at` agree.
        let shown = (first..list.entries.len())
            .take(usize::from(rows))
            .map(|index| {
                (
                    index,
                    editor
                        .suggestion_label(index, notes)
                        .unwrap_or(("", "", "")),
                )
            });
        let longest = shown
            .clone()
            .map(|(_, (dim, text, end))| {
                display_width(dim) + display_width(text) + display_width(end)
            })
            .max()
            .unwrap_or(0);
        let width = u16::try_from(longest.saturating_add(2))
            .unwrap_or(u16::MAX)
            .max(LIST_MIN_WIDTH)
            .min(screen.width / 2);
        if width < LIST_MIN_WIDTH {
            return;
        }
        let x = cursor_x.min(screen.right() - width);
        let mut drawn = 0;
        for (row, (index, (folder, title, end))) in (y..).zip(shown) {
            drawn += 1;
            let (plain, folder_style) = if index == list.highlight {
                (LIST_HIGHLIGHT, LIST_HIGHLIGHT_FOLDER)
            } else {
                (LIST, LIST_FOLDER)
            };
            grid.fill(Rect::new(x, row, width, 1), plain);
            let room = width - 2;
            let fits = display_width(folder) + display_width(title) + display_width(end)
                <= usize::from(room);
            // A name too long keeps its last column for `…`.
            let mut left = if fits { room } else { room - 1 };
            let mut column = x + 1;
            for (part, style) in [(folder, folder_style), (title, plain), (end, folder_style)] {
                let used = grid.put_str(column, row, part, style, left);
                column += used;
                left -= used;
            }
            if !fits {
                grid.put_str(x + width - 2, row, "…", plain, 1);
            }
        }
        if drawn > 0 {
            self.list = Some((Rect::new(x, y, width, drawn), first));
        }
    }

    /// The suggestion list's entry (an index into `List::notes`) drawn at
    /// the cell (`column`, `row`) in the last frame, if the list was drawn
    /// there (decision 0073's mouse).
    pub fn suggestion_at(&self, column: u16, row: u16) -> Option<usize> {
        let (area, first) = self.list?;
        let inside =
            (area.x..area.right()).contains(&column) && (area.y..area.bottom()).contains(&row);
        inside.then(|| first + usize::from(row - area.y))
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
        let top = if text.is_char_boundary(self.top & !MARK) {
            row_start(text, self.top, width, &self.look)
        } else {
            0
        };
        let mut start = top;
        for _ in area.y..y {
            match row_at(text, start, width, &self.look).next {
                Some(next) => start = next,
                None => return Some(text.len()),
            }
        }
        let row = row_at(text, start, width, &self.look);
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

    /// Moves `top` so the cursor's row is on screen.
    fn scroll(&mut self, text: &str, cursor: usize, width: usize, rows: usize) {
        if !text.is_char_boundary(self.top & !MARK) {
            self.top = 0;
        }
        // An edit or a new width may have moved the row starts. At the
        // text's start, the rows before its first line show (a table's
        // top border).
        let live = &self.look;
        self.top = row_start(text, self.top, width, live);
        if self.top == 0 {
            self.top = live.first_row(0);
        }
        if cursor < self.top & !MARK {
            self.top = row_start(text, cursor, width, live);
            return;
        }
        let mut start = self.top;
        for _ in 0..rows {
            let row = row_at(text, start, width, live);
            if row.contains(cursor) {
                return;
            }
            match row.next {
                Some(next) => start = next,
                None => return,
            }
        }
        // Below the screen: the cursor's row becomes the last one shown.
        self.top = top_for(text, cursor, width, rows, live);
    }
}

/// The start of the row `rows - 1` rows above the cursor's row, or the
/// text's first row, so the cursor's row is the last one shown.
///
/// Walks each logical line forward once, keeping only the last row starts
/// it needs: on a line of a million rows, finding the row above by
/// walking from the line's start again for every screen row took seconds
/// per key (found by review).
fn top_for(text: &str, cursor: usize, width: usize, rows: usize, live: &Live) -> usize {
    let mut need = rows.max(1);
    let mut line = line_start(text, cursor);
    // On the cursor's line, the walk stops at the cursor's row; on the
    // lines above, at their end.
    let mut until = Some(cursor);
    loop {
        let mut last = VecDeque::with_capacity(need);
        // A line's rows start with its marked rows, if it has any.
        let mut start = live.first_row(line);
        loop {
            if last.len() == need {
                last.pop_front();
            }
            last.push_back(start);
            let row = row_at(text, start, width, live);
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

/// Draws one row: its graphemes, the selection, and the cursor (after the
/// last grapheme when it sits at the line's end), each grapheme as `live`
/// says. Returns the column the cursor was drawn at, if on this row.
fn draw_row(grid: &mut Grid, area: Rect, editor: &Editor, row: &Row, live: &Live) -> Option<u16> {
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
    let selection = editor.selection().unwrap_or(0..0);
    let style_at = |at: usize| match (at == editor.cursor(), selection.contains(&at)) {
        (true, false) if !selection.is_empty() => CURSOR_BESIDE,
        (true, _) => CURSOR,
        (false, true) => SELECTED,
        (false, false) => live.at(at).1,
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
        if at == editor.cursor() {
            cursor = Some(x);
        }
        // Live mode: hidden markup takes no column, a fixed string stands
        // for its run. The cursor's lines are always drawn as written.
        match live.at(at).0 {
            Show::Hidden => continue,
            Show::Fixed(fixed) => {
                column += usize::from(grid.put_str(x, area.y, fixed, style, room));
                continue;
            }
            Show::Text => {}
        }
        match shape(grapheme) {
            Shape::Text(g, _) => {
                let used = grid.put_str(x, area.y, g, style, room);
                // A web link's cells carry its address for the terminal
                // (Ctrl+click), as in the viewer; the grid checks it again.
                if let Some(link) = live.link(at).filter(|link| link.web)
                    && let Some(target) = text.get(link.target.clone())
                {
                    let id = grid.add_link(target);
                    grid.set_link(x, area.y, used, id);
                }
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

    /// The scroll's first version, kept as the reference: from the
    /// cursor's row, step to the row above `rows - 1` times, each found by
    /// walking its line from the start. Right, but quadratic on long lines.
    fn top_by_rows(text: &str, cursor: usize, width: usize, rows: usize) -> usize {
        let mut top = row_start(text, cursor, width, &Live::default());
        for _ in 1..rows {
            let Some(c) = text.get(..top).and_then(|head| head.chars().next_back()) else {
                break;
            };
            top = row_start(text, top - c.len_utf8(), width, &Live::default());
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

    /// Notes named `names` (`folder/title`), and an editor on `text`
    /// with the cursor at its end that has typed `typed` through
    /// `key_in`, so the suggestion list is open.
    fn listing(names: &[&str], text: &str, typed: &str) -> (Editor, crate::notes::Notes) {
        listing_with(names, "", text, typed)
    }

    /// `listing` with `first_body` as the first note's text.
    fn listing_with(
        names: &[&str],
        first_body: &str,
        text: &str,
        typed: &str,
    ) -> (Editor, crate::notes::Notes) {
        use crate::notes::tests::{note, notes};
        let n = notes(
            names
                .iter()
                .zip(1u8..)
                .map(|(name, id)| {
                    let (folder, title) = name.rsplit_once('/').unwrap_or(("", name));
                    note(id, folder, title, if id == 1 { first_body } else { "" })
                })
                .collect(),
        );
        let mut ed = Editor::new(text).unwrap();
        let mut clip = Clipboard::default();
        ed.key(Key::Ctrl('a'), &mut clip);
        ed.key(Key::Right, &mut clip);
        for c in typed.chars() {
            ed.key_in(Key::Char(c), &mut clip, &n);
        }
        (ed, n)
    }

    /// The frame with the list drawn over it.
    fn with_list(ed: &mut Editor, n: &crate::notes::Notes, width: u16, height: u16) -> Grid {
        let mut view = View::default();
        let mut grid = drawn(ed, &mut view, width, height);
        view.draw_suggestions(&mut grid, ed, n);
        grid
    }

    /// The cells of row `y` in the list's gray, as (first column, text).
    fn gray_run(grid: &Grid, y: u16) -> Option<(u16, String)> {
        let gray = |x: u16| grid.cell(x, y).unwrap().style().bg == Color::Indexed(236);
        let first = (0..grid.width()).find(|&x| gray(x))?;
        let text = (first..grid.width())
            .take_while(|&x| gray(x))
            .map(|x| grid.cell(x, y).unwrap().symbol())
            .collect();
        Some((first, text))
    }

    #[test]
    fn list_opens_under_the_cursor() {
        let (mut ed, n) = listing(&["plan", "work/plan", "Restic Backup"], "", "[[");
        let grid = with_list(&mut ed, &n, 40, 10);
        // `1 [[]]`: the cursor is after `[[`, column 4 on row 1.
        assert_eq!(rows(&grid)[1], "1 [[]]");
        assert_eq!(gray_run(&grid, 2), Some((4, " plan          ".into())));
        assert_eq!(gray_run(&grid, 3), Some((4, " Restic Backup ".into())));
        assert_eq!(gray_run(&grid, 4), Some((4, " work/plan     ".into())));
        assert_eq!(gray_run(&grid, 5), None);
        // The first row is highlighted; folders are dimmed.
        let style = |x, y| grid.cell(x, y).unwrap().style().mods;
        assert!(style(5, 2).contains(Mods::REVERSED));
        assert!(!style(5, 3).contains(Mods::REVERSED));
        assert!(style(5, 4).contains(Mods::DIM));
        assert!(!style(10, 4).contains(Mods::DIM));
        // The text is untouched; the key line names the list.
        assert!(rows(&grid)[9].starts_with("Notes · Up/Down"));
    }

    /// The key line names the open list's kind, and is the usual one
    /// again once it closes.
    #[test]
    fn key_line_names_the_list() {
        let body = "# Plan\n\nA paragraph\n";
        let line = |typed: &str| {
            let (mut ed, n) = listing_with(&["plan"], body, "", typed);
            let grid = with_list(&mut ed, &n, 100, 12);
            rows(&grid)[11].clone()
        };
        assert!(line("[[pl").starts_with("Notes · "));
        assert!(line("[[plan#").starts_with("Headings · "));
        assert!(line("[[plan#^").starts_with("Blocks · "));
        assert!(line("[[plan#^").contains("an ^id is added if missing"));
        assert!(line("[[zz").starts_with("Ctrl+S save"), "no list");
    }

    #[test]
    fn list_opens_above_when_there_is_more_room() {
        let text = "\n".repeat(7);
        let (mut ed, n) = listing(&["a", "b", "c"], &text, "[[");
        // Rows 1 to 8 hold text, the cursor on row 8: no room below.
        let grid = with_list(&mut ed, &n, 30, 10);
        // Short names: the list is padded to its narrowest, 4 columns.
        assert_eq!(gray_run(&grid, 5).map(|r| r.1), Some(" a  ".into()));
        assert_eq!(gray_run(&grid, 7).map(|r| r.1), Some(" c  ".into()));
        assert_eq!(gray_run(&grid, 8), None);
        assert_eq!(gray_run(&grid, 9), None);
    }

    #[test]
    fn list_moves_left_to_fit_and_cuts_long_names() {
        let long = "a".repeat(30);
        let (mut ed, n) = listing(&[&long, "b"], &"x".repeat(25), "[[");
        // The cursor is at column 29; the list is half of 40 columns wide,
        // so it ends at the right edge.
        let grid = with_list(&mut ed, &n, 40, 6);
        let (x, row) = gray_run(&grid, 2).unwrap();
        assert_eq!(x, 20);
        assert_eq!(row, format!(" {}… ", "a".repeat(17)));
        assert_eq!(
            gray_run(&grid, 3).unwrap().1,
            format!(" b{} ", " ".repeat(17))
        );
    }

    #[test]
    fn wide_and_combining_names_cut_cleanly() {
        let (mut ed, n) = listing(&["日本語のノート名", "e\u{301}te\u{301}"], "", "[[");
        let grid = with_list(&mut ed, &n, 20, 6);
        // No gutter (too narrow); 10 columns: a space, 7 for the name,
        // `…`, a space. `é` (with a combining accent) sorts first; a wide
        // character that does not fit whole becomes a space.
        assert_eq!(
            gray_run(&grid, 2).unwrap(),
            (2, " e\u{301}te\u{301}      ".into())
        );
        assert_eq!(gray_run(&grid, 3).unwrap(), (2, " 日本語 … ".into()));
    }

    #[test]
    fn fewer_rows_keep_the_highlight_shown() {
        let names: Vec<String> = (0..12).map(|i| format!("n{i:02}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (mut ed, n) = listing(&names, "", "[[");
        let mut clip = Clipboard::default();
        for _ in 0..5 {
            ed.key_in(Key::Down, &mut clip, &n);
        }
        // Three rows of text area, the cursor on the first: two below.
        let grid = with_list(&mut ed, &n, 30, 5);
        assert_eq!(gray_run(&grid, 2).unwrap().1, " n04 ");
        assert_eq!(gray_run(&grid, 3).unwrap().1, " n05 ");
        assert!(
            grid.cell(6, 3)
                .unwrap()
                .style()
                .mods
                .contains(Mods::REVERSED)
        );
    }

    #[test]
    fn suggestion_at_maps_cells_to_entries() {
        let names: Vec<String> = (0..12).map(|i| format!("n{i:02}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (mut ed, n) = listing(&names, "", "[[");
        let mut view = View::default();
        let mut grid = drawn(&mut ed, &mut view, 40, 12);
        // Before the list is drawn, and with it drawn: rows 2 to 9 from
        // column 4, 5 columns wide.
        assert_eq!(view.suggestion_at(4, 2), None);
        view.draw_suggestions(&mut grid, &ed, &n);
        assert_eq!(view.suggestion_at(4, 2), Some(0));
        assert_eq!(view.suggestion_at(8, 9), Some(7));
        for (x, y) in [(3, 2), (9, 2), (4, 1), (4, 10)] {
            assert_eq!(view.suggestion_at(x, y), None, "({x}, {y})");
        }
        // Scrolled: the first row shows entry 4.
        let mut clip = Clipboard::default();
        for _ in 0..11 {
            ed.key_in(Key::Down, &mut clip, &n);
        }
        let mut grid = drawn(&mut ed, &mut view, 40, 12);
        view.draw_suggestions(&mut grid, &ed, &n);
        assert_eq!(view.suggestion_at(4, 2), Some(4));
        assert_eq!(view.suggestion_at(4, 9), Some(11));
        // A frame without the list forgets it.
        ed.key_in(Key::Esc, &mut clip, &n);
        drawn(&mut ed, &mut view, 40, 12);
        assert_eq!(view.suggestion_at(4, 2), None);
    }

    #[test]
    fn no_list_without_room() {
        let (mut ed, n) = listing(&["plan"], "", "[[");
        for (width, height) in [(1, 1), (1, 5), (7, 5), (40, 2), (40, 3)] {
            let grid = with_list(&mut ed, &n, width, height);
            for y in 0..height {
                assert_eq!(gray_run(&grid, y), None, "{width}x{height}");
            }
        }
        // A closed list draws nothing.
        ed.key_in(Key::Esc, &mut Clipboard::default(), &n);
        let grid = with_list(&mut ed, &n, 40, 10);
        assert_eq!(gray_run(&grid, 2), None);
    }

    proptest! {
        /// Any screen size, names (wide, combining, emoji) and highlight,
        /// for a list of notes or of the first note's headings (the names
        /// again, as `# NAME` lines): the list stays off the title and key
        /// lines, never covers the cursor, shows the highlight when drawn,
        /// and no cell holds a control character.
        #[test]
        fn suggestion_list_stays_in_bounds(
            width in 1u16..50,
            height in 1u16..14,
            names in prop::collection::btree_set("[a-c日é😀]{1,3}(\u{301})?[a-z]{0,20}", 1..15),
            lines in 0usize..12,
            downs in 0usize..20,
            kind in 0u8..3,
            hostile in prop::collection::vec(
                "[a-c\u{0}-\u{1f}\u{7f}-\u{9f}\u{202e}\u{2066}\u{200b}日é😀\t]{1,12}",
                1..8,
            ),
        ) {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let text = "x\n".repeat(lines);
            // Its headings: the names; its blocks: hostile paragraphs.
            let mut body: String = names.iter().map(|name| format!("# {name}\n\n")).collect();
            body.push_str(&hostile.join("\n\n"));
            let typed = match kind {
                0 => "[[".to_owned(),
                1 => format!("[[{}#", names[0]),
                _ => format!("[[{}#^", names[0]),
            };
            let (mut ed, n) = listing_with(&names, &body, &text, &typed);
            let mut clip = Clipboard::default();
            for _ in 0..downs {
                ed.key_in(Key::Down, &mut clip, &n);
            }
            let mut view = View::default();
            let mut grid = drawn(&mut ed, &mut view, width, height);
            let cursor = cursor_cell(&grid);
            view.draw_suggestions(&mut grid, &ed, &n);
            prop_assert_eq!(cursor_cell(&grid), cursor);
            let mut highlighted = false;
            let mut drawn_any = false;
            use crate::editor::suggest::Source;
            let Some(list) = ed.suggestions() else {
                // Hostile paragraphs can all be code or HTML: no blocks.
                prop_assert_eq!(kind, 2);
                return Ok(());
            };
            let expected = match kind {
                0 => matches!(list.source, Source::Notes),
                1 => matches!(list.source, Source::Headings(_)),
                _ => matches!(list.source, Source::Blocks(_)),
            };
            prop_assert!(expected);
            for y in 0..height {
                for x in 0..width {
                    let cell = grid.cell(x, y).unwrap();
                    prop_assert!(!cell.symbol().chars().any(char::is_control));
                    prop_assert!(!cell.symbol().chars().any(crate::ui::grid::is_forbidden));
                    let gray = cell.style().bg == Color::Indexed(236);
                    // The mouse finds an entry exactly where one is drawn,
                    // and the highlighted entry is the reversed row.
                    let entry = view.suggestion_at(x, y);
                    prop_assert_eq!(gray, entry.is_some());
                    if let Some(entry) = entry {
                        drawn_any = true;
                        prop_assert!(entry < list.entries.len());
                        prop_assert!(y > 0 && y + 1 < height, "list on row {y}");
                        let reversed = cell.style().mods.contains(Mods::REVERSED);
                        prop_assert_eq!(reversed, entry == list.highlight);
                        highlighted |= reversed;
                    }
                }
            }
            prop_assert_eq!(drawn_any, highlighted);
        }

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
                top_for(&text, cursor, width, rows, &Live::default()),
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
            moves in prop::collection::vec(0u8..5, 0..12),
        ) {
            let mut clip = Clipboard::default();
            let mut ed = Editor::new(&text).unwrap();
            for m in moves {
                let key = match m {
                    0 => Key::Up,
                    1 => Key::Down,
                    2 => Key::Left,
                    3 => Key::Right,
                    _ => Key::ShiftDown,
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

    /// Folds move with each change: kept before it, moved after it,
    /// dropped inside what it removed.
    #[test]
    fn folds_move_with_changes() {
        let mut folds = vec![10, 20, 30];
        shift(
            &mut folds,
            Change {
                at: 15,
                removed: 10,
                inserted: 2,
            },
        );
        assert_eq!(folds, [10, 22]);
        // Typed at a heading's start (a line break before it): it moves.
        shift(
            &mut folds,
            Change {
                at: 10,
                removed: 0,
                inserted: 1,
            },
        );
        assert_eq!(folds, [11, 23]);
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
    /// LT2): above it, between two body rows, below it (before a blank
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
                "  | a | b |",
                "  |---|---|",
                "  | 1 | 2 |",
                "  ├───┼───┤",
                "  | 3 | 4 |",
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
        // The box takes 9 columns, more than the row's 8.
        let grid = drawn(&mut ed, &mut View::live(), 9, 12);
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
                    "    | a |",
                    "    |---|",
                    "    | 1 |",
                    "    └───┘",
                    ""
                ],
                "{text:?}"
            );
        }
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
    /// subheadings, a link, a paragraph right before a list): every word
    /// is drawn in the viewer's row and column, the cursor away from them.
    #[test]
    fn live_rows_are_the_viewers() {
        let text = "# Heading\n\n- Item one\n- Item two\n\nThis is a **normal** paragraph \
                    with *italic* alpha:\n\n```rust\npub enum bravo {\n    charlie,\n}\n```\n\n\
                    Look delta `another` echo.\n\n## Subheading\n\n### Subsub\n\nThis is \
                    foxtrot [Monero](https://getmonero.org)\nI am golf testing:\n- \
                    [[hotel#^3y5wrv]]\n\nmike end";
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
            "mike",
        ];
        let page = crate::md::render::render(text, crate::md::render::Kind::Markdown, 70);
        let mut viewer = Grid::new(70, 40);
        page.draw(&mut viewer, Rect::new(0, 0, 70, 40), 0);
        let mut ed = Editor::new(text).unwrap();
        ed.jump_to(text.len());
        let live = drawn(&mut ed, &mut View::live(), 73, 42);
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
