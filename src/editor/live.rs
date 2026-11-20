//! Live mode's runs (`edit --live`, steps LE1 to LE7, LR1, LI1, LI2):
//! which bytes of the editor's text are shown, hidden or replaced, and in
//! which style; which elements the cursor reveals; and each line's layout
//! (`layout`: the viewer's indent, list markers, quote bars and heading
//! icons, standing in for the line's leading markup).
//!
//! The text is parsed with pulldown-cmark, as the viewer parses it
//! (`render::OPTIONS`), keeping each event's source range. Text the
//! parser decodes (entities, escapes) is never used: a run is always a
//! range of the editor's own text, drawn through the editor's own checks,
//! or a fixed string from here.
//!
//! The text is walked once, in order. Between two events lies markup:
//! inside emphasis, strong, strikethrough, a link, an image or a heading
//! it is hidden (`**`, `[`, `](url)`, `## `), anywhere else it is shown
//! as written. A line break is never hidden, so lines stay lines.
//!
//! Blocks look as in the viewer: a task box `[ ]` or `[x]` is dimmed, a
//! rule becomes a gray line, a code block's text is gray. A table's cells
//! are styled; its pipes and `|---|` line are shown as written here (the
//! view boxes a table that is not edited, steps LT1 to LT3).

pub mod layout;

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, LinkType, Parser, Tag, TagEnd};
use zeroize::Zeroizing;

use crate::md::blocks::blocks;
use crate::md::link::clickable;
use crate::md::render::{CODE, LINK, OPTIONS, QUOTE_BAR, eq_ci, heading_mark};
use crate::notes::Fragment;
use crate::ui::grid::{Mods, Style, display_width};

/// How a run's bytes are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    /// As written, in the run's style.
    Text,
    /// Not at all: zero columns.
    Hidden,
    /// A rule's line in their place, to the row's end as in the viewer
    /// (`glyph::put`); one column when rows are measured, so it never
    /// wraps.
    Rule,
}

impl Show {
    /// The columns a rule's line counts, drawn once at its run's start;
    /// `None` for text as written or hidden.
    pub fn columns(self) -> Option<usize> {
        match self {
            Show::Text | Show::Hidden => None,
            Show::Rule => Some(1),
        }
    }
}

/// A range of the text and how it is drawn. Holds offsets, never text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub range: Range<usize>,
    pub show: Show,
    pub style: Style,
}

/// What the markup and text inside an open tag look like.
#[derive(Clone, Copy)]
struct Frame {
    style: Style,
    /// Markup here is hidden.
    hide: bool,
}

/// A task box or a list's number, dimmed as in the viewer.
const TASK: Style = Style::PLAIN.mods(Mods::DIM);

/// A note's text as live mode sees it.
pub struct Parsed {
    /// In order, without gaps or overlaps, from 0 to the text's end, each
    /// non-empty and on char boundaries.
    pub runs: Vec<Run>,
    /// Each element the cursor reveals (step LR1), by start: an inline
    /// tag, inline code, a fenced code block, a rule, or a heading's
    /// closing markup, without a final line break. Every byte the runs do
    /// not show as written lies in one, or in a line's leading markup
    /// (the layout's).
    pub elements: Vec<Range<usize>>,
    /// Each line indented as in the viewer (steps LI1, LI2).
    pub layout: layout::Layout,
    /// The links a click follows (step LF3), in order.
    pub links: Vec<Link>,
    /// Code: each code block and inline code span, without a final line
    /// break, in order. Drawn as written, it keeps the code style.
    pub code: Vec<Range<usize>>,
    /// Each fenced code block and its fences (without line breaks): the
    /// cursor in the block shows its fences, its code stays styled.
    pub fenced: Vec<(Range<usize>, Vec<Range<usize>>)>,
}

/// A link a click follows (step LF3). Holds offsets, never text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// The whole link as written.
    pub range: Range<usize>,
    /// Where its target is written: a `[[link]]`'s name and fragment, or
    /// a web address `md::link::clickable` accepts.
    pub target: Range<usize>,
    /// A web link: the terminal's to open (Ctrl+click), never Crystal's.
    pub web: bool,
}

