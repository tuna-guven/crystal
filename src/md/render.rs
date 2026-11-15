//! The renderer (docs/design/dependencies.md, "Markdown renderer"): a
//! note's text to wrapped, styled lines for the pager.
//!
//! - **Markdown** (every note but `.txt`): pulldown-cmark events, a small
//!   subset drawn as text with styles: headings (an icon and color per
//!   level, their sections indented, decision 0052), emphasis, strong,
//!   strikethrough, inline and block code, lists (nested, numbered, task
//!   items), block quotes, rules, tables boxed with aligned columns, links
//!   (clickable, decision 0054, when
//!   `md::link::clickable` accepts the target, else with their address
//!   dimmed after them), Obsidian `[[links]]`, images as `[image: alt]`, raw HTML
//!   dimmed and literal. A line break in the note stays a line break, as
//!   in Obsidian's reading view. Nothing is ever opened or followed here.
//! - **Line numbers:** each row records the number of the note line that
//!   starts on it, from the parser's source offsets (decision 0053),
//!   counting lines with text only (`model::lines`, decision 0055);
//!   wrapped rows, blanks and table borders have none.
//! - **Plain text** (`*.txt`, decision 0038):
//!   shown as written, wrapped, with no Markdown.
//!
//! Safety: the whole text is sanitized first (`md::safe`), then parsed.
//! Text the parser makes itself (entity and escape decoding: `&#27;` is
//! ESC) is sanitized again. Everything drawn comes from `SafeText` or from
//! fixed strings here, and the grid checks every cell once more.
//!
//! Memory: the sanitized text is one allocation; lines refer to it by byte
//! range and are kept flat (one run list, one line-start list), so a large
//! note costs little beyond its text. Nesting never recurses: containers
//! are a stack, and only the first `MAX_INDENT_LEVELS` levels indent.

use std::ops::Range;

use zeroize::Zeroizing;

use pulldown_cmark::{
    Alignment, CowStr, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd,
};

use super::link::clickable;
use super::safe::{SafeText, sanitize};
use crate::model::lines;
use crate::ui::grid::{Color, Grid, Mods, Style, display_width};
#[cfg(test)]
use crate::ui::layout::Rect;
use crate::ui::wrap::{Span, wrap_each};

/// How a note is shown, from its title (decision 0038).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Markdown,
    Plain,
}

/// A title ending in `.txt` (any case) is plain text; every other note is
/// Markdown.
pub fn kind_of(title: &str) -> Kind {
    let bytes = title.as_bytes();
    if bytes.len() > 4 && bytes[bytes.len() - 4..].eq_ignore_ascii_case(b".txt") {
        Kind::Plain
    } else {
        Kind::Markdown
    }
}

/// Containers deeper than this still render, without more indent.
const MAX_INDENT_LEVELS: usize = 8;
/// Indent strings are slices of this.
const SPACES: &str = "                ";

const PLAIN: Style = Style::PLAIN;
const DIM: Style = Style::PLAIN.mods(Mods::DIM);
const CODE: Style = Style::PLAIN.fg(Color::Yellow);
const QUOTE_BAR: Style = Style::PLAIN.fg(Color::DarkGray);
const LINK: Style = Style::PLAIN.fg(Color::Cyan).mods(Mods::UNDERLINED);

/// Where a run's text comes from.
#[derive(Clone)]
enum Text {
    /// A byte range of the sanitized note.
    Source(Range<usize>),
    /// A byte range of an extra sanitized piece (decoded by the parser).
    Extra(usize, Range<usize>),
    /// Fixed text from the renderer.
    Fixed(&'static str),
    /// A horizontal rule: `─` across the rest of the line.
    Rule,
}

#[derive(Clone)]
struct Run {
    text: Text,
    style: Style,
    /// The `[[link]]` this run belongs to (an index into `Page::links`).
    link: Option<u32>,
    /// The clickable link this run belongs to (an index into
    /// `Page::urls`).
    url: Option<u32>,
    /// Where in the sanitized note the run's text begins, for line
    /// numbers; `None` for what the renderer adds around the text
    /// (indents, markers, table borders and padding).
    at: Option<usize>,
}

/// An Obsidian `[[link]]` on the page: its target as written (`name`,
/// `name#Heading`, `name#^id`, `#Heading`) and the first line it is on.
struct PageLink {
    target: Text,
    line: Option<usize>,
}

/// An Obsidian block id: ` ^id` at the end of a paragraph or list item
/// (letters, digits and `-`). Returns where the marker starts (its space
/// included) and the id.
pub fn block_id(text: &str) -> Option<(usize, &str)> {
    let trimmed = text.trim_end();
    let caret = trimmed.rfind(" ^")?;
    let id = &trimmed[caret + 2..];
    let valid = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    valid.then(|| (trimmed[..caret].trim_end().len(), id))
}

/// `a` and `b` equal ignoring case, without lowercase copies.
fn eq_ci(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
}

/// A rendered note: lines of styled runs over sanitized text. Holds note
/// text: no Debug, Display, Clone or Serialize.
pub struct Page {
    source: SafeText,
    extra: Vec<SafeText>,
    runs: Vec<Run>,
    /// Where each line starts in `runs`.
    line_starts: Vec<usize>,
    links: Vec<PageLink>,
    /// The targets of clickable links, each `clickable`.
    urls: Vec<Text>,
    /// Heading text (sanitized) and its line.
    headings: Vec<(Zeroizing<String>, usize)>,
    /// Block ids and their first line.
    blocks: Vec<(Zeroizing<String>, usize)>,
    /// The sections headings outside quotes and lists open, in order.
    sections: Vec<Section>,
    /// Each line's number in the note (from 1, as `model::lines` counts), 0
    /// for a line holding no note text, with `CONTINUES` set on a line
    /// that wraps on from the one above.
    numbers: Vec<u32>,
}

/// Marks a line number as continued from the line above. A note holds at
/// most 8 MiB, so its line numbers never reach this bit.
const CONTINUES: u32 = 1 << 31;

/// A heading's section: the heading's line, its level (1 to 6), and the
/// line after its last one, where the next heading of the same or a
/// higher level starts (or the page ends). Folding hides `line + 1..end`.
#[derive(Clone, Copy)]
pub struct Section {
    pub line: usize,
    pub level: u8,
    pub end: usize,
}

crate::model::assert_not_impl!(Page: std::fmt::Debug, std::fmt::Display, Clone);

impl Page {
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    fn line(&self, index: usize) -> &[Run] {
        let start = self.line_starts[index];
        let end = self
            .line_starts
            .get(index + 1)
            .copied()
            .unwrap_or(self.runs.len());
        &self.runs[start..end]
    }

    fn text_of<'a>(&'a self, text: &'a Text) -> &'a str {
        match text {
            Text::Source(r) => &self.source.as_str()[r.clone()],
            Text::Extra(i, r) => &self.extra[*i].as_str()[r.clone()],
            Text::Fixed(s) => s,
            Text::Rule => "",
        }
    }

    /// The note line (from 1) that starts on line `index`, if one does:
    /// `None` for a wrapped line, a blank and a table border.
    pub fn number(&self, index: usize) -> Option<u32> {
        self.numbers
            .get(index)
            .copied()
            .filter(|&n| n > 0 && n & CONTINUES == 0)
    }

    /// The note line (from 1) line `index` shows: the one starting on it,
    /// or the one a wrapped line continues. `None` for a line holding no
    /// note text (a blank, a table border).
    pub fn note_line(&self, index: usize) -> Option<u32> {
        self.numbers
            .get(index)
            .map(|&n| n & !CONTINUES)
            .filter(|&n| n > 0)
    }

    /// How many `[[links]]` the page has.
    pub fn link_count(&self) -> usize {
        self.links.len()
    }

    /// Link `index`'s target as written (sanitized).
    pub fn link_target(&self, index: usize) -> &str {
        self.text_of(&self.links[index].target)
    }

    /// The first line link `index` is drawn on.
    pub fn link_line(&self, index: usize) -> Option<usize> {
        self.links[index].line
    }

    /// The line of the first heading with this text, ignoring case.
    pub fn heading_line(&self, heading: &str) -> Option<usize> {
        self.headings
            .iter()
            .find(|(text, _)| eq_ci(text, heading))
            .map(|(_, line)| *line)
    }

