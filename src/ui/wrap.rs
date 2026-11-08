//! Word wrap by display width (decision 0031).
//!
//! Input is a run of styled spans; output is lines of pieces, each a byte
//! range into one span. Nothing is copied, so wrapping makes no new copy
//! of note text. Widths come from `grid::grapheme_width`, the same as
//! `Grid::put_str` uses, so a wrapped line always fits its width when
//! drawn.
//!
//! - A newline (`\n` or `\r\n`) ends a line.
//! - A line breaks after the last space that fits; a word longer than the
//!   width is broken between graphemes. Spaces that do not fit hang at the
//!   end of the line (drawing clips them), so no line starts with the space
//!   it broke at, and no text is dropped.
//! - A grapheme wider than the whole width (a wide one in a 1-column
//!   pane) gets a line of its own; the grid draws it as a space.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the viewer loop (Phase 3, V6)")
)]

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use super::grid::{Grid, Style, grapheme_width};

/// Text in one style.
#[derive(Clone, Copy)]
pub struct Span<'a> {
    pub text: &'a str,
    pub style: Style,
}

/// Part of a wrapped line: `range` of the text of span number `span`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Piece {
    pub span: usize,
    pub range: Range<usize>,
}

/// A wrapped line.
pub type Line = Vec<Piece>;

/// One grapheme of the input, by position.
struct Unit {
    span: usize,
    range: Range<usize>,
    width: u16,
    space: bool,
    newline: bool,
}

/// Wraps `spans` to `width` columns. Empty input is one empty line; a
/// width of 0 gives no lines.
pub fn wrap(spans: &[Span], width: u16) -> Vec<Line> {
    if width == 0 {
        return Vec::new();
    }
    let units: Vec<Unit> = spans
        .iter()
        .enumerate()
        .flat_map(|(span, s)| {
            s.text.grapheme_indices(true).map(move |(start, g)| Unit {
                span,
                range: start..start + g.len(),
                width: u16::from(grapheme_width(g)),
                space: g == " ",
                newline: g == "\n" || g == "\r\n",
            })
        })
        .collect();

    let mut lines = Vec::new();
    let mut start = 0;
    let mut column: u32 = 0;
    // Where the line may break: just after the last space.
    let mut after_space: Option<usize> = None;
    let mut i = 0;
    while i < units.len() {
        let unit = &units[i];
        if unit.newline {
            lines.push(pieces(&units[start..i]));
            start = i + 1;
            column = 0;
            after_space = None;
            i += 1;
            continue;
        }
        let fits = column + u32::from(unit.width) <= u32::from(width);
        if !fits && !unit.space && i > start {
            let cut = after_space.filter(|&b| b > start && b <= i).unwrap_or(i);
            lines.push(pieces(&units[start..cut]));
            start = cut;
            column = units[cut..i].iter().map(|u| u32::from(u.width)).sum();
            after_space = None;
            // Look at the same grapheme again on the new line.
            continue;
        }
        column += u32::from(unit.width);
        if unit.space {
            after_space = Some(i + 1);
        }
        i += 1;
    }
    lines.push(pieces(&units[start..]));
    lines
}

/// Consecutive graphemes of one span become one piece.
fn pieces(units: &[Unit]) -> Line {
    let mut line: Line = Vec::new();
    for unit in units {
        match line.last_mut() {
            Some(last) if last.span == unit.span && last.range.end == unit.range.start => {
                last.range.end = unit.range.end;
            }
            _ => line.push(Piece {
                span: unit.span,
                range: unit.range.clone(),
            }),
        }
    }
    line
}

