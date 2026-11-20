//! Live mode's look of one frame (step RF3): which bytes of the note are
//! drawn as written, which are hidden or replaced, which lines get no row,
//! which rows are drawn that the note has no line for, and which tables
//! are boxed. `View::draw` makes a new `Look` on every frame from
//! `live::parse`; nothing in it outlives the frame but the folds the view
//! keeps by offset (`View::folds`), which `Look::new` checks and prunes.
//!
//! The rule all of it serves: the cursor is never hidden. Whatever the
//! cursor is in or beside, or the selection overlaps, is drawn as written,
//! so it can be seen and edited; only text away from the cursor is
//! styled, folded, skipped or boxed. A boxed table is the one exception
//! (step TB1): it stays boxed with the cursor in a cell's text, which the
//! box draws, and is drawn as written with the cursor anywhere else in it. Each step of `Look::new` applies the
//! rule to one kind of thing.
//!
//! Not live, the look is `Look::default()`: everything empty, so every
//! grapheme is drawn as written, with no prefix and no drawn rows.

use std::ops::Range;

use super::fold::{Fold, folds};
use super::glyph::width_at;
use super::rows::RowStart;
use super::table::{Boxed, tables};
use crate::editor::live::layout::{Layout, Piece};
use crate::editor::live::{Link, Parsed, Run, Show};
use crate::md::render::CODE;
use crate::ui::grid::{Style, display_width};

/// How live mode draws the text (steps LE1 to LE7, LR1, LI2): the runs
/// and the line layout of the last frame (`live::parse`), the bytes drawn
/// as written (sorted, merged) and the lines' leading markup hidden behind
/// their prefix (sorted). Not live: all empty, so every grapheme is drawn
/// as written, with no prefix.
#[derive(Default)]
pub struct Look {
    pub runs: Vec<Run>,
    raw: Vec<Range<usize>>,
    hidden: Vec<Range<usize>>,
    pub layout: Layout,
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
    pub skip_end: bool,
    /// The rows drawn before a line that the note has no line for (steps
    /// LV2, LT2, LW1), sorted by that line (a line drawn, past any
    /// skipped ones; the text's end after the last line), each line's in
    /// order: a table's bottom border, the viewer's blank row, a table's
    /// top or inner border.
    drawn: Vec<(usize, Vec<Drawn>)>,
    /// The boxed tables' lines, sorted (step LW2).
    pub boxed: Vec<Boxed>,
    /// Code blocks and inline code, sorted (`Parsed::code`).
    code: Vec<Range<usize>>,
}

/// A row the note has no line for (steps LV2, LT2, LW1).
#[derive(Clone)]
pub enum Drawn {
    /// The viewer's blank row between two blocks.
    Gap,
    /// A table's border: pieces of `Layout::pieces`, its prefix first.
    Pieces(Range<u32>),
    /// Row `row` (from 1) of the boxed table line `boxed` (an index of
    /// `Look::boxed`), whose cells wrap (step LW3).
    Cells { boxed: usize, row: usize },
}

/// What is drawn left of a row's text in live mode.
pub struct Prefix<'a> {
    /// Spaces first: the section's indent before a line whose markup is
    /// drawn as written.
    pub spaces: u16,
    pub pieces: &'a [Piece],
    /// A code line: its block's widest line, for the gray box.
    pub code: Option<u16>,
}

impl Prefix<'_> {
    const NONE: Prefix<'static> = Prefix {
        spaces: 0,
        pieces: &[],
        code: None,
    };

    pub fn width(&self) -> usize {
        usize::from(self.spaces)
            + self
                .pieces
                .iter()
                .map(|(s, _)| display_width(s))
                .sum::<usize>()
    }
}

/// The lines live mode does not draw (step LV1), as `Look::skips` and
/// `Look::skip_end` say. One the cursor is on, the selection touches or
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

/// The drawn rows grouped by the line they go before, in their order
/// (`rank`, then as found), sorted by line (step LW1).
fn by_line(mut drawn: Vec<(usize, u8, Drawn)>) -> Vec<(usize, Vec<Drawn>)> {
    drawn.sort_by_key(|&(line, rank, _)| (line, rank));
    let mut out: Vec<(usize, Vec<Drawn>)> = Vec::new();
    for (line, _, row) in drawn {
        match out.last_mut() {
            Some((last, rows)) if *last == line => rows.push(row),
            _ => out.push((line, vec![row])),
        }
    }
    out
}

