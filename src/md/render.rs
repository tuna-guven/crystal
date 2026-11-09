//! The renderer (docs/design/dependencies.md, "Markdown renderer"): a
//! note's text to wrapped, styled lines for the pager.
//!
//! - **Markdown** (every note but `.txt`): pulldown-cmark events, a small
//!   subset drawn as text with styles: headings, emphasis, strong,
//!   strikethrough, inline and block code, lists (nested, numbered, task
//!   items), block quotes, rules, simple tables, links shown with their
//!   address, Obsidian `[[links]]`, images as `[image: alt]`, raw HTML
//!   dimmed and literal. Nothing is ever opened or followed here.
//! - **Plain text** (`*.txt`, docs/decisions/0038-plain-text-notes.md):
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

use pulldown_cmark::{CowStr, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd};

use super::safe::{SafeText, sanitize};
use crate::ui::grid::{Color, Grid, Mods, Style, display_width};
use crate::ui::layout::Rect;
use crate::ui::wrap::{Span, wrap_each};

/// How a note is shown, from its title (docs/decisions/0038-plain-text-notes.md).
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
    /// Heading text (sanitized) and its line.
    headings: Vec<(Zeroizing<String>, usize)>,
    /// Block ids and their first line.
    blocks: Vec<(Zeroizing<String>, usize)>,
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

    /// The line of the block with this id.
    pub fn block_line(&self, id: &str) -> Option<usize> {
        self.blocks
            .iter()
            .find(|(block, _)| block.as_str() == id)
            .map(|(_, line)| *line)
    }

    /// Draws the lines from `scroll` down into `area`, with link
    /// `selected` (if any) reversed.
    pub fn draw_selected(
        &self,
        grid: &mut Grid,
        area: Rect,
        scroll: usize,
        selected: Option<usize>,
    ) {
        let selected = selected.and_then(|s| u32::try_from(s).ok());
        for (y, index) in (area.y..area.bottom()).zip(scroll..self.line_count()) {
            let mut used = 0;
            for run in self.line(index) {
                let x = area.x.saturating_add(used);
                let room = area.width - used;
                let style = if run.link.is_some() && run.link == selected {
                    run.style.mods(run.style.mods | Mods::REVERSED)
                } else {
                    run.style
                };
                used += match run.text {
                    Text::Rule => {
                        for i in 0..room {
                            grid.put_str(x + i, y, "─", style, 1);
                        }
                        room
                    }
                    ref text => grid.put_str(x, y, self.text_of(text), style, room),
                };
            }
        }
    }

    /// Draws the lines from `scroll` down into `area`.
    #[cfg(test)]
    pub fn draw(&self, grid: &mut Grid, area: Rect, scroll: usize) {
        self.draw_selected(grid, area, scroll, None);
    }
}