/// Draws `line` at (x, y), at most `max` columns; returns the columns used.
pub fn draw_line(grid: &mut Grid, x: u16, y: u16, max: u16, line: &[Piece], spans: &[Span]) -> u16 {
    let mut used = 0;
    for piece in line {
        let Some(span) = spans.get(piece.span) else {
            continue;
        };
        let Some(text) = span.text.get(piece.range.clone()) else {
            continue;
        };
        used += grid.put_str(x.saturating_add(used), y, text, span.style, max - used);
    }
    used
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn plain(text: &str) -> [Span<'_>; 1] {
        [Span {
            text,
            style: Style::PLAIN,
        }]
    }

    /// The lines of `text` wrapped to `width`, as strings.
    fn lines(text: &str, width: u16) -> Vec<String> {
        let spans = plain(text);
        wrap(&spans, width)
            .iter()
            .map(|line| line.iter().map(|p| &text[p.range.clone()]).collect())
            .collect()
    }

    #[test]
    fn breaks_after_the_last_space_that_fits() {
        assert_eq!(
            lines("the quick brown fox", 10),
            ["the quick ", "brown fox"]
        );
        assert_eq!(lines("a b c", 3), ["a b ", "c"]);
        assert_eq!(lines("ab    cd", 3), ["ab    ", "cd"]);
        assert_eq!(lines("short", 10), ["short"]);
    }

    #[test]
    fn long_words_break_between_graphemes() {
        assert_eq!(lines("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(lines("ab cdefghij", 4), ["ab ", "cdef", "ghij"]);
    }

    #[test]
    fn newlines_end_lines() {
        assert_eq!(lines("a\nb\r\nc", 10), ["a", "b", "c"]);
        assert_eq!(lines("a\n\nb\n", 10), ["a", "", "b", ""]);
        assert_eq!(lines("", 10), [""]);
        assert!(wrap(&plain("abc"), 0).is_empty());
    }

    #[test]
    fn wide_graphemes_count_two_columns() {
        assert_eq!(lines("日本語です", 4), ["日本", "語で", "す"]);
        assert_eq!(lines("a日", 2), ["a", "日"]);
        // Wider than the pane: a line of its own.
        assert_eq!(lines("日日", 1), ["日", "日"]);
    }

    #[test]
    fn styles_are_kept_across_breaks() {
        let bold = Style::PLAIN.mods(crate::ui::grid::Mods::BOLD);
        let spans = [
            Span {
                text: "plain ",
                style: Style::PLAIN,
            },
            Span {
                text: "bold words",
                style: bold,
            },
        ];
        let wrapped = wrap(&spans, 11);
        let piece = |span, range| Piece { span, range };
        assert_eq!(
            wrapped,
            [vec![piece(0, 0..6), piece(1, 0..5)], vec![piece(1, 5..10)]]
        );
        let mut grid = Grid::new(11, 1);
        assert_eq!(draw_line(&mut grid, 0, 0, 11, &wrapped[0], &spans), 11);
        assert_eq!(grid.cell(6, 0).unwrap().style(), bold);
        assert_eq!(grid.cell(5, 0).unwrap().style(), Style::PLAIN);
    }

    #[test]
    fn draw_line_ignores_pieces_that_do_not_belong() {
        let spans = plain("abc");
        let bad = [
            Piece {
                span: 5,
                range: 0..1,
            },
            Piece {
                span: 0,
                range: 2..9,
            },
        ];
        let mut grid = Grid::new(3, 1);
        assert_eq!(draw_line(&mut grid, 0, 0, 3, &bad, &spans), 0);
    }

    /// Text from a mix of ASCII, spaces, newlines, wide, combining,
    /// emoji, control and BiDi characters.
    fn text() -> impl Strategy<Value = String> {
        let pieces = prop::sample::select(vec![
            "a", "b", "word", " ", "  ", "\n", "\r\n", "日", "本", "e\u{301}", "🙂", "🇩🇪",
            "\u{1b}", "\t", "\u{202E}", "\u{200D}", "\u{9b}",
        ]);
        prop::collection::vec(pieces, 0..60).prop_map(|v| v.concat())
    }

    proptest! {
        #[test]
        fn every_line_fits_and_nothing_is_lost(text in text(), width in 1u16..12) {
            let spans = plain(&text);
            let wrapped = wrap(&spans, width);
            let mut grid = Grid::new(width, 1);
            let mut joined = String::new();
            for line in &wrapped {
                grid.reset();
                let used = draw_line(&mut grid, 0, 0, width, line, &spans);
                prop_assert!(used <= width);
                for piece in line {
                    joined.push_str(&text[piece.range.clone()]);
                }
                // Every drawn cell is free of forbidden characters.
                for x in 0..width {
                    let symbol = grid.cell(x, 0).unwrap().symbol();
                    prop_assert!(!symbol.contains(crate::ui::grid::is_forbidden));
                }
            }
            // Only the newlines are left out.
            let expected: String = text.replace("\r\n", "").replace('\n', "");
            prop_assert_eq!(joined, expected);
        }

        #[test]
        fn lines_are_as_full_as_they_can_be(text in "[a-z ]{0,80}", width in 1u16..20) {
            // A break happens only when the next grapheme would not fit.
            let spans = plain(&text);
            let wrapped = wrap(&spans, width);
            for pair in wrapped.windows(2) {
                let len: usize = pair[0].iter().map(|p| p.range.len()).sum();
                prop_assert!(len >= 1);
                let next_word_len = text[pair[1][0].range.clone()]
                    .split(' ')
                    .next()
                    .map_or(0, str::len);
                // The next line's first word (or its first grapheme, or
                // the space that overflowed) did not fit on this one.
                prop_assert!(len + next_word_len + 1 > usize::from(width));
            }
        }
    }
}
