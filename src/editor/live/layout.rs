//! Live mode's line layout (step LI1): each line of the note indented as
//! the viewer indents it (`md::render`, decision 0052).
//!
//! The viewer puts a prefix before every row: the section's indent (2
//! columns per heading level, for what follows a heading outside quotes
//! and lists), then for each container a quote's `│ ` or a list item's
//! marker on its first row and spaces of the marker's width after; a
//! heading's icon hangs the same way. Live mode gives each line of the
//! note the same prefix, in place of the line's own leading markup
//! (spaces, `>`, `-`, `#`), which the view hides unless the cursor is in
//! it. Inside a code block the prefix ends in a gray space and the line
//! gets a gray box as wide as the block's widest line.
//!
//! A line is laid out when its first content arrives, from the parser
//! events that `live::parse` reads: its markup is what lies before that
//! content. A line without content is markup only: a code fence or a
//! Setext heading's underline is hidden whole, any other (a blank line,
//! a link definition) only its leading spaces and, in a quote, `>`s.
//! Ordered lists keep their numbers as written: they are the note's text.
//! Tables are recorded whole (step LT1), for the view to box.
//! Holds offsets and fixed strings, never note text.

use std::ops::Range;

use pulldown_cmark::{Alignment, Event, Tag, TagEnd};

use crate::md::render::{CODE, QUOTE_BAR, heading_mark};
use crate::ui::grid::{Mods, Style, display_width};

/// Containers deeper than this indent no more, as in the viewer.
const MAX_LEVELS: usize = 8;
/// Indents are slices of this.
const SPACES: &str = "                                                                ";
/// A tab in a code line counts this many columns for the box's width.
const TAB: usize = 4;

/// One part of a prefix: fixed text and its style.
pub type Piece = (&'static str, Style);

/// One line of the note, laid out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// From the line's start: its leading markup, which the prefix stands
    /// in for (empty when the line starts with its content).
    pub markup: Range<usize>,
    /// The prefix of its first row and of its wrapped rows, as ranges of
    /// `Layout::pieces`.
    pub first: Range<u32>,
    pub rest: Range<u32>,
    /// The section's indent, kept when the markup is shown as written.
    pub section: u16,
    /// A fence or a Setext heading's underline: the viewer gives it no
    /// row, so live mode draws none while it is not edited (step LV1).
    pub skip: bool,
    /// The viewer draws a blank row before it that the note does not
    /// have: the first line of a block right after another (step LV2).
    pub gap: bool,
    /// In a code block: its widest line, in columns, for the gray box.
    pub code: Option<u16>,
}

/// The note's lines, in order, and the pieces their prefixes use.
#[derive(Default)]
pub struct Layout {
    pub lines: Vec<Line>,
    pub pieces: Vec<Piece>,
    /// The headings that open a section (outside quotes and lists, as in
    /// the viewer): their line's start and level, in order (step LF1).
    pub headings: Vec<(usize, u8)>,
    /// The tables, in order (step LT1).
    pub tables: Vec<Table>,
}

/// A table, for the view to box as the viewer does (step LT1).
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    /// Its lines, as indexes of `Layout::lines`: the header, the `|---|`
    /// line, the body rows.
    pub lines: Range<usize>,
    /// Each column's alignment; there are as many columns.
    pub aligns: Vec<Alignment>,
    /// Each row's line (an index of `Layout::lines`) and its cells'
    /// text, without the spaces around it. A short row has fewer: the
    /// parser's empty cells for its missing columns are left out (step
    /// TB5), as they are no place in the row (they sit at its end, past
    /// its line break), so every cell is a place the cursor may be.
    pub rows: Vec<(usize, Vec<Range<usize>>)>,
    /// Each column's widest cell as drawn, in columns (`live::parse`
    /// sets it from the runs: hidden markup takes none).
    pub widths: Vec<u16>,
}

impl Layout {
    /// The pieces of `range` (a line's `first` or `rest`).
    pub fn prefix(&self, range: &Range<u32>) -> &[Piece] {
        let (start, end) = (range.start as usize, range.end as usize);
        self.pieces.get(start..end).unwrap_or(&[])
    }
}