/// Renders `text` as `kind` for `width` columns. The whole note is laid
/// out at once: an 8 MiB note of empty paragraphs is about 4 million lines
/// (about 120 MB of line data), within the 512 MiB memory budget. Accepted;
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
        headings,
        blocks,
        ..
    } = builder;
    Page {
        source,
        extra,
        runs,
        line_starts,
        links,
        headings,
        blocks,
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
    page_links: Vec<PageLink>,
    headings: Vec<(Zeroizing<String>, usize)>,
    blocks: Vec<(Zeroizing<String>, usize)>,
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
            page_links: Vec::new(),
            headings: Vec::new(),
            blocks: Vec::new(),
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

    fn add(&mut self, text: Text, style: Style) {
        let link = self.current_link;
        self.content.push(Run { text, style, link });
    }

    fn markdown(&mut self) {
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_WIKILINKS;
        // The parser borrows the sanitized text for 's, apart from `self`.
        let source: &'s SafeText = self.source;
        for event in Parser::new_ext(source.as_str(), options) {
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
            Event::SoftBreak => self.add(Text::Fixed(" "), PLAIN),
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
            Tag::Paragraph | Tag::HtmlBlock | Tag::TableHead | Tag::TableRow => self.start_block(),
            Tag::Heading { level, .. } => {
                self.start_block();
                let (marker, color) = match level {
                    HeadingLevel::H1 => ("# ", Color::Magenta),
                    HeadingLevel::H2 => ("## ", Color::Blue),
                    HeadingLevel::H3 => ("### ", Color::Cyan),
                    HeadingLevel::H4 => ("#### ", Color::Cyan),
                    HeadingLevel::H5 => ("##### ", Color::Cyan),
                    HeadingLevel::H6 => ("###### ", Color::Cyan),
                };
                self.add(Text::Fixed(marker), DIM);
                self.push_style(|_| PLAIN.fg(color).mods(Mods::BOLD));
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
                    },
                    width: u16::try_from(width).unwrap_or(2).min(16),
                    shown: false,
                });
            }
            Tag::Table(_) => self.start_block(),
            Tag::TableCell => {
                if !self.content.is_empty() {
                    self.add(Text::Fixed(" │ "), QUOTE_BAR);
                }
            }
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
                let shown = match link_type {
                    LinkType::Autolink | LinkType::Email | LinkType::WikiLink { .. } => None,
                    _ if dest_url.is_empty() => None,
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
            TagEnd::HtmlBlock | TagEnd::Table => {
                self.flush();
                self.blank = true;
            }
            TagEnd::Heading(_) => {
                self.pop_style();
                // Content run 0 is the `#` marker.
                let text = self.content_text(1);
                self.headings.push((text, self.line_starts.len()));
                self.flush();
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
            TagEnd::TableHead | TagEnd::TableRow => self.flush(),
            TagEnd::TableCell => {}
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript => self.pop_style(),
            TagEnd::Link => {
                self.current_link = None;
                self.pop_style();
                if let Some(Some(url)) = self.links.pop() {
                    self.add(Text::Fixed(" ("), DIM);
                    self.add(url, DIM);
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

    /// Before a new block: finish the one in progress, and leave a blank
    /// line after the previous block if it asked for one.
    fn start_block(&mut self) {
        self.flush();
        if self.blank && !self.line_starts.is_empty() {
            self.line_starts.push(self.runs.len());
        }
        self.blank = false;
    }

    /// The prefix of the next line (quote bars, list markers or their
    /// indent), and its width. Marks item markers as shown.
    fn prefix(&mut self) -> (Vec<Run>, u16) {
        let mut runs = Vec::new();
        let mut width: u16 = 0;
        for container in self.containers.iter_mut().take(MAX_INDENT_LEVELS) {
            match container {
                Container::Quote => {
                    runs.push(Run {
                        text: Text::Fixed("│ "),
                        style: QUOTE_BAR,
                        link: None,
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
        // Spans for wrap, each with where its text comes from.
        let mut spans: Vec<(usize, Span<'_>)> = Vec::new();
        let mut origin: Vec<&Run> = Vec::new();
        for run in &content {
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
        // A rule is one line, whatever the width.
        if matches!(
            content.first(),
            Some(Run {
                text: Text::Rule,
                ..
            })
        ) {
            lines.push(content.clone());
        } else {
            let room = match self.prefix_width() {
                // No room for text beside the indent: no indent.
                w if w >= self.width => self.width.max(1),
                w => self.width - w,
            };
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
                            Text::Extra(i, _) => Text::Extra(
                                *i,
                                offset + piece.range.start..offset + piece.range.end,
                            ),
                            Text::Fixed(s) => Text::Fixed(&s[piece.range.clone()]),
                            Text::Rule => Text::Rule,
                        };
                        Run {
                            text,
                            style: only[piece.span].style,
                            link: run.link,
                        }
                    })
                    .collect();
                lines.push(runs);
            });
        }
        let indent = self.prefix_width() < self.width;
        for line in lines {
            let (prefix, _) = self.prefix();
            let number = self.line_starts.len();
            for run in &line {
                if let Some(link) = run.link.and_then(|l| self.page_links.get_mut(l as usize)) {
                    link.line.get_or_insert(number);
                }
            }
            self.line_starts.push(self.runs.len());
            if indent {
                self.runs.extend(prefix);
            }
            self.runs.extend(line);
        }
    }

    /// The prefix width, without marking markers as shown.
    fn prefix_width(&self) -> u16 {
        self.containers
            .iter()
            .take(MAX_INDENT_LEVELS)
            .map(|c| match c {
                Container::Quote => 2,
                Container::List(_) => 0,
                Container::Item { width, .. } => *width,
            })
            .sum()
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
        assert_eq!(
            md("one two three\nfour\n\nnext one", 10),
            ["one two", "three four", "", "next one"]
        );
    }

    #[test]
    fn headings_and_emphasis() {
        assert_eq!(
            md("# Title\n\nSome *it* and **bold**.\n\n## Sub", 30),
            ["# Title", "", "Some it and bold.", "", "## Sub"]
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
        assert_eq!(md("> quoted\n> text", 20), ["│ quoted text"]);
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
            ["see site (https://x.org) and https://y.org"]
        );
        assert_eq!(md("[[Roadmap]] and [[work/a|A]]", 40), ["Roadmap and A"]);
        assert_eq!(md("![a cat](cat.png)", 40), ["[image: a cat]"]);
        assert_eq!(md("<b>raw</b>", 40), ["<b>raw</b>"]);
        assert_eq!(
            md("| a | b |\n|---|---|\n| 1 | 2 |", 40),
            ["a │ b", "1 │ 2"]
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
        assert_eq!(page.heading_line("part"), line_of("## Part"));
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
        let area = grid.area();
        page.draw_selected(&mut grid, area, 0, Some(1));
        let row = u16::try_from(page.link_line(1).unwrap()).unwrap();
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

    proptest::proptest! {
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