    /// The sections, in page order.
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// The line of the block with this id.
    pub fn block_line(&self, id: &str) -> Option<usize> {
        self.blocks
            .iter()
            .find(|(block, _)| block.as_str() == id)
            .map(|(_, line)| *line)
    }

    /// Draws line `index` at (x, y), at most `width` columns, with link
    /// `selected` (if any) reversed. Returns the columns used.
    pub fn draw_line(
        &self,
        grid: &mut Grid,
        (x, y): (u16, u16),
        width: u16,
        index: usize,
        selected: Option<usize>,
    ) -> u16 {
        let selected = selected.and_then(|s| u32::try_from(s).ok());
        let mut used = 0;
        for run in self.line(index) {
            let x = x.saturating_add(used);
            let room = width - used;
            let style = if run.link.is_some() && run.link == selected {
                run.style.mods(run.style.mods | Mods::REVERSED)
            } else {
                run.style
            };
            let columns = match run.text {
                Text::Rule => {
                    for i in 0..room {
                        grid.put_str(x + i, y, "─", style, 1);
                    }
                    room
                }
                ref text => grid.put_str(x, y, self.text_of(text), style, room),
            };
            // A clickable link's cells carry its target; the grid checks
            // it again.
            if let Some(url) = run.url.and_then(|u| self.urls.get(u as usize)) {
                let link = grid.add_link(self.text_of(url));
                grid.set_link(x, y, columns, link);
            }
            used += columns;
        }
        used
    }

    /// Draws the lines from `scroll` down into `area`.
    #[cfg(test)]
    pub fn draw(&self, grid: &mut Grid, area: Rect, scroll: usize) {
        for (y, index) in (area.y..area.bottom()).zip(scroll..self.line_count()) {
            self.draw_line(grid, (area.x, y), area.width, index, None);
        }
    }
}

/// Renders `text` as `kind` for `width` columns. The whole note is laid
/// out at once: an 8 MiB note of empty paragraphs is about 4 million lines
/// (about 140 MB of line data, numbers included), within the 512 MiB
/// memory budget. Accepted;
/// laying out only the visible part is the fix if it ever matters.
pub fn render(text: &str, kind: Kind, width: u16) -> Page {
    let source = sanitize(text);
    let mut builder = Builder::new(&source, width);
    match kind {
        Kind::Plain => {
            let all = 0..source.as_str().len();
            builder.content.push(Run {
                text: Text::Source(all),
                style: PLAIN,
                link: None,
                url: None,
                at: Some(0),
            });
            builder.flush();
        }
        Kind::Markdown => builder.markdown(),
    }
    let Builder {
        extra,
        runs,
        line_starts,
        page_links: links,
        urls,
        headings,
        blocks,
        mut sections,
        numbers,
        ..
    } = builder;
    // A section ends where the next heading of its level or higher starts.
    for i in 0..sections.len() {
        let level = sections[i].level;
        sections[i].end = sections[i + 1..]
            .iter()
            .find(|s| s.level <= level)
            .map_or(line_starts.len(), |s| s.line);
    }
    Page {
        source,
        extra,
        runs,
        line_starts,
        links,
        urls,
        headings,
        blocks,
        sections,
        numbers,
    }
}

/// A container a line can be inside.
enum Container {
    Quote,
    /// A list: the next item's number, or `None` for bullets.
    List(Option<u64>),
    /// A list item: its marker, shown on the item's first line only.
    Item {
        marker: Run,
        width: u16,
        shown: bool,
    },
}

/// Builds a page over the sanitized text it borrows.
struct Builder<'s> {
    source: &'s SafeText,
    extra: Vec<SafeText>,
    runs: Vec<Run>,
    line_starts: Vec<usize>,
    width: u16,
    containers: Vec<Container>,
    /// The inline content of the block being built.
    content: Vec<Run>,
    /// Inline styles in effect (emphasis, links, code block, heading).
    styles: Vec<Style>,
    /// Link addresses to show at the end of each open link.
    links: Vec<Option<Text>>,
    /// A blank line goes before the next block.
    blank: bool,
    /// Inside a code block: its text keeps its line breaks.
    in_code: bool,
    /// The `[[link]]` being read, if any.
    current_link: Option<u32>,
    /// The clickable link being read, if any (an index into `urls`).
    current_url: Option<u32>,
    urls: Vec<Text>,
    page_links: Vec<PageLink>,
    headings: Vec<(Zeroizing<String>, usize)>,
    blocks: Vec<(Zeroizing<String>, usize)>,
    /// The table being read: drawn whole at its end, once every column's
    /// width is known.
    table: Option<Table>,
    /// Columns every line is indented by for the section it is in, before
    /// any quote or list prefix.
    section_indent: u16,
    sections: Vec<Section>,
    /// The start of the parser event being read.
    at: usize,
    /// Each row's line number in the note (from 1), 0 for none.
    numbers: Vec<u32>,
    /// The last number given to a row.
    last_number: u32,
    /// Numbers the sanitized note's lines, which keep the note's line
    /// breaks.
    lines: lines::Counter,
}

/// A table's cells as runs (rows of cells), held until the table ends.
struct Table {
    aligns: Vec<Alignment>,
    rows: Vec<Vec<Vec<Run>>>,
    /// How many rows are the header (0 or 1).
    head: usize,
}

/// A table's borders are slices of this (3 bytes a character).
const DASHES: &str = "────────────────";

/// The fewest columns a narrowed table column keeps for its text; with
/// less, the table is drawn unboxed.
const MIN_CELL: usize = 3;

/// Column widths for a boxed table with `room` columns for cell text, from
/// each column's `natural` width (its widest cell). If all fit, each keeps
/// its width. Else the columns that fit their share of what is left keep
/// theirs, and the wider ones split the rest evenly (the widest getting the
/// odd columns). `None` if that leaves one of them fewer than `MIN_CELL`.
fn fit_columns(natural: &[usize], room: usize) -> Option<Vec<usize>> {
    if natural.iter().sum::<usize>() <= room {
        return Some(natural.to_vec());
    }
    let mut order: Vec<usize> = (0..natural.len()).collect();
    order.sort_by_key(|&c| natural[c]);
    let mut widths = natural.to_vec();
    let mut left = room;
    for (i, &c) in order.iter().enumerate() {
        let wide = &order[i..];
        let share = left / wide.len();
        if natural[c] <= share {
            left -= natural[c];
            continue;
        }
        // This column and every wider one are over their share: they
        // split what is left.
        if share < MIN_CELL {
            return None;
        }
        let odd = left % wide.len();
        for (k, &c) in wide.iter().enumerate() {
            widths[c] = share + usize::from(k >= wide.len() - odd);
        }
        break;
    }
    Some(widths)
}

impl<'s> Builder<'s> {
    fn new(source: &'s SafeText, width: u16) -> Self {
        Self {
            source,
            extra: Vec::new(),
            runs: Vec::new(),
            line_starts: Vec::new(),
            width,
            containers: Vec::new(),
            content: Vec::new(),
            styles: vec![PLAIN],
            links: Vec::new(),
            blank: false,
            in_code: false,
            current_link: None,
            current_url: None,
            urls: Vec::new(),
            page_links: Vec::new(),
            headings: Vec::new(),
            blocks: Vec::new(),
            table: None,
            section_indent: 0,
            sections: Vec::new(),
            at: 0,
            numbers: Vec::new(),
            last_number: 0,
            lines: lines::Counter::default(),
        }
    }

    fn text_of(&self, text: &Text) -> &str {
        match text {
            Text::Source(r) => &self.source.as_str()[r.clone()],
            Text::Extra(i, r) => &self.extra[*i].as_str()[r.clone()],
            Text::Fixed(s) => s,
            Text::Rule => "",
        }
    }

    /// The block's text from content run `from` on, for a heading.
    fn content_text(&self, from: usize) -> Zeroizing<String> {
        let parts: Vec<&str> = self.content[from..]
            .iter()
            .map(|r| self.text_of(&r.text))
            .collect();
        let mut text = Zeroizing::new(String::with_capacity(parts.iter().map(|p| p.len()).sum()));
        for part in parts {
            text.push_str(part);
        }
        text
    }