/// Where the line starting at `start` ends: its line break (`\r\n`'s
/// `\r`) or the text's end.
pub fn line_end(text: &str, start: usize) -> usize {
    let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    if text[..end].ends_with('\r') {
        end - 1
    } else {
        end
    }
}

/// How the run of `runs` (sorted) holding `at` draws it, if one does: a
/// fixed string or a cell border at its start, nothing on its other
/// bytes.
fn run_at(runs: &[Run], at: usize) -> Option<(Show, Style)> {
    let run = runs.get(runs.partition_point(|run| run.range.end <= at))?;
    (run.range.start <= at).then(|| match run.show {
        show if show.columns().is_some() && at != run.range.start => (Show::Hidden, run.style),
        show => (show, run.style),
    })
}

/// `runs` with the bytes of `raw` (sorted, merged) drawn as written
/// (step TB1): what they measure is what is drawn, so a boxed table's
/// columns, wrapping and padding count markup revealed in a cell at its
/// written width. A run split by `raw` keeps its show on its first part;
/// a fixed string's later parts are hidden, as `run_at` draws them. Runs
/// `raw` does not touch are kept as they are, so drawing (`Look::at`,
/// which checks `raw` first) is unchanged.
fn revealed(runs: Vec<Run>, raw: &[Range<usize>]) -> Vec<Run> {
    if raw.is_empty() {
        return runs;
    }
    let mut out = Vec::with_capacity(runs.len());
    for run in runs {
        if !overlaps(raw, &run.range) {
            out.push(run);
            continue;
        }
        let mut at = run.range.start;
        while at < run.range.end {
            let i = raw.partition_point(|r| r.end <= at);
            let (inside, stop) = match raw.get(i) {
                Some(r) if r.start <= at => (true, r.end.min(run.range.end)),
                Some(r) => (false, r.start.min(run.range.end)),
                None => (false, run.range.end),
            };
            let show = match run.show {
                _ if inside => Show::Text,
                show if at == run.range.start || show == Show::Text => show,
                _ => Show::Hidden,
            };
            out.push(Run {
                range: at..stop,
                show,
                style: run.style,
            });
            at = stop;
        }
    }
    out
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

/// Where the cursor and the selection are: what `Look::new`'s steps
/// draw as written, so it is never hidden.
#[derive(Clone, Copy)]
pub struct Editing<'a> {
    pub cursor: usize,
    pub selection: &'a Range<usize>,
}

impl Editing<'_> {
    /// Whether the selection overlaps `r`.
    pub fn selects(&self, r: &Range<usize>) -> bool {
        self.selection.start < r.end && r.start < self.selection.end
    }

    /// Whether the cursor is on the lines from `start` to `end` (their
    /// last line break), or a selection overlaps them.
    pub fn on_lines(&self, start: usize, end: usize) -> bool {
        (start..=end).contains(&self.cursor)
            || (!self.selection.is_empty()
                && self.selection.start <= end
                && start < self.selection.end)
    }
}

/// What hides lines from the row walk: the skipped lines (`skipped`) and
/// the folded sections (`folds`). A drawn row (a blank row, a border)
/// goes before the line that is drawn, never into a fold.
pub struct Hiding<'a> {
    skips: &'a [Range<usize>],
    folds: &'a [Fold],
}

impl Hiding<'_> {
    /// The first line drawn from `at` on: past any skipped lines (a fence)
    /// starting there.
    pub fn past_skipped(&self, mut at: usize) -> usize {
        while let Some(skip) = self.skips.iter().find(|r| r.start == at) {
            at = skip.end;
        }
        at
    }

    /// Whether a drawn row may go before `at`: not in a fold, nor right
    /// after one (the viewer's fold hides the blank row before the next
    /// heading too).
    pub fn open(&self, at: usize) -> bool {
        !self
            .folds
            .iter()
            .any(|f| f.hidden.contains(&at) || f.hidden.end == at)
    }
}

