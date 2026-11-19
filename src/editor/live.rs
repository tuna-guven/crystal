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
//! rule becomes a gray line, a code block's text is gray. A table is
//! shown as written.

pub mod layout;

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag};

use crate::md::render::{CODE, DASHES, LINK, OPTIONS, QUOTE_BAR, heading_mark};
use crate::ui::grid::{Mods, Style};

/// How a run's bytes are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    /// As written, in the run's style.
    Text,
    /// Not at all: zero columns.
    Hidden,
    /// This fixed string in their place (a rule's line).
    Fixed(&'static str),
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
    /// Everything here is shown as written, unstyled (a table).
    raw: bool,
}

/// A task box, dimmed as in the viewer.
const TASK: Style = Style::PLAIN.mods(Mods::DIM);

/// A note's text as live mode sees it.
pub struct Parsed {
    /// In order, without gaps or overlaps, from 0 to the text's end, each
    /// non-empty and on char boundaries.
    pub runs: Vec<Run>,
    /// Each element the cursor reveals (step LR1), by start: an inline
    /// tag, inline code, a heading, a fenced code block or a rule, without
    /// a final line break. Every byte the runs do not show as written lies
    /// in one; a line's leading markup is the layout's.
    pub elements: Vec<Range<usize>>,
    /// Each line indented as in the viewer (steps LI1, LI2).
    pub layout: layout::Layout,
}

/// Parses `text` into its runs and elements.
pub fn parse(text: &str) -> Parsed {
    let mut out = Runs {
        text,
        runs: Vec::new(),
        elements: Vec::new(),
        pos: 0,
    };
    let top = Frame {
        style: Style::PLAIN,
        hide: false,
        raw: false,
    };
    let mut stack = vec![top];
    let mut lines = layout::Builder::new(text);
    for (event, range) in Parser::new_ext(text, OPTIONS).into_offset_iter() {
        lines.event(&event, &range);
        let frame = stack.last().copied().unwrap_or(top);
        match event {
            Event::Start(tag) => {
                out.gap(range.start, frame);
                let mut inner = frame;
                if !frame.raw
                    && matches!(
                        tag,
                        Tag::Emphasis
                            | Tag::Strong
                            | Tag::Strikethrough
                            | Tag::Link { .. }
                            | Tag::Image { .. }
                            | Tag::Heading { .. }
                            | Tag::CodeBlock(CodeBlockKind::Fenced(_))
                    )
                {
                    out.element(range.clone());
                }
                match tag {
                    _ if frame.raw => {}
                    Tag::Emphasis => inner.style = add(frame.style, Mods::ITALIC),
                    Tag::Strong => inner.style = add(frame.style, Mods::BOLD),
                    Tag::Strikethrough => inner.style = add(frame.style, Mods::CROSSED_OUT),
                    Tag::Link { .. } => inner.style = LINK,
                    Tag::Image { .. } => inner.style = add(frame.style, Mods::DIM),
                    Tag::Heading { level, .. } => inner.style = heading_mark(level).1,
                    Tag::CodeBlock(_) => inner.style = CODE,
                    Tag::Table(_) => {
                        inner = Frame {
                            style: Style::PLAIN,
                            hide: false,
                            raw: true,
                        };
                    }
                    _ => {}
                }
                inner.hide |= !inner.raw
                    && matches!(
                        tag,
                        Tag::Emphasis
                            | Tag::Strong
                            | Tag::Strikethrough
                            | Tag::Link { .. }
                            | Tag::Image { .. }
                            | Tag::Heading { .. }
                            | Tag::CodeBlock(CodeBlockKind::Fenced(_))
                    );
                stack.push(inner);
            }
            Event::End(_) => {
                // The closing markup belongs to the tag.
                out.gap(range.end, frame);
                stack.pop();
            }
            Event::Code(_) if !frame.raw => {
                out.gap(range.start, frame);
                out.element(range.clone());
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
            Event::Rule if !frame.raw => {
                out.gap(range.start, frame);
                // The line break after it stays shown.
                let end = range.start + text[range].trim_end_matches(['\n', '\r']).len();
                out.element(out.pos..end);
                out.push(out.pos..end, Show::Fixed(DASHES), QUOTE_BAR);
            }
            Event::TaskListMarker(_) if !frame.raw => {
                out.gap(range.start, frame);
                out.push(range, Show::Text, TASK);
            }
            _ => {
                out.gap(range.start, frame);
                let style = if frame.raw { Style::PLAIN } else { frame.style };
                out.push(range, Show::Text, style);
            }
        }
    }
    out.push(out.pos..text.len(), Show::Text, Style::PLAIN);
    out.elements.sort_by_key(|range| range.start);
    Parsed {
        runs: out.runs,
        elements: out.elements,
        layout: lines.finish(),
    }
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
    pos: usize,
}

impl Runs<'_> {
    /// Records an element, without its final line break.
    fn element(&mut self, range: Range<usize>) {
        let end = range.start
            + self.text[range.clone()]
                .trim_end_matches(['\n', '\r'])
                .len();
        if range.start < end {
            self.elements.push(range.start..end);
        }
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
            && !matches!(show, Show::Fixed(_))
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
                Show::Fixed(s) => s,
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
        // The `#`s are hidden; the icon is the layout's (step LI2).
        let text = "## Title\nbody";
        let style = heading_mark(pulldown_cmark::HeadingLevel::H2).1;
        assert_eq!(shown(text), "Title\nbody");
        assert_eq!(style_of(text, "Title"), style);
        assert_eq!(style.fg, Color::Green);
        assert_eq!(style_of(text, "body"), Style::PLAIN);
        // Closing `#`s hidden; a Setext heading keeps its line break and
        // hides its underline.
        assert_eq!(shown("# A ##"), "A");
        assert_eq!(shown("Title\n==="), "Title\n");
    }

    #[test]
    fn line_breaks_and_other_markup_stay() {
        // A strong span over two lines keeps its line break.
        assert_eq!(shown("**a\nb**"), "a\nb");
        assert_eq!(shown("**a\r\nb**"), "a\r\nb");
        // Tables, numbers and blank lines are shown as written.
        for text in [
            "| a | **b** |\n|---|---|\n| 1 | 2 |",
            "1. one\n2. two",
            "plain\n\n\nblank lines",
        ] {
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
            // Per byte: two elements' hidden markup can make one run.
            for run in parsed.runs.iter().filter(|run| run.show != Show::Text) {
                for at in run.range.clone() {
                    let inside = parsed.elements.iter().any(|e| e.contains(&at));
                    prop_assert!(inside, "{} outside {:?}", at, parsed.elements);
                }
            }
        }
    }
}