    /// A trailing ` ^id` on the block: hidden, and its line recorded.
    fn take_block_id(&mut self) {
        let Some(Run {
            text: Text::Source(range),
            ..
        }) = self.content.last()
        else {
            return;
        };
        let range = range.clone();
        let text = &self.source.as_str()[range.clone()];
        if let Some((cut, id)) = block_id(text) {
            self.blocks
                .push((Zeroizing::new(id.to_owned()), self.line_starts.len()));
            if let Some(Run {
                text: Text::Source(r),
                ..
            }) = self.content.last_mut()
            {
                r.end = range.start + cut;
            }
        }
    }

    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or(PLAIN)
    }

    fn push_style(&mut self, f: impl FnOnce(Style) -> Style) {
        let style = f(self.style());
        self.styles.push(style);
    }

    fn pop_style(&mut self) {
        if self.styles.len() > 1 {
            self.styles.pop();
        }
    }

    /// Event text as a run: a range of the sanitized source if the parser
    /// borrowed it from there, else sanitized again on its own.
    fn text(&mut self, text: &CowStr<'_>) -> Text {
        let source = self.source.as_str();
        let base = source.as_ptr() as usize;
        let at = text.as_ptr() as usize;
        if let CowStr::Borrowed(_) = text
            && at >= base
            && at + text.len() <= base + source.len()
        {
            let start = at - base;
            return Text::Source(start..start + text.len());
        }
        let safe = sanitize(text);
        let len = safe.as_str().len();
        self.extra.push(safe);
        Text::Extra(self.extra.len() - 1, 0..len)
    }

    /// Note text, or the renderer's text standing for it (a task box, an
    /// image's `[image: `): numbered by where it begins, or else by the
    /// event it comes from.
    fn add(&mut self, text: Text, style: Style) {
        let at = match &text {
            Text::Source(r) => r.start,
            _ => self.at,
        };
        self.add_at(text, style, Some(at));
    }

    /// Text the renderer adds around the note's (table borders, padding,
    /// separators): never numbered.
    fn decorate(&mut self, text: Text, style: Style) {
        self.add_at(text, style, None);
    }

    fn add_at(&mut self, text: Text, style: Style, at: Option<usize>) {
        let link = self.current_link;
        self.content.push(Run {
            text,
            style,
            link,
            url: self.current_url,
            at,
        });
    }

    fn markdown(&mut self) {
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_WIKILINKS;
        // The parser borrows the sanitized text for 's, apart from `self`.
        let source: &'s SafeText = self.source;
        for (event, range) in Parser::new_ext(source.as_str(), options).into_offset_iter() {
            self.at = range.start;
            self.event(event);
        }
        self.flush();
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                let run = self.text(&t);
                let style = self.style();
                self.add(run, style);
            }
            Event::Code(t) => {
                let run = self.text(&t);
                self.add(run, CODE);
            }
            Event::Html(t) | Event::InlineHtml(t) => {
                let run = self.text(&t);
                self.add(run, DIM);
            }
            Event::InlineMath(t) | Event::DisplayMath(t) | Event::FootnoteReference(t) => {
                let run = self.text(&t);
                let style = self.style();
                self.add(run, style);
            }
            // A line break in the note stays one on screen, as in
            // Obsidian's reading view, so every line of the note starts a
            // row and has its number.
            Event::SoftBreak => self.add(Text::Fixed("\n"), PLAIN),
            Event::HardBreak => self.add(Text::Fixed("\n"), PLAIN),
            Event::Rule => {
                self.start_block();
                self.add(Text::Rule, QUOTE_BAR);
                self.flush();
                self.blank = true;
            }
            Event::TaskListMarker(done) => {
                let mark = if done { "[x] " } else { "[ ] " };
                self.add(Text::Fixed(mark), DIM);
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph | Tag::HtmlBlock => self.start_block(),
            Tag::TableHead | Tag::TableRow => {
                if matches!(tag, Tag::TableHead) {
                    self.push_style(|s| s.mods(s.mods | Mods::BOLD));
                }
                if let Some(t) = self.table.as_mut() {
                    t.rows.push(Vec::new());
                }
            }
            Tag::Heading { level, .. } => {
                self.start_block();
                let (icon, color) = match level {
                    HeadingLevel::H1 => ("◉ ", Color::Yellow),
                    HeadingLevel::H2 => ("○ ", Color::Green),
                    HeadingLevel::H3 => ("● ", Color::Red),
                    HeadingLevel::H4 => ("✿ ", Color::Blue),
                    HeadingLevel::H5 => ("◆ ", Color::Magenta),
                    HeadingLevel::H6 => ("◇ ", Color::Cyan),
                };
                let style = PLAIN.fg(color).mods(Mods::BOLD);
                let level = level as u8;
                // Only a heading outside quotes and lists opens a section:
                // it is drawn two columns per level in, and what follows
                // it two more. The icon is a hanging marker, so a wrapped
                // heading lines up under its text.
                if self.containers.is_empty() {
                    self.section_indent = 2 * u16::from(level - 1);
                    self.sections.push(Section {
                        line: self.line_starts.len(),
                        level,
                        end: 0,
                    });
                }
                self.containers.push(Container::Item {
                    marker: Run {
                        text: Text::Fixed(icon),
                        style,
                        link: None,
                        url: None,
                        at: None,
                    },
                    width: 2,
                    shown: false,
                });
                self.push_style(|_| style);
            }
            Tag::BlockQuote(_) => {
                self.start_block();
                self.containers.push(Container::Quote);
            }
            Tag::CodeBlock(_) => {
                self.start_block();
                self.in_code = true;
                self.push_style(|_| CODE);
            }
            Tag::List(first) => {
                self.start_block();
                self.containers.push(Container::List(first));
            }
            Tag::Item => {
                self.flush();
                let marker = match self.containers.last_mut() {
                    Some(Container::List(Some(n))) => {
                        let number = *n;
                        *n = n.saturating_add(1);
                        let safe = sanitize(&format!("{number}. "));
                        let len = safe.as_str().len();
                        self.extra.push(safe);
                        Text::Extra(self.extra.len() - 1, 0..len)
                    }
                    _ => Text::Fixed("• "),
                };
                let width = match &marker {
                    Text::Extra(i, _) => display_width(self.extra[*i].as_str()),
                    _ => 2,
                };
                self.containers.push(Container::Item {
                    marker: Run {
                        text: marker,
                        style: DIM,
                        link: None,
                        url: None,
                        at: None,
                    },
                    width: u16::try_from(width).unwrap_or(2).min(16),
                    shown: false,
                });
            }
            Tag::Table(aligns) => {
                self.start_block();
                self.table = Some(Table {
                    aligns,
                    rows: Vec::new(),
                    head: 0,
                });
            }
            Tag::TableCell => {}
            Tag::Emphasis => self.push_style(|s| s.mods(s.mods | Mods::ITALIC)),
            Tag::Strong => self.push_style(|s| s.mods(s.mods | Mods::BOLD)),
            Tag::Strikethrough => self.push_style(|s| s.mods(s.mods | Mods::CROSSED_OUT)),
            Tag::Superscript | Tag::Subscript => self.push_style(|s| s),
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => {
                if let LinkType::WikiLink { .. } = link_type {
                    let target = self.text(&dest_url);
                    self.current_link = u32::try_from(self.page_links.len()).ok();
                    self.page_links.push(PageLink { target, line: None });
                }
                // A target that may be clickable is checked as the parser
                // decoded it, before sanitizing: an entity that decodes to
                // ESC (`&#27;`) is refused here. Its address is not shown.
                let wiki = matches!(link_type, LinkType::WikiLink { .. });
                if !wiki && clickable(&dest_url) {
                    let target = self.text(&dest_url);
                    self.current_url = u32::try_from(self.urls.len()).ok();
                    self.urls.push(target);
                }
                let shown = match link_type {
                    LinkType::Autolink | LinkType::Email | LinkType::WikiLink { .. } => None,
                    _ if dest_url.is_empty() || self.current_url.is_some() => None,
                    _ => Some(self.text(&dest_url)),
                };
                self.links.push(shown);
                self.push_style(|s| LINK.mods(s.mods | Mods::UNDERLINED));
            }
            Tag::Image { .. } => {
                self.add(Text::Fixed("[image: "), DIM);
                self.links.push(None);
                self.push_style(|_| DIM);
            }
            Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::MetadataBlock(_) => self.start_block(),
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.take_block_id();
                self.flush();
                self.blank = true;
            }
            TagEnd::HtmlBlock => {
                self.flush();
                self.blank = true;
            }
            TagEnd::Table => {
                self.draw_table();
                self.blank = true;
            }
            TagEnd::Heading(level) => {
                self.pop_style();
                let text = self.content_text(0);
                self.headings.push((text, self.line_starts.len()));
                self.flush();
                // The icon's marker.
                self.containers.pop();
                if self.containers.is_empty() {
                    self.section_indent = 2 * u16::from(level as u8);
                }
                self.blank = true;
            }
            TagEnd::CodeBlock => {
                // The block's last line break ends it; it is not a line.
                if let Some(Run {
                    text: Text::Source(r),
                    ..
                }) = self.content.last_mut()
                    && self.source.as_str()[r.clone()].ends_with('\n')
                {
                    r.end -= 1;
                }
                self.pop_style();
                self.flush();
                self.in_code = false;
                self.blank = true;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.containers.pop();
                self.blank = true;
            }
            TagEnd::List(_) => {
                self.flush();
                self.containers.pop();
                self.blank = true;
            }
            TagEnd::Item => {
                self.take_block_id();
                self.flush();
                self.containers.pop();
            }
            TagEnd::TableHead => {
                self.pop_style();
                if let Some(t) = self.table.as_mut() {
                    t.head = t.rows.len();
                }
            }
            TagEnd::TableRow => {}
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.content);
                if let Some(row) = self.table.as_mut().and_then(|t| t.rows.last_mut()) {
                    row.push(cell);
                }
            }
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript => self.pop_style(),
            TagEnd::Link => {
                self.current_link = None;
                self.current_url = None;
                self.pop_style();
                if let Some(Some(url)) = self.links.pop() {
                    self.add(Text::Fixed(" ("), DIM);
                    // A reference link's address is written elsewhere in
                    // the note: number it by the link.
                    self.add_at(url, DIM, Some(self.at));
                    self.add(Text::Fixed(")"), DIM);
                }
            }
            TagEnd::Image => {
                self.pop_style();
                self.links.pop();
                self.add(Text::Fixed("]"), DIM);
            }
            TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::MetadataBlock(_) => {
                self.flush();
                self.blank = true;
            }
        }
    }

    /// Draws the table read so far, boxed: every column as wide as its
    /// widest cell, cells padded by their column's alignment with one
    /// blank on each side, a border around the table and a rule between
    /// rows (`┌─┬─┐`, `│`, `├─┼─┤`, `└─┴─┘`). A table wider than the
    /// screen is narrowed by `fit_columns`: each cell wraps inside its
    /// column, a row is as tall as its tallest cell, and shorter cells are
    /// padded. Only if a column would keep fewer than `MIN_CELL` columns is
    /// the table drawn unpadded and unboxed, each row wrapping like a
    /// paragraph, `│` between cells, under a full-width rule after the
    /// header. Cells keep their runs (styles, `[[links]]`); padding and
    /// borders are fixed text, so no note text is copied.
    fn draw_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        let columns = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let mut natural = vec![0; columns];
        for row in &table.rows {
            for (width, cell) in natural.iter_mut().zip(row) {
                *width = (*width).max(self.cell_width(cell));
            }
        }
        let screen = match self.prefix_width() {
            w if w >= self.width => self.width,
            w => self.width - w,
        };
        // `│ ` before each cell, ` ` after it, and the closing `│`.
        let widths = usize::from(screen)
            .checked_sub(3 * columns + 1)
            .and_then(|room| fit_columns(&natural, room));
        let Some(widths) = widths else {
            self.draw_wrapped_table(table, columns);
            return;
        };
        let rows = table.rows.len();
        self.border(&widths, "┌─", "─┬─", "─┐");
        for (i, row) in table.rows.into_iter().enumerate() {
            // Each cell as lines of its column's width.
            let mut cells = row.into_iter();
            let mut lines: Vec<std::vec::IntoIter<Vec<Run>>> = Vec::with_capacity(columns);
            for &width in &widths {
                let cell = cells.next().unwrap_or_default();
                let mut wrapped = if cell.is_empty() {
                    Vec::new()
                } else if self.cell_width(&cell) <= width {
                    vec![cell]
                } else {
                    // At most the screen's width, so it fits a u16.
                    let room = u16::try_from(width).unwrap_or(u16::MAX);
                    self.wrap_runs(&cell, room)
                };
                for line in &mut wrapped {
                    self.trim_end(line);
                }
                lines.push(wrapped.into_iter());
            }
            let height = lines.iter().map(ExactSizeIterator::len).max().unwrap_or(0);
            for _ in 0..height.max(1) {
                for (c, &width) in widths.iter().enumerate() {
                    self.decorate(Text::Fixed(if c == 0 { "│ " } else { " │ " }), QUOTE_BAR);
                    let line = lines[c].next().unwrap_or_default();
                    let room = width.saturating_sub(self.cell_width(&line));
                    let before = match table.aligns.get(c) {
                        Some(Alignment::Right) => room,
                        Some(Alignment::Center) => room / 2,
                        _ => 0,
                    };
                    self.repeat(SPACES, before, PLAIN);
                    self.content.extend(line);
                    self.repeat(SPACES, room - before, PLAIN);
                }
                self.decorate(Text::Fixed(" │"), QUOTE_BAR);
                self.flush();
            }
            if i + 1 < rows {
                self.border(&widths, "├─", "─┼─", "─┤");
            }
        }
        self.border(&widths, "└─", "─┴─", "─┘");
    }

    /// One border line of a boxed table: `left`, then each column's
    /// dashes, `mid` between columns, and `right`.
    fn border(
        &mut self,
        widths: &[usize],
        left: &'static str,
        mid: &'static str,
        right: &'static str,
    ) {
        for (c, &width) in widths.iter().enumerate() {
            self.decorate(Text::Fixed(if c == 0 { left } else { mid }), QUOTE_BAR);
            self.repeat(DASHES, width, QUOTE_BAR);
        }
        self.decorate(Text::Fixed(right), QUOTE_BAR);
        self.flush();
    }

    /// A table wider than the screen: rows wrap like paragraphs, cells
    /// parted by `│`, a full-width rule under the header.
    fn draw_wrapped_table(&mut self, table: Table, columns: usize) {
        for (i, row) in table.rows.into_iter().enumerate() {
            if i == table.head && i > 0 {
                self.decorate(Text::Rule, QUOTE_BAR);
                self.flush();
            }
            // A short row still gets its `│`s, as in a boxed table.
            let mut cells = row.into_iter();
            for c in 0..columns {
                if c > 0 {
                    self.decorate(Text::Fixed(" │ "), QUOTE_BAR);
                }
                self.content.extend(cells.next().unwrap_or_default());
            }
            self.flush();
        }
    }

    /// The columns `cell` takes on screen.
    fn cell_width(&self, cell: &[Run]) -> usize {
        cell.iter()
            .map(|run| display_width(self.text_of(&run.text)))
            .sum()
    }

    /// `count` characters of `fill` (one-character-wide, all the same).
    fn repeat(&mut self, fill: &'static str, mut count: usize, style: Style) {
        let char_len = fill.len() / fill.chars().count();
        let most = fill.chars().count();
        while count > 0 {
            let n = count.min(most);
            self.decorate(Text::Fixed(&fill[..n * char_len]), style);
            count -= n;
        }
    }

    /// Before a new block: finish the one in progress, and leave a blank
    /// line after the previous block if it asked for one.
    fn start_block(&mut self) {
        self.flush();
        if self.blank && !self.line_starts.is_empty() {
            self.new_row(None);
        }
        self.blank = false;
    }

    /// The prefix of the next line (quote bars, list markers or their
    /// indent), and its width. Marks item markers as shown.
    fn prefix(&mut self) -> (Vec<Run>, u16) {
        let mut runs = Vec::new();
        let mut width = self.section_indent;
        if width > 0 {
            runs.push(Run {
                text: Text::Fixed(&SPACES[..usize::from(width)]),
                style: PLAIN,
                link: None,
                url: None,
                at: None,
            });
        }
        for container in self.containers.iter_mut().take(MAX_INDENT_LEVELS) {
            match container {
                Container::Quote => {
                    runs.push(Run {
                        text: Text::Fixed("│ "),
                        style: QUOTE_BAR,
                        link: None,
                        url: None,
                        at: None,
                    });
                    width += 2;
                }
                Container::List(_) => {}
                Container::Item {
                    marker,
                    width: w,
                    shown,
                } => {
                    if *shown {
                        runs.push(Run {
                            text: Text::Fixed(&SPACES[..usize::from(*w)]),
                            style: PLAIN,
                            link: None,
                            url: None,
                            at: None,
                        });
                    } else {
                        runs.push(marker.clone());
                        *shown = true;
                    }
                    width += *w;
                }
            }
        }
        (runs, width)
    }

    /// Wraps the block's content into lines.
    fn flush(&mut self) {
        if self.content.is_empty() {
            return;
        }
        let content = std::mem::take(&mut self.content);
        // A rule is one line, whatever the width.
        let lines = if matches!(
            content.first(),
            Some(Run {
                text: Text::Rule,
                ..
            })
        ) {
            vec![content]
        } else {
            let room = match self.prefix_width() {
                // No room for text beside the indent: no indent.
                w if w >= self.width => self.width.max(1),
                w => self.width - w,
            };
            self.wrap_runs(&content, room)
        };
        let indent = self.prefix_width() < self.width;
        for line in lines {
            let (prefix, _) = self.prefix();
            let number = self.line_starts.len();
            for run in &line {
                if let Some(link) = run.link.and_then(|l| self.page_links.get_mut(l as usize)) {
                    link.line.get_or_insert(number);
                }
            }
            let at = line.iter().find_map(|r| r.at);
            self.new_row(at);
            if indent {
                self.runs.extend(prefix);
            }
            self.runs.extend(line);
        }
    }

    /// `content` wrapped to `room` columns: lines of runs, each a piece
    /// of a run of `content` with its style, link and number.
    fn wrap_runs(&self, content: &[Run], room: u16) -> Vec<Vec<Run>> {
        // Spans for wrap, each with where its text comes from.
        let mut spans: Vec<(usize, Span<'_>)> = Vec::new();
        let mut origin: Vec<&Run> = Vec::new();
        for run in content {
            let before = spans.len();
            match &run.text {
                Text::Source(r) => self.source.spans_in(r.clone(), run.style, &mut spans),
                Text::Extra(i, r) => self.extra[*i].spans_in(r.clone(), run.style, &mut spans),
                Text::Fixed(s) => spans.push((
                    0,
                    Span {
                        text: s,
                        style: run.style,
                    },
                )),
                Text::Rule => spans.push((
                    0,
                    Span {
                        text: "",
                        style: run.style,
                    },
                )),
            }
            origin.extend(std::iter::repeat_n(run, spans.len() - before));
        }
        let only: Vec<Span<'_>> = spans.iter().map(|(_, s)| *s).collect();
        let mut lines: Vec<Vec<Run>> = Vec::new();
        wrap_each(&only, room, |line| {
            let runs = line
                .iter()
                .map(|piece| {
                    let (offset, _) = spans[piece.span];
                    let run = origin[piece.span];
                    let text = match &run.text {
                        Text::Source(_) => {
                            Text::Source(offset + piece.range.start..offset + piece.range.end)
                        }
                        Text::Extra(i, _) => {
                            Text::Extra(*i, offset + piece.range.start..offset + piece.range.end)
                        }
                        Text::Fixed(s) => Text::Fixed(&s[piece.range.clone()]),
                        Text::Rule => Text::Rule,
                    };
                    // Note text numbered by where it is: by where this
                    // piece of it is. Else (a reference link's address)
                    // as the whole run.
                    let at = match &run.text {
                        Text::Source(r) if run.at == Some(r.start) => {
                            Some(offset + piece.range.start)
                        }
                        _ => run.at,
                    };
                    Run {
                        text,
                        style: only[piece.span].style,
                        link: run.link,
                        url: run.url,
                        at,
                    }
                })
                .collect();
            lines.push(runs);
        });
        lines
    }

    /// Drops the spaces at the end of `line`: wrap lets the spaces it
    /// breaks at hang past the width, which would push a cell's border.
    fn trim_end(&self, line: &mut Vec<Run>) {
        while let Some(run) = line.last_mut() {
            let text = self.text_of(&run.text);
            let kept = text.trim_end_matches(' ').len();
            if kept == text.len() {
                return;
            }
            match &mut run.text {
                Text::Source(r) | Text::Extra(_, r) => r.end = r.start + kept,
                Text::Fixed(s) => *s = &s[..kept],
                Text::Rule => return,
            }
            if kept > 0 {
                return;
            }
            line.pop();
        }
    }

    /// Starts a row at the end of the runs. `at` is where its first note
    /// text begins: the row is numbered with that text's line if no
    /// earlier row was; a row going on with the last numbered line is
    /// marked `CONTINUES`; a row with no note text (a blank, a border),
    /// on a blank line (spaces in code), or going back to an earlier line,
    /// has none.
    fn new_row(&mut self, at: Option<usize>) {
        let number = match at.and_then(|at| self.lines.number(self.source.as_str(), at)) {
            Some(line) if line > self.last_number => {
                self.last_number = line;
                line
            }
            Some(line) if line == self.last_number => line | CONTINUES,
            _ => 0,
        };
        self.line_starts.push(self.runs.len());
        self.numbers.push(number);
    }

    /// The prefix width, without marking markers as shown.
    fn prefix_width(&self) -> u16 {
        self.section_indent
            + self
                .containers
                .iter()
                .take(MAX_INDENT_LEVELS)
                .map(|c| match c {
                    Container::Quote => 2,
                    Container::List(_) => 0,
                    Container::Item { width, .. } => *width,
                })
                .sum::<u16>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::grid::is_forbidden;

    /// The rendered rows as text, trailing spaces trimmed.
    fn rows(source: &str, kind: Kind, width: u16) -> Vec<String> {
        let page = render(source, kind, width);
        let height = u16::try_from(page.line_count()).unwrap();
        let mut grid = Grid::new(width, height);
        let area = grid.area();
        page.draw(&mut grid, area, 0);
        (0..height)
            .map(|y| {
                let line: String = (0..width)
                    .map(|x| grid.cell(x, y).unwrap().symbol())
                    .collect();
                line.trim_end().to_owned()
            })
            .collect()
    }

    fn md(source: &str, width: u16) -> Vec<String> {
        rows(source, Kind::Markdown, width)
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_of("todo.txt"), Kind::Plain);
        assert_eq!(kind_of("Notes.TXT"), Kind::Plain);
        assert_eq!(kind_of(".txt"), Kind::Markdown);
        assert_eq!(kind_of("v1.2 plan"), Kind::Markdown);
        assert_eq!(kind_of("roadmap"), Kind::Markdown);
        assert_eq!(kind_of("日本.txt"), Kind::Plain);
    }

    #[test]
    fn paragraphs_wrap_with_a_blank_line_between() {
        // A line break in the note stays one.
        assert_eq!(
            md("one two three\nfour\n\nnext one", 10),
            ["one two", "three", "four", "", "next one"]
        );
    }

    #[test]
    fn headings_and_emphasis() {
        assert_eq!(
            md("# Title\n\nSome *it* and **bold**.\n\n## Sub", 30),
            ["◉ Title", "", "  Some it and bold.", "", "  ○ Sub"]
        );
        let page = render("**b** *i* ~~s~~ `c`", Kind::Markdown, 20);
        let styles: Vec<Style> = page.line(0).iter().map(|r| r.style).collect();
        assert!(styles.iter().any(|s| s.mods.contains(Mods::BOLD)));
        assert!(styles.iter().any(|s| s.mods.contains(Mods::ITALIC)));
        assert!(styles.iter().any(|s| s.mods.contains(Mods::CROSSED_OUT)));
        assert!(styles.contains(&CODE));
    }

    #[test]
    fn lists_nest_and_number() {
        assert_eq!(
            md("- one\n- two\n  - deep\n- three", 20),
            ["• one", "• two", "  • deep", "• three"]
        );
        assert_eq!(md("3. c\n4. d", 20), ["3. c", "4. d"]);
        assert_eq!(
            md("- [ ] todo\n- [x] done", 20),
            ["• [ ] todo", "• [x] done"]
        );
        // A long item wraps under its own text, not under the bullet.
        assert_eq!(md("- aaa bbb ccc", 9), ["• aaa bbb", "  ccc"]);
    }

    #[test]
    fn quotes_code_and_rules() {
        assert_eq!(md("> quoted\n> text", 20), ["│ quoted", "│ text"]);
        assert_eq!(md("> a\n>> b", 20), ["│ a", "", "│ │ b"]);
        assert_eq!(
            md("```\nfn x() {\n    y\n}\n```", 20),
            ["fn x() {", "    y", "}"]
        );
        assert_eq!(md("a\n\n---\n\nb", 5), ["a", "", "─────", "", "b"]);
    }

    #[test]
    fn links_images_html_tables() {
        assert_eq!(
            md("see [site](https://x.org) and <https://y.org>", 60),
            ["see site and https://y.org"]
        );
        assert_eq!(md("[[Roadmap]] and [[work/a|A]]", 40), ["Roadmap and A"]);
        assert_eq!(md("![a cat](cat.png)", 40), ["[image: a cat]"]);
        assert_eq!(md("<b>raw</b>", 40), ["<b>raw</b>"]);
        assert_eq!(
            md("| a | b |\n|---|---|\n| 1 | 2 |", 40),
            [
                "┌───┬───┐",
                "│ a │ b │",
                "├───┼───┤",
                "│ 1 │ 2 │",
                "└───┴───┘"
            ]
        );
    }

    /// Columns line up whatever the cells' lengths, by display width (a
    /// wide character, a spelled-out escape), with the header's alignment;
    /// a rule parts every two rows, and the box closes every row.
    #[test]
    fn tables_line_up() {
        let table = "| Name | Qty | Note |\n|:--|--:|:-:|\n| apple | 3 | ok |\n| 日本 | 120 | x |";
        assert_eq!(
            md(table, 60),
            [
                "┌───────┬─────┬──────┐",
                "│ Name  │ Qty │ Note │",
                "├───────┼─────┼──────┤",
                "│ apple │   3 │  ok  │",
                "├───────┼─────┼──────┤",
                "│ 日本  │ 120 │  x   │",
                "└───────┴─────┴──────┘"
            ]
        );
        assert_eq!(
            md("| a | b |\n|---|---|\n| x\u{1b}y | 2 |", 40),
            [
                "┌────────────┬───┐",
                "│ a          │ b │",
                "├────────────┼───┤",
                "│ x<U+001B>y │ 2 │",
                "└────────────┴───┘"
            ]
        );
        // A short row gets empty cells; the header is bold, the border
        // is not. Line 1 is the header: `│ `, then its first cell.
        let page = render("| a | b |\n|---|---|\n| 1 |", Kind::Markdown, 40);
        assert!(!page.line(1)[0].style.mods.contains(Mods::BOLD));
        assert!(page.line(1)[1].style.mods.contains(Mods::BOLD));
        assert_eq!(
            md("| a | b |\n|---|---|\n| 1 |", 40),
            [
                "┌───┬───┐",
                "│ a │ b │",
                "├───┼───┤",
                "│ 1 │   │",
                "└───┴───┘"
            ]
        );
    }

    /// A `[[link]]` in a cell is found on its row (under the top border
    /// and the header's rule), and a table that cannot keep `MIN_CELL`
    /// columns a cell is unboxed, its rows wrapping instead of being cut.
    #[test]
    fn table_links_and_wide_rows() {
        let page = render("| a | b |\n|---|---|\n| [[plan]] | x |", Kind::Markdown, 40);
        assert_eq!(page.link_target(0), "plan");
        assert_eq!(page.link_line(0), Some(3));
        // `│ a │ b │` is 9 columns: boxed at 9, unboxed at 8.
        let table = "| a | b |\n|---|---|\n| 1 | 2 |";
        assert_eq!(md(table, 9)[0], "┌───┬───┐");
        assert_eq!(md(table, 8), ["a │ b", "────────", "1 │ 2"]);
        let rows = md("| aaaa bbbb | cccc |\n|---|---|\n| 1 | 2 |", 8);
        assert_eq!(rows, ["aaaa", "bbbb │", "cccc", "────────", "1 │ 2"]);
        // Unboxed, a cell longer than the screen pads nothing.
        let long = format!("| {} | b |\n|---|---|\n| 1 | 2 |", "x".repeat(100));
        let rows = md(&long, 8);
        assert_eq!(rows[rows.len() - 2..], ["─".repeat(8), "1 │ 2".to_owned()]);
        assert!(rows.iter().all(|r| !r.is_empty()), "{rows:?}");
        // Boxed, it wraps inside its column.
        let rows = md(&long, 40);
        assert_eq!(rows[1], format!("│ {} │ b │", "x".repeat(32)));
        assert_eq!(rows[4], format!("│ {:<32} │   │", "x".repeat(4)));
        assert_eq!(rows[6], format!("│ 1{} │ 2 │", " ".repeat(31)));
    }

    /// A table wider than the screen stays boxed: the narrow column keeps
    /// its width, the wide one wraps at its spaces inside the box, and the
    /// other cells of the row are padded down to its height. Only the
    /// first row of each table row is numbered.
    #[test]
    fn narrow_tables_wrap_inside_the_box() {
        let table = "| Name | Note |\n|---|--:|\n| apple | red and round fruit |\n| fig | ok |";
        assert_eq!(
            md(table, 21),
            [
                "┌───────┬───────────┐",
                "│ Name  │      Note │",
                "├───────┼───────────┤",
                "│ apple │   red and │",
                "│       │     round │",
                "│       │     fruit │",
                "├───────┼───────────┤",
                "│ fig   │        ok │",
                "└───────┴───────────┘"
            ]
        );
        assert_eq!(
            numbers(table, Kind::Markdown, 21),
            [0, 1, 0, 3, 0, 0, 0, 4, 0]
        );
        // Every row fits, at every width down to the unboxed fallback.
        for width in 1..60 {
            assert_fits(&render(table, Kind::Markdown, width), width).unwrap();
        }
    }

    /// Columns that fit their share keep their width; the wider ones split
    /// the rest, the widest taking the odd column; under `MIN_CELL`, none.
    #[test]
    fn columns_share_the_room() {
        assert_eq!(fit_columns(&[3, 4], 10), Some(vec![3, 4]));
        assert_eq!(fit_columns(&[5, 19], 14), Some(vec![5, 9]));
        assert_eq!(fit_columns(&[2, 10, 20], 13), Some(vec![2, 5, 6]));
        assert_eq!(fit_columns(&[10, 10], 7), Some(vec![3, 4]));
        assert_eq!(fit_columns(&[10, 10], 5), None);
        assert_eq!(fit_columns(&[1, 1], 1), None);
    }

    /// Each level has its icon and color; a heading is indented two
    /// columns per level below the first, what follows it two more, and a
    /// wrapped heading lines up under its text. Quotes and lists keep
    /// their prefixes inside the section's indent; a heading inside them
    /// gets its icon but moves nothing.
    #[test]
    fn headings_indent_their_sections() {
        let source = "intro\n\n# A\n\na\n\n## B\n\n### C\n\n#### D\n\n##### E\n\n###### F\n\nf\n\n# G\n\n> q\n\n- i\n\n> ## H\n\ng";
        assert_eq!(
            md(source, 40),
            [
                "intro",
                "",
                "◉ A",
                "",
                "  a",
                "",
                "  ○ B",
                "",
                "    ● C",
                "",
                "      ✿ D",
                "",
                "        ◆ E",
                "",
                "          ◇ F",
                "",
                "            f",
                "",
                "◉ G",
                "",
                "  │ q",
                "",
                "  • i",
                "",
                "  │ ○ H",
                "",
                "  g"
            ]
        );
        let page = render("# a\n\n## b", Kind::Markdown, 40);
        let colors: Vec<Color> = (0..page.line_count())
            .filter_map(|l| {
                page.line(l)
                    .iter()
                    .map(|r| r.style.fg)
                    .find(|&c| c != Color::Default)
            })
            .collect();
        assert_eq!(colors, [Color::Yellow, Color::Green]);
        assert_eq!(
            md("## long heading text", 12),
            ["  ○ long", "    heading", "    text"]
        );
    }

    /// A section ends where the next heading of its level or higher
    /// starts, or at the end of the page; headings in quotes open none.
    #[test]
    fn sections_end_at_the_next_peer() {
        let page = render(
            "# A\n\n## B\n\nb\n\n### C\n\n## D\n\n> # Q\n\n# E\n\ne",
            Kind::Markdown,
            40,
        );
        let got: Vec<(usize, u8, usize)> = page
            .sections()
            .iter()
            .map(|s| (s.line, s.level, s.end))
            .collect();
        // Lines: A 0, B 2, b 4, C 6, D 8, Q 10, E 12, e 14.
        assert_eq!(
            got,
            [(0, 1, 12), (2, 2, 8), (6, 3, 8), (8, 2, 12), (12, 1, 15)]
        );
    }

    #[test]
    fn links_headings_and_blocks_are_located() {
        let source = "# Top\n\nfirst [[a]] then\n[[b#Part|shown]]\n\n## Part\n\nA block. ^blk-1\n\n- item ^x";
        let page = render(source, Kind::Markdown, 12);
        assert_eq!(page.link_count(), 2);
        assert_eq!(page.link_target(0), "a");
        assert_eq!(page.link_target(1), "b#Part");
        let lines = md(source, 12);
        let line_of = |text: &str| lines.iter().position(|l| l.contains(text));
        assert!(
            lines[page.link_line(0).unwrap()].contains("first a"),
            "{lines:?}"
        );
        assert!(
            lines[page.link_line(1).unwrap()].contains("shown"),
            "{lines:?}"
        );
        assert_eq!(page.heading_line("part"), line_of("○ Part"));
        assert_eq!(page.heading_line("TOP"), Some(0));
        assert_eq!(page.heading_line("nope"), None);
        assert_eq!(page.block_line("blk-1"), line_of("A block."));
        assert_eq!(page.block_line("x"), line_of("• item"));
        assert!(
            !lines.iter().any(|l| l.contains('^')),
            "block ids are hidden: {lines:?}"
        );
        // The selected link is drawn reversed; the other is not.
        let mut grid = Grid::new(12, u16::try_from(page.line_count()).unwrap());
        let row = u16::try_from(page.link_line(1).unwrap()).unwrap();
        page.draw_line(&mut grid, (0, row), 12, usize::from(row), Some(1));
        let reversed: Vec<bool> = (0..12)
            .map(|x| {
                grid.cell(x, row)
                    .unwrap()
                    .style()
                    .mods
                    .contains(Mods::REVERSED)
            })
            .collect();
        assert!(reversed.iter().any(|&r| r), "{reversed:?}");
        let first = u16::try_from(page.link_line(0).unwrap()).unwrap();
        assert!((0..12).all(|x| {
            !grid
                .cell(x, first)
                .unwrap()
                .style()
                .mods
                .contains(Mods::REVERSED)
        }));
    }

    #[test]
    fn block_ids() {
        assert_eq!(block_id("text ^abc"), Some((4, "abc")));
        assert_eq!(block_id("text  ^a-1  "), Some((4, "a-1")));
        assert_eq!(block_id("text^abc"), None);
        assert_eq!(block_id("text ^"), None);
        assert_eq!(block_id("text ^a b"), None);
    }

    #[test]
    fn plain_text_is_shown_as_written() {
        assert_eq!(
            rows("# not a heading\n- not a list\n\n**x**", Kind::Plain, 40),
            ["# not a heading", "- not a list", "", "**x**"]
        );
        assert_eq!(rows("tab\there", Kind::Plain, 40), ["tab    here"]);
    }

    // Escape injection: ESC, CSI, OSC 52 and BiDi in every context are
    // spelled out, and never reach a cell; entities that decode to a
    // control character are caught too.
    #[test]
    fn hostile_text_is_inert_everywhere() {
        let hostile = "a\u{1b}[2J\u{9b}31m\u{1b}]52;c;aGk=\u{7}\u{202E}z";
        let sources = [
            hostile.to_owned(),
            format!("# {hostile}"),
            format!("`{hostile}`"),
            format!("```\n{hostile}\n```"),
            format!("[{hostile}](http://{hostile})"),
            format!("[[{hostile}]]"),
            format!("- {hostile}"),
            format!("> {hostile}"),
            format!("| {hostile} |\n|---|"),
            "&#27;[2J &#x9b; &#8238;".to_owned(),
        ];
        for source in &sources {
            for kind in [Kind::Markdown, Kind::Plain] {
                let shown = rows(source, kind, 30).join("\n");
                assert!(
                    !shown.contains(|c| c != '\n' && is_forbidden(c)),
                    "{source:?}: {shown:?}"
                );
            }
        }
        assert!(md(hostile, 80)[0].contains("<U+001B>[2J"));
        assert!(md("&#27;x", 80)[0].contains("<U+001B>x"));
    }

    /// A clickable link's cells, on every row it wraps to and through
    /// its emphasis, carry its target and show no address; other text
    /// and a refused target carry none, and the refused one keeps its
    /// address.
    #[test]
    fn clickable_links_mark_their_cells() {
        let source = "go [to **the** site](https://e.org/x) now [f](file:///x)";
        let page = render(source, Kind::Markdown, 12);
        let height = u16::try_from(page.line_count()).unwrap();
        let mut grid = Grid::new(12, height);
        let area = grid.area();
        page.draw(&mut grid, area, 0);
        let shown: Vec<String> = (0..height)
            .map(|y| {
                (0..12)
                    .map(|x| grid.cell(x, y).unwrap().symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect();
        assert_eq!(shown, ["go to the", "site now f", "(file:///x)"]);
        let target = |x, y| grid.link_target(grid.cell(x, y).unwrap().link());
        // "to the" on row 0 and "site" on row 1, the bold "the" too.
        for (x, y) in [(3, 0), (4, 0), (6, 0), (8, 0), (0, 1), (3, 1)] {
            assert_eq!(target(x, y), Some("https://e.org/x"), "({x}, {y})");
        }
        for (x, y) in [(0, 0), (2, 0), (5, 1), (9, 1), (0, 2)] {
            assert_eq!(target(x, y), None, "({x}, {y})");
        }
        // An entity that decodes to ESC makes the target refused.
        assert_eq!(
            md("[x](https://e.org/&#27;)", 40),
            ["x (https://e.org/<U+001B>)"]
        );
    }

    /// Phase 3 acceptance: every file in tests/corpus/escape renders as
    /// inert text, in every context, and as its golden output.
    #[test]
    fn escape_corpus() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/escape");
        #[expect(
            clippy::disallowed_methods,
            reason = "test switch to rewrite golden files"
        )]
        let update = std::env::var_os("UPDATE_GOLDEN").is_some();
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            if !name.ends_with(".md") || name == "README.md" {
                continue;
            }
            let source = String::from_utf8(std::fs::read(&path).unwrap()).unwrap();
            for kind in [Kind::Markdown, Kind::Plain] {
                let shown = rows(&source, kind, 60).join("\n");
                assert!(
                    !shown.contains(|c| c != '\n' && is_forbidden(c)),
                    "{name} {kind:?}: {shown:?}"
                );
            }
            // Every clickable link holds a target `clickable` accepts.
            let page = render(&source, Kind::Markdown, 60);
            let height = u16::try_from(page.line_count()).unwrap();
            let mut grid = Grid::new(60, height);
            let area = grid.area();
            page.draw(&mut grid, area, 0);
            for (x, y) in (0..height).flat_map(|y| (0..60).map(move |x| (x, y))) {
                let link = grid.cell(x, y).unwrap().link();
                if let Some(target) = grid.link_target(link) {
                    assert!(clickable(target), "{name}: {target:?}");
                }
            }
            let shown = rows(&source, Kind::Markdown, 60).join("\n") + "\n";
            let golden = path.with_extension("golden");
            if update {
                std::fs::write(&golden, &shown).unwrap();
            }
            let want = std::fs::read_to_string(&golden)
                .unwrap_or_else(|_| panic!("{name}: no golden file; run with UPDATE_GOLDEN=1"));
            assert_eq!(shown, want, "{name} differs from its golden file");
            checked += 1;
        }
        assert!(checked >= 11, "only {checked} corpus files");
    }

    // Deep nesting neither overflows the stack nor indents without end.
    #[test]
    fn deep_nesting_is_bounded() {
        let started = std::time::Instant::now();
        let quotes = format!("{}deep", ">".repeat(10_000));
        let page = render(&quotes, Kind::Markdown, 40);
        assert!(page.line_count() >= 1);
        let lists: String = (0..2_000)
            .map(|i| format!("{}- x\n", "  ".repeat(i)))
            .collect();
        let page = render(&lists, Kind::Markdown, 40);
        assert!(page.line_count() >= 1);
        assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
        let shown = md(&quotes, 40);
        assert!(shown.iter().all(|l| l.chars().count() <= 40));
    }

    // A large note renders in reasonable time and memory: one megabyte of
    // short lines, as Markdown and as plain text.
    #[test]
    fn large_notes() {
        let big = "word ".repeat(200_000);
        let started = std::time::Instant::now();
        for kind in [Kind::Markdown, Kind::Plain] {
            let page = render(&big, kind, 80);
            assert!(page.line_count() > 10_000);
        }
        assert!(started.elapsed().as_secs() < 20, "{:?}", started.elapsed());
    }

    /// Every line's width, trailing spaces left out (they hang, and the
    /// grid clips them), is at most the page's.
    fn assert_fits(page: &Page, width: u16) -> Result<(), String> {
        for line in 0..page.line_count() {
            // Measured as drawn: each run on its own, as `draw_selected`
            // hands it to the grid. Joining runs first would measure
            // graphemes that span two runs (`*` and U+FE0F, which the
            // parser splits) as one, which is not what is drawn.
            let runs: Vec<&str> = page
                .line(line)
                .iter()
                .filter(|r| !matches!(r.text, Text::Rule))
                .map(|r| page.text_of(&r.text))
                .collect();
            // Trailing spaces hang (the grid clips them), and may span
            // several runs: leave them all out.
            let mut runs = runs;
            while let Some(last) = runs.last_mut() {
                *last = last.trim_end_matches(' ');
                if last.is_empty() {
                    runs.pop();
                } else {
                    break;
                }
            }
            let shown: usize = runs.iter().map(|r| display_width(r)).sum();
            // A wide character with only one column left (a one-column
            // page, or beside an indent) gets a line of its own and the
            // grid draws a space: only that last grapheme may overflow.
            let last = runs
                .last()
                .and_then(|r| {
                    unicode_segmentation::UnicodeSegmentation::graphemes(
                        r.trim_end_matches(' '),
                        true,
                    )
                    .next_back()
                })
                .map_or(0, display_width);
            let wide_last = last == 2 && shown - last < usize::from(width);
            if shown > usize::from(width) && !wide_last {
                return Err(format!("line {line} is {shown} wide, page {width}"));
            }
        }
        Ok(())
    }

    #[test]
    fn no_line_is_wider_than_the_page() {
        let source = "# Heading that is long\n\n- item with words\n  1. nested numbered item\n\n> quote with a long line of text\n\n`code` and [link](http://example.org/a/long/path)\n\n> - [ ] quoted task in a list";
        for width in [1, 2, 3, 5, 13, 40] {
            for kind in [Kind::Markdown, Kind::Plain] {
                assert_fits(&render(source, kind, width), width).unwrap();
            }
        }
    }

    /// Each row's note line, `0` for none.
    fn numbers(source: &str, kind: Kind, width: u16) -> Vec<u32> {
        let page = render(source, kind, width);
        (0..page.line_count())
            .map(|i| page.number(i).unwrap_or(0))
            .collect()
    }

    /// A row has the number of the note line that starts on it, counted
    /// as the editor does; wrapped rows, blanks, the renderer's blank row
    /// between blocks and table borders have none.
    #[test]
    fn rows_carry_the_note_lines_they_start() {
        let source = "# Title\n\none two three\nfour\n\n- a\n- [ ] b\n\n> q\n> r\n\n---";
        assert_eq!(
            md(source, 12),
            [
                "◉ Title",
                "",
                "  one two",
                "  three",
                "  four",
                "",
                "  • a",
                "  • [ ] b",
                "",
                "  │ q",
                "  │ r",
                "",
                "  ──────────"
            ]
        );
        assert_eq!(
            numbers(source, Kind::Markdown, 12),
            [1, 0, 2, 0, 3, 0, 4, 5, 0, 6, 7, 0, 8]
        );
        // `\r\n` is one line break, as in the editor.
        assert_eq!(numbers("a\r\nb", Kind::Markdown, 9), [1, 2]);
        // Code keeps its lines; a blank one, or one of spaces only, has
        // no number and is not counted.
        assert_eq!(numbers("```\nx\n\ny\n```", Kind::Markdown, 9), [2, 0, 3]);
        assert_eq!(numbers("```\nx\n   \ny\n```", Kind::Markdown, 9), [2, 0, 3]);
        // A boxed table: rows numbered, borders not; the `|-|` line
        // under the header makes no row.
        assert_eq!(
            numbers("| a |\n|---|\n| b |\n| c |", Kind::Markdown, 20),
            [0, 1, 0, 3, 0, 4, 0]
        );
        // Plain text: every line its own, blanks without.
        assert_eq!(numbers("aaa bbb\n\nc", Kind::Plain, 4), [1, 0, 0, 2]);
        // Each row's line, wrapped ones included, for copying.
        let page = render("# T\n\naaa bbb\n\n| x |\n|-|", Kind::Markdown, 8);
        let lines: Vec<_> = (0..page.line_count()).map(|i| page.note_line(i)).collect();
        assert_eq!(
            lines,
            [Some(1), None, Some(2), Some(2), None, None, Some(3), None]
        );
        // A reference link's address is numbered by the link, not by its
        // definition further down (`ftp:` is not clickable, so its
        // address is shown).
        assert_eq!(
            numbers("[x][r] and words\n\n[r]: ftp://e.org", Kind::Markdown, 9),
            [1, 0, 0, 0]
        );
    }

    proptest::proptest! {
        // Numbers only go up, and never past the note's last line.
        #[test]
        fn numbers_increase_within_the_note(
            parts in proptest::collection::vec(
                proptest::sample::select(vec![
                    "# ", "- ", "1. ", "> ", "```\n", "|", "|-|", "---", "\n", "\r\n", "\n\n",
                    "word ", "[x][r]", "[r]: http://e.org\n", "- [ ] ", "<b>", "\t", "\u{1b}",
                ]),
                0..40,
            ),
            width in 1u16..30,
        ) {
            let source: String = parts.concat();
            let lines = u32::try_from(source.matches('\n').count()).unwrap() + 1;
            for kind in [Kind::Markdown, Kind::Plain] {
                let mut last = 0;
                for n in numbers(&source, kind, width).into_iter().filter(|&n| n > 0) {
                    proptest::prop_assert!(n > last && n <= lines, "{n} after {last} of {lines}");
                    last = n;
                }
            }
        }

        // Anything at all: no panic, no line wider than the page, nothing
        // forbidden on screen.
        #[test]
        fn any_text_renders_safely(source in proptest::prelude::any::<String>(), width in 1u16..40) {
            for kind in [Kind::Markdown, Kind::Plain] {
                let page = render(&source, kind, width);
                assert_fits(&page, width).map_err(proptest::test_runner::TestCaseError::fail)?;
                let height = u16::try_from(page.line_count().min(200)).unwrap();
                let mut grid = Grid::new(width, height);
                let area = grid.area();
                page.draw(&mut grid, area, 0);
                for y in 0..height {
                    for x in 0..width {
                        let symbol = grid.cell(x, y).unwrap().symbol();
                        proptest::prop_assert!(!symbol.contains(is_forbidden));
                    }
                }
            }
        }

        // Markdown-shaped text, to reach the parser's constructs.
        #[test]
        fn markdown_shapes_render_safely(
            parts in proptest::collection::vec(
                proptest::sample::select(vec![
                    "# ", "## ", "- ", "1. ", "> ", "```\n", "`", "*", "**", "~~", "[", "]", "(",
                    ")", "[[", "]]", "|", "---", "\n", "\n\n", "  ", "word", "日本", "&#27;",
                    "<b>", "\u{1b}[2J", "\u{202E}", "\t", "- [ ] ", "![", "<https://x>",
                ]),
                0..60,
            ),
            width in 1u16..30,
        ) {
            let source: String = parts.concat();
            let page = render(&source, Kind::Markdown, width);
            assert_fits(&page, width).map_err(proptest::test_runner::TestCaseError::fail)?;
        }
    }
}