/// A container a line can be inside, as in the viewer.
enum Container {
    Quote,
    /// A list: whether it is numbered.
    List(bool),
    /// A list item or a heading: its marker, shown on its first line
    /// (`None` for a numbered item: the number is the note's text, from
    /// `number`), and the columns it takes.
    Item {
        marker: Option<Piece>,
        number: usize,
        width: u16,
        shown: bool,
    },
}

/// Builds the layout from the parser's events.
pub struct Builder<'a> {
    text: &'a str,
    layout: Layout,
    /// The start of the first line not laid out; past the text when all
    /// are.
    next: usize,
    /// Where that line ends (kept: found once per line, not per event).
    next_end: usize,
    containers: Vec<Container>,
    section: u16,
    /// Inside a code block: the index of its first line laid out.
    code: Option<usize>,
    /// Inside a fenced code block or a heading: a line without content
    /// (a fence, an underline) is hidden whole.
    hide_bare: bool,
    /// Inside a heading: whether its text's line is laid out (a bare line
    /// after it is a Setext underline).
    heading: Option<bool>,
    /// A block ended: the viewer puts a blank row before the next one
    /// (its `blank`), unless the note has a blank line there.
    ended: bool,
    /// The last line laid out (fences and underlines aside) is blank: the
    /// note's own separator.
    blank_since: bool,
    /// The next line laid out gets `Line::gap`.
    gap_next: bool,
    /// The table being read, and where its current row ends.
    table: Option<Table>,
    row_end: usize,
    /// The last prefix interned, to share it with the next line.
    last: Range<u32>,
    /// A prefix being built, kept to spare an allocation per line.
    scratch: Vec<Piece>,
}