/// Where `fragment` starts in `body` (step LF3): the text of the heading
/// with that text, ignoring case as the viewer does (past its `#`s, so it
/// stays styled), or the start of the block with that `^id`.
pub fn fragment_at(body: &str, fragment: &Fragment<'_>) -> Option<usize> {
    match fragment {
        Fragment::Block(id) => blocks(body)
            .into_iter()
            .find(|block| {
                block
                    .id
                    .as_ref()
                    .is_some_and(|r| body.get(r.clone()) == Some(*id))
            })
            .map(|block| block.text.start),
        Fragment::Heading(heading) => {
            // The heading's text as the parser reads it, in zeroizing
            // memory: it is note text.
            // Its first text's start, and its text.
            let mut open: Option<(Option<usize>, Zeroizing<String>)> = None;
            for (event, range) in Parser::new_ext(body, OPTIONS).into_offset_iter() {
                match event {
                    Event::Start(Tag::Heading { .. }) => {
                        open = Some((None, Zeroizing::new(String::new())));
                    }
                    Event::Text(t) | Event::Code(t) => {
                        if let Some((start, text)) = &mut open {
                            start.get_or_insert(range.start);
                            text.push_str(&t);
                        }
                    }
                    Event::End(TagEnd::Heading(_)) => {
                        if let Some((start, text)) = open.take()
                            && eq_ci(&text, heading)
                        {
                            return Some(start.unwrap_or(range.start));
                        }
                    }
                    _ => {}
                }
            }
            None
        }
    }
}