/// The order of the drawn rows before one line (`by_line`): a wrapped
/// table row's further rows, a table's bottom border, the viewer's blank
/// row, a table's top or inner border.
pub const CONTINUED: u8 = 0;
pub const BOTTOM: u8 = 1;
pub const GAP: u8 = 2;
pub const BORDER: u8 = 3;

/// Step 1, what is drawn as written: the elements the cursor is in or
/// beside (both ends count, so the cursor just after `**b**` reveals it)
/// or the selection overlaps, and a line's markup when the cursor is in
/// it, the selection or a revealed element (a heading, a code block)
/// overlaps it (sorted, merged). Every other line's markup is hidden
/// behind its prefix (the second list, sorted).
fn reveal(parsed: &Parsed, editing: Editing) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let cursor = editing.cursor;
    let elements = parsed
        .elements
        .iter()
        .filter(|e| (e.start..=e.end).contains(&cursor) || editing.selects(e))
        .cloned()
        .collect();
    let elements = merged(elements);
    let mut raw = elements.clone();
    // The cursor or the selection in a fenced code block shows its
    // fences; its code stays styled, so it is typed in yellow on gray.
    for (block, fences) in &parsed.fenced {
        if (block.start..=block.end).contains(&cursor) || editing.selects(block) {
            raw.extend(fences.iter().filter(|f| !f.is_empty()).cloned());
        }
    }
    let mut hidden = Vec::new();
    for line in &parsed.layout.lines {
        let markup = &line.markup;
        if markup.is_empty() {
            continue;
        }
        if markup.contains(&cursor) || editing.selects(markup) || overlaps(&elements, markup) {
            raw.push(markup.clone());
        } else {
            hidden.push(markup.clone());
        }
    }
    (merged(raw), hidden)
}

/// Step 4, the viewer's blank rows between blocks (step LV2): each drawn
/// row's line, its rank (`GAP`) and the row. A gap before a skipped line
/// (a fence) goes before the next line drawn; none inside a folded
/// section or past the text, and one per line.
fn gaps(layout: &Layout, text: &str, hiding: &Hiding) -> Vec<(usize, u8, Drawn)> {
    let mut drawn: Vec<(usize, u8, Drawn)> = Vec::new();
    for line in layout.lines.iter().filter(|line| line.gap) {
        let at = hiding.past_skipped(line.markup.start);
        if at < text.len() && hiding.open(at) && drawn.last().is_none_or(|d| d.0 != at) {
            drawn.push((at, GAP, Drawn::Gap));
        }
    }
    drawn
}

impl Look {
    /// Live mode's look of `parsed` with the cursor at `cursor` and the
    /// selection `selection`, for a screen `width` columns wide; `kept` is
    /// the view's folds, pruned here.
    ///
    /// Made in steps, each a function, in the order each needs the ones
    /// before: what is drawn as written (`reveal`), the folds
    /// (`fold::folds`), the lines with no row (`skipped`, which needs what
    /// is revealed: a code block's fence under the cursor keeps its row),
    /// the viewer's blank rows (`gaps`) and the boxed tables
    /// (`table::tables`), which both need the lines hidden, and last the
    /// links a click follows.
    pub fn new(
        parsed: Parsed,
        text: &str,
        cursor: usize,
        selection: &Range<usize>,
        kept: &mut Vec<usize>,
        width: usize,
    ) -> Self {
        let editing = Editing { cursor, selection };
        let (raw, hidden) = reveal(&parsed, editing);
        let folds = folds(kept, &parsed.layout.headings, text, editing);
        let (skips, skip_end) = skipped(&parsed.layout, text, cursor, selection, &raw);
        let hiding = Hiding {
            skips: &skips,
            folds: &folds,
        };
        let mut drawn = gaps(&parsed.layout, text, &hiding);
        let mut layout = parsed.layout;
        let runs = revealed(parsed.runs, &raw);
        let boxed = tables(
            &mut layout,
            &runs,
            text,
            editing,
            width,
            &hiding,
            &mut drawn,
        );
        // A link drawn as written is being edited: a click there places
        // the cursor.
        let links = parsed
            .links
            .into_iter()
            .filter(|link| !overlaps(&raw, &link.range))
            .collect();
        Self {
            runs,
            raw,
            hidden,
            layout,
            folds,
            links,
            skips,
            skip_end,
            drawn: by_line(drawn),
            boxed,
            code: parsed.code,
        }
    }