impl<'a> Builder<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            layout: Layout::default(),
            next: 0,
            next_end: text.find('\n').unwrap_or(text.len()),
            containers: Vec::new(),
            section: 0,
            code: None,
            hide_bare: false,
            heading: None,
            table: None,
            row_end: 0,
            last: 0..0,
            scratch: Vec::new(),
            ended: false,
            blank_since: false,
            gap_next: false,
        }
    }

    /// Reads one parser event and its source range.
    pub fn event(&mut self, event: &Event<'_>, range: &Range<usize>) {
        match event {
            Event::Start(tag) => match tag {
                Tag::BlockQuote(_) | Tag::List(_) | Tag::Item | Tag::Heading { .. } => {
                    self.before(range.start);
                    if !matches!(tag, Tag::Item) {
                        self.block();
                    }
                    self.open(tag, range.start);
                }
                Tag::CodeBlock(kind) => {
                    self.before(range.start);
                    self.block();
                    self.code = Some(usize::MAX);
                    self.hide_bare = matches!(kind, pulldown_cmark::CodeBlockKind::Fenced(_));
                }
                Tag::Paragraph | Tag::Table(_) | Tag::HtmlBlock => {
                    self.before(range.start);
                    self.block();
                    match tag {
                        Tag::Paragraph => self.content(range),
                        Tag::Table(aligns) => {
                            let first = self.layout.lines.len();
                            self.table = Some(Table {
                                lines: first..first,
                                aligns: aligns.clone(),
                                rows: Vec::new(),
                                widths: Vec::new(),
                            });
                        }
                        _ => {}
                    }
                }
                // A row starts at its line's content: its line is the
                // last laid out.
                Tag::TableHead | Tag::TableRow => {
                    self.row_end = range.end;
                    self.content(range);
                    let line = self.layout.lines.len().saturating_sub(1);
                    if let Some(table) = &mut self.table {
                        table.rows.push((line, Vec::new()));
                    }
                }
                // These start at their line's content.
                Tag::Emphasis
                | Tag::Strong
                | Tag::Strikethrough
                | Tag::Link { .. }
                | Tag::Image { .. } => self.content(range),
                _ => {}
            },
            Event::End(tag) => {
                // As the viewer: after these, a blank row before the next
                // block.
                if matches!(
                    tag,
                    TagEnd::Paragraph
                        | TagEnd::HtmlBlock
                        | TagEnd::Table
                        | TagEnd::Heading(_)
                        | TagEnd::CodeBlock
                        | TagEnd::BlockQuote(_)
                        | TagEnd::List(_)
                ) {
                    self.ended = true;
                }
                // Lines left before the end belong to the tag.
                self.upto(range.end);
                self.close(*tag, range);
            }
            Event::Rule => {
                self.before(range.start);
                self.block();
                self.content(range);
                self.ended = true;
            }
            _ => self.content(range),
        }
    }

    /// A block starts (the viewer's `start_block`): after another, with no
    /// blank line between them in the note, its first line gets a gap.
    fn block(&mut self) {
        if self.ended {
            self.gap_next = !self.blank_since && !self.layout.lines.is_empty();
            self.ended = false;
        }
    }

    /// The layout, every line laid out.
    pub fn finish(mut self) -> Layout {
        self.upto(self.text.len() + 1);
        self.layout
    }

    fn open(&mut self, tag: &Tag<'_>, at: usize) {
        match tag {
            Tag::BlockQuote(_) => self.containers.push(Container::Quote),
            Tag::List(first) => self.containers.push(Container::List(first.is_some())),
            Tag::Item => {
                let numbered = matches!(self.containers.last(), Some(Container::List(true)));
                self.containers.push(Container::Item {
                    // Dimmed, as the viewer's.
                    marker: (!numbered).then_some(("• ", Style::PLAIN.mods(Mods::DIM))),
                    number: at,
                    width: 2,
                    shown: false,
                });
            }
            Tag::Heading { level, .. } => {
                // Only a heading outside quotes and lists opens a section.
                if self.containers.is_empty() {
                    self.section = 2 * (*level as u16 - 1);
                    let line = self.text[..at].rfind('\n').map_or(0, |i| i + 1);
                    self.layout.headings.push((line, *level as u8));
                }
                self.hide_bare = true;
                self.heading = Some(false);
                self.containers.push(Container::Item {
                    marker: Some(heading_mark(*level)),
                    number: at,
                    width: 2,
                    shown: false,
                });
            }
            _ => {}
        }
    }

    fn close(&mut self, tag: TagEnd, range: &Range<usize>) {
        match tag {
            TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item => {
                self.containers.pop();
            }
            TagEnd::Heading(level) => {
                self.heading = None;
                self.containers.pop();
                self.hide_bare = false;
                if self.containers.is_empty() {
                    self.section = 2 * level as u16;
                }
            }
            TagEnd::TableCell => {
                // A missing column's cell: empty, at the row's very end
                // (a real empty cell, `||`, lies before it).
                let missing = range.is_empty() && range.start >= self.row_end;
                let row = self.table.as_mut().and_then(|t| t.rows.last_mut());
                if let Some((_, cells)) = row.filter(|_| !missing) {
                    cells.push(trimmed(self.text, range));
                }
            }
            TagEnd::Table => {
                if let Some(mut table) = self.table.take() {
                    table.lines.end = self.layout.lines.len();
                    self.layout.tables.push(table);
                }
            }
            TagEnd::CodeBlock => {
                if let Some(first) = self.code.take().filter(|&i| i != usize::MAX) {
                    let lines = &mut self.layout.lines[first..];
                    // The fences do not count, as the viewer has none.
                    let widest = lines
                        .iter()
                        .filter(|line| !line.skip)
                        .map(|line| code_width(self.text, line))
                        .max()
                        .unwrap_or(0);
                    let widest = u16::try_from(widest).unwrap_or(u16::MAX);
                    for line in lines {
                        line.code = Some(widest);
                    }
                }
                self.hide_bare = false;
            }
            _ => {}
        }
    }

    /// Content over `range`: its first line ends its markup there; any
    /// later line it starts (a code block's text) starts with content.
    fn content(&mut self, range: &Range<usize>) {
        self.before(range.start);
        if self.next <= range.start {
            self.line(range.start);
        }
        while self.next < range.end && self.next < self.text.len() {
            let start = self.next;
            self.line(start);
        }
    }

    /// Lays out, markup only, every line that ends before `at`'s line.
    fn before(&mut self, at: usize) {
        while self.next <= self.text.len() && self.next_end < at {
            self.bare();
        }
    }

    /// Lays out, markup only, every line that starts before `end`.
    fn upto(&mut self, end: usize) {
        while self.next < end && self.next <= self.text.len() {
            self.bare();
        }
    }

    /// Where the line starting at `start` ends (its line break, or the
    /// text's end).
    fn end_of(&self, start: usize) -> usize {
        self.text[start.min(self.text.len())..]
            .find('\n')
            .map_or(self.text.len(), |i| start + i)
    }

    /// Lays out the next line, which has no content.
    fn bare(&mut self) {
        let start = self.next;
        let end = self.next_end;
        let line = self.text[start..end].trim_end_matches('\r');
        let quoted = self
            .containers
            .iter()
            .any(|c| matches!(c, Container::Quote));
        // A heading's bare line (a Setext underline, an empty heading) is
        // markup whole; a fence only up to its backticks, so shown while
        // the cursor is in its block it sits in the gray box like code.
        let markup = if self.hide_bare && self.code.is_none() {
            line.len()
        } else {
            line.len()
                - line
                    .trim_start_matches(|c| c == ' ' || c == '\t' || (quoted && c == '>'))
                    .len()
        };
        let skip = self.hide_bare && (self.code.is_some() || self.heading == Some(true));
        if !skip {
            self.blank_since = line[markup..].trim().is_empty();
        }
        self.push(start, start + markup, false, skip);
    }

    /// Lays out the next line, whose content starts at `content`.
    fn line(&mut self, content: usize) {
        let start = self.next;
        // A numbered item's first line shows its number as written.
        let mut markup_end = content;
        for container in &mut self.containers {
            if let Container::Item {
                marker: None,
                number,
                width,
                shown: false,
            } = container
                && (start..content).contains(number)
            {
                markup_end = markup_end.min(*number);
                *width =
                    u16::try_from(display_width(&self.text[*number..content])).unwrap_or(u16::MAX);
            }
        }
        self.blank_since = false;
        self.push(start, markup_end, true, false);
    }

    /// Adds the line starting at `start` with markup up to `markup_end`;
    /// `content` marks its item markers as shown.
    fn push(&mut self, start: usize, markup_end: usize, content: bool, skip: bool) {
        if self.heading.is_some() {
            self.heading = Some(true);
        }
        self.prefix(content);
        let first = self.intern();
        self.prefix(false);
        let rest = self.intern();
        if self.code == Some(usize::MAX) {
            self.code = Some(self.layout.lines.len());
        }
        self.layout.lines.push(Line {
            markup: start..markup_end,
            first,
            rest,
            section: self.section,
            skip,
            gap: std::mem::take(&mut self.gap_next),
            code: None,
        });
        self.next = self.next_end + 1;
        self.next_end = self.end_of(self.next);
    }

    /// Builds the prefix of a row in `scratch`: on a line's first row
    /// (`first`), an item's marker not shown yet is shown, and then counts
    /// as shown.
    fn prefix(&mut self, first: bool) {
        let pieces = &mut self.scratch;
        pieces.clear();
        if self.section > 0 {
            pieces.push((spaces(self.section), Style::PLAIN));
        }
        for container in self.containers.iter_mut().take(MAX_LEVELS) {
            match container {
                Container::Quote => pieces.push(("│ ", QUOTE_BAR)),
                Container::List(_) => {}
                Container::Item {
                    marker,
                    width,
                    shown,
                    ..
                } => match marker {
                    Some(piece) if first && !*shown => {
                        pieces.push(*piece);
                        *shown = true;
                    }
                    // A number is drawn as written on its first line.
                    None if first && !*shown => *shown = true,
                    _ => pieces.push((spaces(*width), Style::PLAIN)),
                },
            }
        }
        if self.code.is_some() {
            pieces.push((" ", CODE));
        }
    }

    /// Stores `scratch`, sharing it with the last prefix when the same.
    fn intern(&mut self) -> Range<u32> {
        if self.layout.prefix(&self.last) == self.scratch.as_slice() {
            return self.last.clone();
        }
        let start = u32::try_from(self.layout.pieces.len()).unwrap_or(u32::MAX);
        self.layout.pieces.extend_from_slice(&self.scratch);
        let end = u32::try_from(self.layout.pieces.len()).unwrap_or(u32::MAX);
        self.last = start..end;
        start..end
    }
}