/// Parses `text` into its runs and elements.
pub fn parse(text: &str) -> Parsed {
    let mut out = Runs {
        text,
        runs: Vec::new(),
        elements: Vec::new(),
        code: Vec::new(),
        pos: 0,
    };
    let top = Frame {
        style: Style::PLAIN,
        hide: false,
    };
    let mut stack = vec![top];
    let mut lines = layout::Builder::new(text);
    let mut links = Vec::new();
    // The fenced code block being read: its start, and its fences so far.
    let mut fence: Option<(usize, Vec<Range<usize>>)> = None;
    let mut fenced = Vec::new();
    for (event, range) in Parser::new_ext(text, OPTIONS).into_offset_iter() {
        lines.event(&event, &range);
        let frame = stack.last().copied().unwrap_or(top);
        match event {
            Event::Start(tag) => {
                out.gap(range.start, frame);
                let mut inner = frame;
                // A link a click follows: a `[[link]]`, or a web address
                // the viewer would make clickable; its target where it is
                // written (one decoded from escapes is not followed).
                if let Tag::Link {
                    link_type,
                    dest_url,
                    ..
                } = &tag
                {
                    let wiki = matches!(link_type, LinkType::WikiLink { .. });
                    let found = text[range.clone()].find(&**dest_url);
                    if let Some(i) = found.filter(|_| !dest_url.is_empty())
                        && (wiki || clickable(dest_url))
                    {
                        let start = range.start + i;
                        links.push(Link {
                            range: range.clone(),
                            target: start..start + dest_url.len(),
                            web: !wiki,
                        });
                    }
                }
                if matches!(
                    tag,
                    Tag::Emphasis
                        | Tag::Strong
                        | Tag::Strikethrough
                        | Tag::Link { .. }
                        | Tag::Image { .. }
                ) {
                    out.element(range.clone());
                }
                // A code block: its fences are elements of their own, so
                // the cursor in its code leaves it styled.
                if let Tag::CodeBlock(kind) = &tag {
                    out.code(range.clone());
                    if matches!(kind, CodeBlockKind::Fenced(_)) {
                        fence = Some((range.start, Vec::new()));
                    }
                }
                match tag {
                    Tag::Emphasis => inner.style = add(frame.style, Mods::ITALIC),
                    Tag::Strong => inner.style = add(frame.style, Mods::BOLD),
                    Tag::Strikethrough => inner.style = add(frame.style, Mods::CROSSED_OUT),
                    Tag::Link { .. } => inner.style = LINK,
                    Tag::Image { .. } => inner.style = add(frame.style, Mods::DIM),
                    Tag::Heading { level, .. } => inner.style = heading_mark(level).1,
                    // A table's header, bold as in the viewer.
                    Tag::TableHead => inner.style = add(frame.style, Mods::BOLD),
                    Tag::CodeBlock(_) => inner.style = CODE,
                    // A numbered item's number, dimmed as in the viewer.
                    Tag::Item => {
                        let digits = text[range.start..]
                            .bytes()
                            .take_while(u8::is_ascii_digit)
                            .count();
                        let mark = text.as_bytes().get(range.start + digits);
                        if digits > 0 && matches!(mark, Some(b'.' | b')')) {
                            out.push(range.start..range.start + digits + 1, Show::Text, TASK);
                        }
                    }
                    _ => {}
                }
                inner.hide |= matches!(
                    tag,
                    Tag::Emphasis
                        | Tag::Strong
                        | Tag::Strikethrough
                        | Tag::Link { .. }
                        | Tag::Image { .. }
                        | Tag::CodeBlock(CodeBlockKind::Fenced(_))
                );
                stack.push(inner);
            }
            Event::End(tag) => {
                // The closing markup belongs to the tag. A heading is no
                // element: it stays styled while the cursor is in its
                // text, and its leading `#`s are the line's markup (the
                // layout's); only closing `#`s or a Setext underline are
                // hidden here, as an element of their own.
                let start = out.pos;
                if matches!(tag, TagEnd::Heading(_)) {
                    out.hide(start..range.end);
                    out.element(start..range.end);
                } else {
                    out.gap(range.end, frame);
                }
                // A fenced block's closing fence (or both, if it has no
                // code): from its code's end to its own.
                if let (TagEnd::CodeBlock, Some((block, mut fences))) = (tag, fence.take()) {
                    let first = if fences.is_empty() { block } else { start };
                    fences.extend(out.element(first..range.end));
                    fenced.push((block..range.end, fences));
                }
                stack.pop();
            }
            Event::Code(_) => {
                out.gap(range.start, frame);
                out.element(range.clone());
                out.code(range.clone());
                // The backticks around inline code are hidden.
                let inner = text[range.clone()].trim_matches('`');
                let start = range.start + (text[range.clone()].len() - inner.len()) / 2;
                let ticks = start - range.start;
                let end = range.end - ticks;
                out.hide(range.start..start);
                if start < end {
                    out.push(start..end, Show::Text, code(frame.style));
                }
                out.hide(end..range.end);
            }
            Event::Rule => {
                out.gap(range.start, frame);
                // The line break after it stays shown.
                let end = range.start + text[range].trim_end_matches(['\n', '\r']).len();
                out.element(out.pos..end);
                out.push(out.pos..end, Show::Rule, QUOTE_BAR);
            }
            Event::TaskListMarker(_) => {
                out.gap(range.start, frame);
                out.push(range, Show::Text, TASK);
            }
            _ => {
                // A fenced block's first code: its opening fence ends here.
                if let Some((block, fences)) = &mut fence
                    && fences.is_empty()
                {
                    fences.extend(out.element(*block..range.start));
                    // Marked, even when the fence made no element.
                    if fences.is_empty() {
                        fences.push(*block..*block);
                    }
                }
                out.gap(range.start, frame);
                out.push(range, Show::Text, frame.style);
            }
        }
    }
    out.push(out.pos..text.len(), Show::Text, Style::PLAIN);
    out.elements.sort_by_key(|range| range.start);
    // Each table column's widest cell as drawn (step LT1).
    let mut layout = lines.finish();
    for table in &mut layout.tables {
        table.widths = vec![0; table.aligns.len()];
        for (_, cells) in &table.rows {
            for (width, cell) in table.widths.iter_mut().zip(cells) {
                let drawn = u16::try_from(drawn_width(text, &out.runs, cell));
                *width = (*width).max(drawn.unwrap_or(u16::MAX));
            }
        }
    }
    Parsed {
        runs: out.runs,
        elements: out.elements,
        layout,
        links,
        code: out.code,
        fenced,
    }
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

/// How far the cursor still counts as in a table cell whose text (without
/// the spaces around it) is `cell` (steps TB1 to TB6): up to the spaces
/// after the text, so a space typed at a cell's end leaves the cursor in
/// the cell, drawn right after its text, and the table boxed.
pub fn cell_reach(text: &str, cell: &Range<usize>) -> usize {
    let after = text.get(cell.end..).unwrap_or("");
    cell.end + after.bytes().take_while(|&b| b == b' ').count()
}

/// The columns `range` takes as `runs` draw it on one line: hidden bytes
/// none, a fixed string its own width.
pub fn drawn_width(text: &str, runs: &[Run], range: &Range<usize>) -> usize {
    let first = runs.partition_point(|run| run.range.end <= range.start);
    runs[first..]
        .iter()
        .take_while(|run| run.range.start < range.end)
        .map(|run| match run.show {
            Show::Text => {
                let part = run.range.start.max(range.start)..run.range.end.min(range.end);
                display_width(&text[part])
            }
            Show::Hidden => 0,
            show if run.range.start >= range.start => show.columns().unwrap_or(0),
            _ => 0,
        })
        .sum()
}

fn add(style: Style, mods: Mods) -> Style {
    style.mods(style.mods.union(mods))
}

/// Inline code: the viewer's code style, kept bold or crossed out by an
/// outer tag.
fn code(outer: Style) -> Style {
    CODE.mods(outer.mods)
}

/// The runs built so far, up to `pos`, and the elements found.
struct Runs<'a> {
    text: &'a str,
    runs: Vec<Run>,
    elements: Vec<Range<usize>>,
    code: Vec<Range<usize>>,
    pos: usize,
}