    /// Where the next row starts after a line break that ends at `at`:
    /// past any folded section and skipped lines; `None` past the text.
    /// The text's end after a line break is the empty last line, drawn
    /// unless skipped; after a fold reaching it, it is the fold's.
    pub fn next_row(&self, mut at: usize, text: &str) -> Option<RowStart> {
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
        (!gone || first.is_drawn()).then_some(first)
    }

    /// The first row of the line at `line`: the first drawn row before
    /// it, or the line itself.
    pub fn first_row(&self, line: usize) -> RowStart {
        if self.before(line).is_empty() {
            RowStart::text(line)
        } else {
            RowStart::drawn(line)
        }
    }

    /// The drawn rows before the line at `line`, in order.
    fn before(&self, line: usize) -> &[Drawn] {
        match self.drawn.binary_search_by_key(&line, |(l, _)| *l) {
            Ok(i) => &self.drawn[i].1,
            Err(_) => &[],
        }
    }

    /// The drawn row the row start `at` is, if it is one and still there.
    pub fn drawn_row(&self, at: RowStart) -> Option<&Drawn> {
        self.before(at.line()).get(at.ordinal()?)
    }

    /// The drawn row after the drawn row `at`, before the same line.
    pub fn next_drawn(&self, at: RowStart) -> Option<RowStart> {
        let next = at.next_drawn();
        self.drawn_row(next).is_some().then_some(next)
    }

    /// A drawn row's pieces (a table's border); none for a blank row.
    pub fn pieces(&self, at: RowStart) -> &[Piece] {
        match self.drawn_row(at) {
            Some(Drawn::Pieces(range)) => self.layout.prefix(range),
            _ => &[],
        }
    }

    /// The skipped lines `at` is in, if any.
    pub fn skipped_at(&self, at: usize) -> Option<&Range<usize>> {
        let i = self.skips.partition_point(|r| r.end <= at);
        self.skips.get(i).filter(|r| r.start <= at)
    }

    /// The link drawn at `at`, if any.
    pub fn link(&self, at: usize) -> Option<&Link> {
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
    pub fn folded_heading(&self, at: usize) -> Option<usize> {
        self.folds
            .iter()
            .find(|fold| fold.hidden.contains(&at))
            .map(|fold| fold.heading)
    }

    /// Whether a folded heading's line ends at `end`.
    pub fn folded_end(&self, end: usize) -> bool {
        self.folds.iter().any(|fold| fold.end == end)
    }

    /// The prefix of the row starting at `start`: a line's first row gets
    /// its first prefix, or only its section's indent while its markup is
    /// drawn as written; a wrapped row its rest prefix.
    pub fn prefix(&self, start: usize) -> Prefix<'_> {
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
    pub fn at(&self, at: usize) -> (Show, Style) {
        if overlaps(&self.raw, &(at..at + 1)) {
            // Code drawn as written keeps the code style (a fence, inline
            // code being edited).
            let code = overlaps(&self.code, &(at..at + 1));
            return (Show::Text, if code { CODE } else { Style::PLAIN });
        }
        if overlaps(&self.hidden, &(at..at + 1)) {
            return (Show::Hidden, Style::PLAIN);
        }
        run_at(&self.runs, at).unwrap_or((Show::Text, Style::PLAIN))
    }

    /// The boxed table line starting at `start`, if one does (step LW2).
    pub fn boxed_at(&self, start: usize) -> Option<&Boxed> {
        let i = self.boxed.binary_search_by_key(&start, |b| b.start).ok()?;
        self.boxed.get(i)
    }

    /// The columns the grapheme at `at` takes when it starts at `column`.
    pub fn width(&self, at: usize, grapheme: &str, column: usize) -> usize {
        match self.at(at).0 {
            Show::Text => width_at(grapheme, column),
            Show::Hidden => 0,
            show => show.columns().unwrap_or(0),
        }
    }
}