/// `range` without the spaces and tabs around its text (a table cell).
fn trimmed(text: &str, range: &Range<usize>) -> Range<usize> {
    let cell = &text[range.clone()];
    let start = range.start + (cell.len() - cell.trim_start_matches([' ', '\t']).len());
    let end = range.end - (cell.len() - cell.trim_end_matches([' ', '\t']).len());
    start..end.max(start)
}

/// `width` spaces (at most `SPACES`).
fn spaces(width: u16) -> &'static str {
    &SPACES[..usize::from(width).min(SPACES.len())]
}

/// A code line's width after its markup, a tab as `TAB` columns.
fn code_width(text: &str, line: &Line) -> usize {
    let end = text[line.markup.end..]
        .find('\n')
        .map_or(text.len(), |i| line.markup.end + i);
    // A fence line is markup only: no width.
    text[line.markup.end..end]
        .trim_end_matches('\r')
        .split('\t')
        .map(display_width)
        .sum::<usize>()
        + TAB * text[line.markup.end..end].matches('\t').count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::md::render::OPTIONS;
    use pulldown_cmark::Parser;

    fn layout(text: &str) -> Layout {
        let mut builder = Builder::new(text);
        for (event, range) in Parser::new_ext(text, OPTIONS).into_offset_iter() {
            builder.event(&event, &range);
        }
        builder.finish()
    }

    /// Each line as (first prefix, rest prefix, markup), the prefixes as
    /// their text.
    fn lines(text: &str) -> Vec<(String, String, String)> {
        let layout = layout(text);
        let joined = |range: &Range<u32>| -> String {
            layout.prefix(range).iter().map(|(s, _)| *s).collect()
        };
        layout
            .lines
            .iter()
            .map(|line| {
                (
                    joined(&line.first),
                    joined(&line.rest),
                    text[line.markup.clone()].to_owned(),
                )
            })
            .collect()
    }

    fn line(first: &str, rest: &str, markup: &str) -> (String, String, String) {
        (first.into(), rest.into(), markup.into())
    }

    #[test]
    fn sections_indent_by_heading_level() {
        let h1 = heading_mark(pulldown_cmark::HeadingLevel::H1).0;
        let h2 = heading_mark(pulldown_cmark::HeadingLevel::H2).0;
        assert_eq!(
            lines("# A\ntext\n\n## B\nmore"),
            [
                line(h1, "  ", "# "),
                line("  ", "  ", ""),
                line("  ", "  ", ""),
                line(&format!("  {h2}"), "    ", "## "),
                line("    ", "    ", ""),
            ]
        );
    }

    #[test]
    fn lists_nest_at_the_viewers_widths() {
        assert_eq!(
            lines("- a\n  - b\n    c\n1. one\n2. two"),
            [
                line("• ", "  ", "- "),
                line("  • ", "    ", "  - "),
                line("    ", "    ", "    "),
                // Numbers stay as written; wrapped rows hang under the text.
                line("", "   ", ""),
                line("", "   ", ""),
            ]
        );
    }

    #[test]
    fn quotes_bar_every_line_lazy_ones_too() {
        assert_eq!(
            lines("> a\nb\n> - c\n>\n> ## H"),
            [
                line("│ ", "│ ", "> "),
                line("│ ", "│ ", ""),
                line("│ • ", "│   ", "> - "),
                // Still inside the item (it ends at the heading): typing
                // here continues it.
                line("│   ", "│   ", ">"),
                line(
                    &format!("│ {}", heading_mark(pulldown_cmark::HeadingLevel::H2).0),
                    "│   ",
                    "> ## "
                ),
            ]
        );
    }

    #[test]
    fn code_blocks_are_boxed_and_fences_hidden() {
        let text = "- x\n\n  ```\n  code\n  longer line\n  ```\n";
        assert_eq!(
            lines(text),
            [
                line("• ", "  ", "- "),
                line("  ", "  ", ""),
                line("   ", "   ", "  "),
                line("   ", "   ", "  "),
                line("   ", "   ", "  "),
                line("   ", "   ", "  "),
                line("", "", ""),
            ]
        );
        let layout = layout(text);
        let boxes: Vec<_> = layout.lines.iter().map(|l| l.code).collect();
        // The fences have no row in the viewer (step LV1).
        let skips: Vec<_> = layout.lines.iter().map(|l| l.skip).collect();
        assert_eq!(skips, [false, false, true, false, false, true, false]);
        let w = Some(11);
        assert_eq!(boxes, [None, None, w, w, w, w, None]);
        // An indented block: its four spaces are markup.
        assert_eq!(
            lines("    a\n    b"),
            [line(" ", " ", "    "), line(" ", " ", "    ")]
        );
    }

    #[test]
    fn other_lines_keep_their_text() {
        // A link definition has no events: only its spaces are markup.
        // A table is drawn as written, its delimiter row too.
        assert_eq!(
            lines("  [a]: http://x\n\n| a |\n|---|"),
            [
                line("", "", "  "),
                line("", "", ""),
                line("", "", ""),
                line("", "", "")
            ]
        );
        // Every line once, in order.
        let text = "# H\n- a\n> b\n```\nc\n```\n\n    d\n";
        let layout = layout(text);
        assert_eq!(layout.lines.len(), text.split('\n').count());
    }

    /// Where the viewer adds a blank row the note does not have (step
    /// LV2): the first line of a block right after another; none where a
    /// blank line is, between items of a list, or before the first block.
    #[test]
    fn gaps_where_the_viewer_adds_a_blank_row() {
        let text = "## H\ntext:\n- a\n- b\n\npara\n```\nc\n```\nend";
        let gaps: Vec<bool> = layout(text).lines.iter().map(|l| l.gap).collect();
        // ## H, text:, - a, - b, blank, para, fence, c, fence, end.
        assert_eq!(
            gaps,
            [
                false, true, true, false, false, false, true, false, false, true
            ]
        );
    }

    proptest::proptest! {
        /// Any text: one line laid out per line of the note, in order,
        /// each markup inside its line and on char boundaries.
        #[test]
        fn every_line_once(text in "([*_`#\\[\\]()|>~!\\\\ a\n\t1.-]|\r\n|é|日){0,80}") {
            let layout = layout(&text);
            let starts: Vec<usize> = std::iter::once(0)
                .chain(text.match_indices('\n').map(|(i, _)| i + 1))
                .collect();
            proptest::prop_assert_eq!(layout.lines.len(), starts.len());
            for (line, start) in layout.lines.iter().zip(starts) {
                proptest::prop_assert_eq!(line.markup.start, start);
                let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
                proptest::prop_assert!(line.markup.end <= end);
                proptest::prop_assert!(text.is_char_boundary(line.markup.end));
            }
            // A table's rows are its lines, each cell inside its row's
            // line (step LT1).
            for table in &layout.tables {
                proptest::prop_assert!(table.lines.end <= layout.lines.len());
                for (row, cells) in &table.rows {
                    proptest::prop_assert!(table.lines.contains(row));
                    let start = layout.lines[*row].markup.start;
                    let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
                    for cell in cells {
                        proptest::prop_assert!(start <= cell.start && cell.end <= end);
                    }
                }
            }
        }
    }
}