impl Runs<'_> {
    /// Records an element, without its final line break.
    fn element(&mut self, range: Range<usize>) -> Option<Range<usize>> {
        let range = self.trimmed(range)?;
        self.elements.push(range.clone());
        Some(range)
    }

    /// Records code (a block or an inline span), without its line break.
    fn code(&mut self, range: Range<usize>) {
        if let Some(range) = self.trimmed(range) {
            self.code.push(range);
        }
    }

    /// `range` without a final line break; `None` if nothing is left.
    fn trimmed(&self, range: Range<usize>) -> Option<Range<usize>> {
        let end = range.start
            + self.text[range.clone()]
                .trim_end_matches(['\n', '\r'])
                .len();
        (range.start < end).then_some(range.start..end)
    }

    /// Adds `range` (its part past `pos`), joined to the last run when
    /// drawn alike.
    fn push(&mut self, range: Range<usize>, show: Show, style: Style) {
        let range = range.start.max(self.pos)..range.end;
        if range.is_empty() {
            return;
        }
        self.pos = range.end;
        if let Some(last) = self.runs.last_mut()
            && last.range.end == range.start
            && last.style == style
            && last.show == show
            && show.columns().is_none()
        {
            last.range.end = range.end;
            return;
        }
        self.runs.push(Run { range, show, style });
    }

    /// Hides `range`, except its line breaks, which stay shown.
    fn hide(&mut self, range: Range<usize>) {
        let mut start = range.start.max(self.pos);
        while start < range.end {
            let piece = &self.text[start..range.end];
            match piece.find(['\n', '\r']) {
                Some(0) => {
                    let len = if piece.starts_with("\r\n") { 2 } else { 1 };
                    self.push(start..start + len, Show::Text, Style::PLAIN);
                    start += len;
                }
                Some(i) => {
                    self.push(start..start + i, Show::Hidden, Style::PLAIN);
                    start += i;
                }
                None => {
                    self.push(start..range.end, Show::Hidden, Style::PLAIN);
                    start = range.end;
                }
            }
        }
    }

    /// The markup from `pos` to `end`: hidden inside `frame` if it hides
    /// markup, else shown as written.
    fn gap(&mut self, end: usize, frame: Frame) {
        if frame.hide {
            self.hide(self.pos..end);
        } else {
            self.push(self.pos..end, Show::Text, Style::PLAIN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::md::render::DASHES;
    use crate::ui::grid::Color;
    use proptest::prelude::*;

    fn runs(text: &str) -> Vec<Run> {
        parse(text).runs
    }

    /// The text as live mode shows it: hidden bytes left out, fixed
    /// strings in place.
    fn shown(text: &str) -> String {
        runs(text)
            .iter()
            .map(|run| match run.show {
                Show::Text => &text[run.range.clone()],
                Show::Hidden => "",
                Show::Rule => DASHES,
            })
            .collect()
    }

    /// The style of the first shown run holding `part`.
    fn style_of(text: &str, part: &str) -> Style {
        let at = text.find(part).unwrap();
        runs(text)
            .into_iter()
            .find(|run| run.show == Show::Text && run.range.contains(&at))
            .unwrap()
            .style
    }

    #[test]
    fn inline_markup_is_hidden_and_styled() {
        let text = "a **bold** *it* ~~gone~~ `code` b";
        assert_eq!(shown(text), "a bold it gone code b");
        assert!(style_of(text, "bold").mods.contains(Mods::BOLD));
        assert!(style_of(text, "it").mods.contains(Mods::ITALIC));
        assert!(style_of(text, "gone").mods.contains(Mods::CROSSED_OUT));
        assert_eq!(style_of(text, "code"), CODE);
        assert_eq!(style_of(text, "a "), Style::PLAIN);
        // Nested: bold and italic both.
        let text = "***both***";
        assert_eq!(shown(text), "both");
        let mods = style_of(text, "both").mods;
        assert!(mods.contains(Mods::BOLD) && mods.contains(Mods::ITALIC));
        // Double backticks, and a backtick inside.
        assert_eq!(shown("``a`b``"), "a`b");
    }

    #[test]
    fn links_show_their_words() {
        let text = "see [the site](https://example.org) and [[Note|alias]] or [[Plain]]";
        assert_eq!(shown(text), "see the site and alias or Plain");
        assert_eq!(style_of(text, "the site"), LINK);
        assert_eq!(style_of(text, "alias"), LINK);
        assert_eq!(style_of(text, "Plain"), LINK);
        assert_eq!(shown("![alt](x.png)"), "alt");
    }

    #[test]
    fn headings_get_the_viewers_style() {
        // The leading `#`s are the line's markup, the icon the layout's
        // (step LI2): the runs show them.
        let text = "## Title\nbody";
        let style = heading_mark(pulldown_cmark::HeadingLevel::H2).1;
        assert_eq!(shown(text), "## Title\nbody");
        assert_eq!(style_of(text, "Title"), style);
        assert_eq!(style.fg, Color::Green);
        assert_eq!(style_of(text, "body"), Style::PLAIN);
        // Closing `#`s hidden; a Setext heading keeps its line break and
        // hides its underline.
        assert_eq!(shown("# A ##"), "# A");
        assert_eq!(shown("Title\n==="), "Title\n");
    }

    #[test]
    fn line_breaks_and_other_markup_stay() {
        // A strong span over two lines keeps its line break.
        assert_eq!(shown("**a\nb**"), "a\nb");
        assert_eq!(shown("**a\r\nb**"), "a\r\nb");
        // Numbers and blank lines are shown as written; a table's pipes
        // too, its cells styled (step LT1).
        let table = "| a | **b** |\n|---|---|\n| 1 | 2 |";
        assert_eq!(shown(table), "| a | b |\n|---|---|\n| 1 | 2 |");
        assert!(style_of(table, "b").mods.contains(Mods::BOLD));
        assert!(style_of(table, "a").mods.contains(Mods::BOLD));
        assert_eq!(style_of(table, "1"), Style::PLAIN);
        for text in ["1. one\n2. two", "plain\n\n\nblank lines"] {
            assert_eq!(shown(text), text);
        }
    }

    /// Blocks look as in the viewer (step LE4).
    #[test]
    fn blocks_look_as_in_the_viewer() {
        // A task box dimmed. List markers and quotes' `>`s are leading
        // markup: the layout's (steps LI1, LI2).
        let text = "- item\n+ [ ] task";
        assert_eq!(style_of(text, "[ ]"), TASK);
        // A list's number, dimmed as in the viewer.
        assert_eq!(style_of("12) twelve", "12)"), TASK);
        assert_eq!(style_of("12) twelve", "twelve"), Style::PLAIN);
        assert_eq!(style_of(text, "item"), Style::PLAIN);
        assert_eq!(shown("> a > b"), "> a > b");
        // A fenced code block: fences hidden, rows kept, text gray.
        let text = "```rust\nlet x;\n```\nafter";
        assert_eq!(shown(text), "\nlet x;\n\nafter");
        assert_eq!(style_of(text, "let x;"), CODE);
        assert_eq!(style_of(text, "after"), Style::PLAIN);
        // An indented one stays, gray.
        let text = "    code\n";
        assert_eq!(shown(text), text);
        assert_eq!(style_of(text, "code"), CODE);
        // Rules, after a blank line (else `---` makes a heading).
        assert_eq!(shown("a\n\n---\nb"), format!("a\n\n{DASHES}\nb"));
        assert_eq!(shown("***"), DASHES);
        let rule = runs("***").into_iter().next().unwrap();
        assert_eq!(rule.style, QUOTE_BAR);
    }

    /// Links a click follows (step LF3): a `[[link]]` and a web address
    /// the viewer makes clickable, their targets where written; another
    /// address is not one.
    #[test]
    fn links_record_their_targets() {
        let text = "see [[Note#Head|alias]], [site](https://example.org) and [rel](notes.md)";
        let links = parse(text).links;
        let targets: Vec<(&str, bool)> = links
            .iter()
            .map(|link| (&text[link.target.clone()], link.web))
            .collect();
        assert_eq!(
            targets,
            [("Note#Head", false), ("https://example.org", true)]
        );
        assert_eq!(&text[links[0].range.clone()], "[[Note#Head|alias]]");
    }

    /// Where a link's fragment leads (step LF3): a heading's text, matched
    /// ignoring case; a block's start; nothing for a missing one.
    #[test]
    fn fragments_lead_to_their_line() {
        let body = "# Intro\ntext\n## Next *Part*\nx ^abc\n";
        assert_eq!(fragment_at(body, &Fragment::Heading("next part")), Some(16));
        assert_eq!(fragment_at(body, &Fragment::Heading("Intro")), Some(2));
        assert_eq!(fragment_at(body, &Fragment::Block("abc")), Some(28));
        assert_eq!(fragment_at(body, &Fragment::Heading("none")), None);
        assert_eq!(fragment_at(body, &Fragment::Block("zzz")), None);
    }

    /// A table's cells and column widths as drawn (step LT1): hidden
    /// markup takes no column; a short row has fewer cells (step TB5),
    /// a real empty one (`||`) is kept.
    #[test]
    fn tables_record_cells_and_widths() {
        let text = "> | a  | **bold** |\n> |:-|--:|\n> | `日本` |";
        let parsed = parse(text);
        let [table] = parsed.layout.tables.as_slice() else {
            panic!("one table");
        };
        assert_eq!(table.lines, 0..3);
        let cells: Vec<(usize, Vec<&str>)> = table
            .rows
            .iter()
            .map(|(line, cells)| (*line, cells.iter().map(|c| &text[c.clone()]).collect()))
            .collect();
        assert_eq!(cells, [(0, vec!["a", "**bold**"]), (2, vec!["`日本`"])]);
        let short = parse("| a | b | c |\n|-|-|-|\n| x |\n||  |\n| y | |\nz");
        let lens: Vec<usize> = short.layout.tables[0]
            .rows
            .iter()
            .map(|(_, c)| c.len())
            .collect();
        assert_eq!(lens, [3, 1, 2, 2, 1]);
        assert_eq!(table.widths, [4, 4]);
        // In a list, after a paragraph: its own lines.
        let text = "- x\n\n  | a |\n  |---|\n  | b |\nafter";
        let table = &parse(text).layout.tables[0];
        assert_eq!(table.lines, 2..5);
        assert_eq!(table.widths, [1]);
    }

    proptest! {
        /// Any text: the runs cover it in order, each non-empty, on char
        /// boundaries; a line break is never hidden or replaced.
        #[test]
        fn runs_cover_the_text(text in "([*_`#\\[\\]()|>~!\\\\ a\n\r-]|é|日){0,80}") {
            let mut at = 0;
            for run in runs(&text) {
                prop_assert_eq!(run.range.start, at);
                prop_assert!(run.range.start < run.range.end);
                prop_assert!(text.is_char_boundary(run.range.end));
                if run.show != Show::Text {
                    prop_assert!(!text[run.range.clone()].contains(['\n', '\r']));
                }
                at = run.range.end;
            }
            prop_assert_eq!(at, text.len());
        }

        /// Any text: every byte not shown as written lies in an element,
        /// so a cursor there reveals it (step LR1).
        #[test]
        fn hidden_bytes_lie_in_elements(text in "([*_`#\\[\\]()|>~!\\\\ a\n\r-]|é|日){0,80}") {
            let parsed = parse(&text);
            // Per byte: two elements' hidden markup can make one run. A
            // heading's `#`s are its line's markup, which the view reveals.
            for run in parsed.runs.iter().filter(|run| run.show != Show::Text) {
                for at in run.range.clone() {
                    let inside = parsed.elements.iter().any(|e| e.contains(&at))
                        || parsed.layout.lines.iter().any(|l| l.markup.contains(&at));
                    prop_assert!(inside, "{} outside {:?}", at, parsed.elements);
                }
            }
        }
    }
}
