//! The screen grid: one cell per terminal column and row (decision 0031).
//!
//! - A cell holds one grapheme (a character with its combining marks, or
//!   an emoji sequence) inline, at most `MAX_CELL_BYTES` bytes, with its
//!   width and style. No cell allocates; a grid is one allocation of
//!   width × height cells that never grows, and is wiped on drop, since it
//!   holds note text.
//! - Text enters only through `put_str`, which splits it into graphemes,
//!   measures them with `unicode-width`, and replaces anything unsafe or
//!   unmeasurable with `?`: a control or BiDi character (`is_forbidden`),
//!   a grapheme wider than 2 columns, or one longer than `MAX_CELL_BYTES`.
//! - A wide grapheme takes two cells: the first holds it, the second is a
//!   continuation (width 0). Overwriting either half blanks the other, so
//!   the grid never holds half a wide character.
//! - Nothing panics: writes outside the grid are clipped.
//!
//! Not `Debug`: cells hold note text (docs/design/leakage.md).

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the viewer loop (Phase 3, V6)")
)]

use std::ops::BitOr;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use zeroize::Zeroize;

use super::layout::Rect;
use crate::model::assert_not_impl;

/// The longest grapheme a cell holds, in bytes. Longer ones (stacked
/// combining marks, "Zalgo" text) are drawn as `?`: they could not be
/// shown sensibly in one cell anyway.
pub const MAX_CELL_BYTES: usize = 16;

/// Drawn in place of a grapheme that must not, or cannot, be shown: plain
/// ASCII, so every font has it (U+FFFD is missing from some terminal
/// fonts and then shows as nothing; docs/decisions/0037-visible-escapes.md).
/// Note text normally reaches the grid sanitized (`md::safe`), with hidden
/// characters spelled out; this is the last line of defence.
pub const REPLACEMENT: &str = "?";

/// True for characters that must never reach the terminal: C0 controls,
/// DEL and C1 controls (`char::is_control`, which covers ESC, CSI U+009B
/// and the rest), and the BiDi controls that reorder text on screen
/// (Trojan Source, CVE-2021-42574): ALM, LRM, RLM, the embeddings and
/// overrides U+202A to U+202E, and the isolates U+2066 to U+2069.
pub fn is_forbidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        )
}

/// Whether `grapheme` carries a character that shows as nothing, or as a
/// blank, behind a visible one: `a` and a tag character look like `a`.
/// Such a grapheme is spelled out whole, so text that looks the same is
/// the same. A joiner between two pictographs and the emoji-style selector
/// U+FE0F stay, as emoji need them (`👨‍💻`, `❤️`, `1️⃣`).
pub fn hides_something(grapheme: &str) -> bool {
    let emoji = grapheme.starts_with(is_pictographic) || grapheme.ends_with('\u{20E3}');
    let mut chars = grapheme.chars().peekable();
    while let Some(c) = chars.next() {
        let hidden = match c {
            '\u{200D}' => !(emoji && chars.peek().is_some_and(|&next| is_pictographic(next))),
            '\u{FE0F}' => !emoji,
            c => is_invisible(c),
        };
        if hidden {
            return true;
        }
    }
    false
}

/// Characters that draw nothing (or a blank) yet are not spaces: the
/// combining grapheme joiner, the Hangul fillers, the zero-width
/// characters and marks, word joiner and invisible operators, the byte
/// order mark, the variation selectors (which can carry hidden bytes
/// inside one emoji), and the tag characters.
pub fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{034F}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2060}'..='\u{2064}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{E0000}'..='\u{E007F}'
            | '\u{E0100}'..='\u{E01EF}'
    )
}

/// The start of an emoji, roughly Unicode's Extended_Pictographic: a few
/// symbols that have an emoji form, and the emoji planes. Too wide only
/// lets a joiner or U+FE0F stay after an unusual symbol.
pub fn is_pictographic(c: char) -> bool {
    matches!(
        c,
        '\u{00A9}'
            | '\u{00AE}'
            | '\u{203C}'
            | '\u{2049}'
            | '\u{2122}'
            | '\u{2139}'
            | '\u{2194}'..='\u{21FF}'
            | '\u{2300}'..='\u{23FF}'
            | '\u{24C2}'
            | '\u{25A0}'..='\u{27BF}'
            | '\u{2900}'..='\u{297F}'
            | '\u{2B00}'..='\u{2BFF}'
            | '\u{3030}'
            | '\u{303D}'
            | '\u{3297}'
            | '\u{3299}'
            | '\u{1F000}'..='\u{1FFFF}'
    )
}

/// A terminal color: the default, one of the 16 named ones, or one of the
/// 256 indexed ones.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Color {
    #[default]
    Default,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    White,
    Indexed(u8),
}

/// Text attributes, as a set.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mods(u8);

impl Mods {
    pub const NONE: Self = Self(0);
    pub const BOLD: Self = Self(1);
    pub const DIM: Self = Self(1 << 1);
    pub const ITALIC: Self = Self(1 << 2);
    pub const UNDERLINED: Self = Self(1 << 3);
    pub const REVERSED: Self = Self(1 << 4);
    pub const CROSSED_OUT: Self = Self(1 << 5);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Both sets; `|` in a `const`.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl BitOr for Mods {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Colors and attributes of a cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub mods: Mods,
}

impl Style {
    pub const PLAIN: Self = Self {
        fg: Color::Default,
        bg: Color::Default,
        mods: Mods::NONE,
    };

    pub const fn fg(self, fg: Color) -> Self {
        Self { fg, ..self }
    }

    pub const fn bg(self, bg: Color) -> Self {
        Self { bg, ..self }
    }

    pub const fn mods(self, mods: Mods) -> Self {
        Self { mods, ..self }
    }
}

/// One terminal column of one row.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    bytes: [u8; MAX_CELL_BYTES],
    len: u8,
    /// 1 or 2 for a grapheme, 0 for the right half of a wide one.
    width: u8,
    style: Style,
}

assert_not_impl!(Cell: std::fmt::Debug, std::fmt::Display);

impl Cell {
    /// A space in `style`.
    const fn blank(style: Style) -> Self {
        let mut bytes = [0; MAX_CELL_BYTES];
        bytes[0] = b' ';
        Self {
            bytes,
            len: 1,
            width: 1,
            style,
        }
    }

    /// The right half of a wide grapheme.
    const fn continuation(style: Style) -> Self {
        Self {
            bytes: [0; MAX_CELL_BYTES],
            len: 0,
            width: 0,
            style,
        }
    }

    /// `symbol` must be at most `MAX_CELL_BYTES` long (checked by `shape`).
    fn new(symbol: &str, width: u8, style: Style) -> Self {
        let mut bytes = [0; MAX_CELL_BYTES];
        bytes[..symbol.len()].copy_from_slice(symbol.as_bytes());
        Self {
            bytes,
            // At most MAX_CELL_BYTES, so it fits.
            len: symbol.len() as u8,
            width,
            style,
        }
    }

    /// The grapheme; empty for a continuation.
    pub fn symbol(&self) -> &str {
        // Always whole UTF-8, as copied from a &str; the fallback only
        // keeps this free of panics.
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or(REPLACEMENT)
    }

    /// Columns taken: 1 or 2, or 0 for a continuation.
    pub fn width(&self) -> u8 {
        self.width
    }

    pub fn style(&self) -> Style {
        self.style
    }
}

/// The columns `grapheme` takes in a grid: exactly what `Grid::put_str`
/// gives it, so wrapping and drawing always agree (0 means left out).
pub fn grapheme_width(grapheme: &str) -> u8 {
    shape(grapheme).1
}

/// The columns `text` takes when drawn on one row with room to spare.
pub fn display_width(text: &str) -> usize {
    text.graphemes(true)
        .map(|g| usize::from(grapheme_width(g)))
        .sum()
}

/// What a grapheme is drawn as, and its width; width 0 means it is left
/// out (a lone zero-width character, such as a joiner at the start).
fn shape(grapheme: &str) -> (&str, u8) {
    if grapheme.contains(is_forbidden) {
        return (REPLACEMENT, 1);
    }
    match grapheme.width() {
        0 => ("", 0),
        width @ (1 | 2) if grapheme.len() <= MAX_CELL_BYTES => (grapheme, width as u8),
        _ => (REPLACEMENT, 1),
    }
}

/// The terminal's cells, row by row.
pub struct Grid {
    width: u16,
    height: u16,
    cells: Vec<Cell>,
}

impl Grid {
    /// A blank grid of `width` columns and `height` rows.
    pub fn new(width: u16, height: u16) -> Self {
        let count = usize::from(width) * usize::from(height);
        Self {
            width,
            height,
            cells: vec![Cell::blank(Style::PLAIN); count],
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    /// The whole grid as a rectangle.
    pub fn area(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// Blanks every cell of `area` (clipped to the grid) in `style`.
    pub fn fill(&mut self, area: Rect, style: Style) {
        let area = area.intersection(self.area());
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                self.put(x, y, " ", 1, style);
            }
        }
    }

    /// Blanks every cell; the text they held is wiped.
    pub fn reset(&mut self) {
        for cell in &mut self.cells {
            cell.bytes.zeroize();
            *cell = Cell::blank(Style::PLAIN);
        }
    }

    fn index(&self, x: u16, y: u16) -> Option<usize> {
        (x < self.width && y < self.height)
            .then(|| usize::from(y) * usize::from(self.width) + usize::from(x))
    }

    /// The cell at (x, y), if inside the grid.
    pub fn cell(&self, x: u16, y: u16) -> Option<&Cell> {
        self.index(x, y).map(|i| &self.cells[i])
    }

    /// Writes `text` from (x, y) rightwards, at most `max` columns and
    /// never past the right edge; returns the columns used. A wide
    /// grapheme that does not fit whole is replaced by a space, and the
    /// text stops there. Nothing is written outside the grid.
    pub fn put_str(&mut self, x: u16, y: u16, text: &str, style: Style, max: u16) -> u16 {
        if y >= self.height {
            return 0;
        }
        let limit = x.saturating_add(max).min(self.width);
        let mut column = x;
        for grapheme in text.graphemes(true) {
            let (symbol, width) = shape(grapheme);
            if width == 0 {
                continue;
            }
            // In u32: on a grid 65535 columns wide, column + 2 overflows u16.
            let end = u32::from(column) + u32::from(width);
            if end > u32::from(limit) {
                if column < limit {
                    self.put(column, y, " ", 1, style);
                    column += 1;
                }
                break;
            }
            self.put(column, y, symbol, width, style);
            column += u16::from(width);
        }
        column.saturating_sub(x)
    }

    /// Sets the style of `len` cells from (x, y), keeping their text.
    pub fn set_style(&mut self, x: u16, y: u16, len: u16, style: Style) {
        for column in x..x.saturating_add(len).min(self.width) {
            if let Some(i) = self.index(column, y) {
                self.cells[i].style = style;
            }
        }
    }

    /// Puts one shaped grapheme; for width 2, (x + 1, y) is inside the
    /// grid (checked by `put_str`). Blanks the other half of any wide
    /// grapheme it overwrites.
    fn put(&mut self, x: u16, y: u16, symbol: &str, width: u8, style: Style) {
        let Some(i) = self.index(x, y) else {
            return;
        };
        self.split_wide(i);
        if width == 2 {
            self.split_wide(i + 1);
            self.cells[i + 1] = Cell::continuation(style);
        }
        self.cells[i] = Cell::new(symbol, width, style);
    }

    /// Stores `symbol` as is, unchecked, so the frame writer's own check
    /// can be tested.
    #[cfg(test)]
    pub fn put_raw_for_test(&mut self, x: u16, y: u16, symbol: &str, width: u8) {
        let i = self.index(x, y).unwrap();
        self.cells[i] = Cell::new(symbol, width, Style::PLAIN);
        if width == 2 {
            self.cells[i + 1] = Cell::continuation(Style::PLAIN);
        }
    }

    /// If cell `i` is half of a wide grapheme, blanks the other half (cell
    /// `i` itself is about to be overwritten).
    fn split_wide(&mut self, i: usize) {
        let row_start = i - i % usize::from(self.width);
        match self.cells[i].width {
            0 if i > row_start => {
                let style = self.cells[i - 1].style;
                self.cells[i - 1] = Cell::blank(style);
            }
            2 if i + 1 < row_start + usize::from(self.width) => {
                let style = self.cells[i + 1].style;
                self.cells[i + 1] = Cell::blank(style);
            }
            _ => {}
        }
    }

    /// The cells of `self` that differ from `shown`, as (x, y, cell), left
    /// to right and top to bottom; continuations are left out (the wide
    /// grapheme before them covers them). If the sizes differ, every cell
    /// counts as changed.
    pub fn diff<'a>(&'a self, shown: &'a Grid) -> impl Iterator<Item = (u16, u16, &'a Cell)> {
        let same_size = self.width == shown.width && self.height == shown.height;
        let width = usize::from(self.width.max(1));
        self.cells
            .iter()
            .enumerate()
            .filter(move |&(i, cell)| cell.width > 0 && !(same_size && shown.cells[i] == *cell))
            .map(move |(i, cell)| {
                // Both fit in u16: i < width * height.
                ((i % width) as u16, (i / width) as u16, cell)
            })
    }
}

impl Drop for Grid {
    fn drop(&mut self) {
        for cell in &mut self.cells {
            cell.bytes.zeroize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row as text; a continuation shows as nothing.
    fn row(grid: &Grid, y: u16) -> String {
        (0..grid.width())
            .map(|x| grid.cell(x, y).unwrap().symbol())
            .collect()
    }

    fn widths(grid: &Grid, y: u16) -> Vec<u8> {
        (0..grid.width())
            .map(|x| grid.cell(x, y).unwrap().width())
            .collect()
    }

    #[test]
    fn writes_text_and_reports_columns() {
        let mut grid = Grid::new(6, 2);
        assert_eq!(grid.put_str(1, 1, "abc", Style::PLAIN, 10), 3);
        assert_eq!(row(&grid, 0), "      ");
        assert_eq!(row(&grid, 1), " abc  ");
    }

    #[test]
    fn clips_at_max_and_at_the_right_edge() {
        let mut grid = Grid::new(5, 1);
        assert_eq!(grid.put_str(0, 0, "abcdef", Style::PLAIN, 2), 2);
        assert_eq!(row(&grid, 0), "ab   ");
        assert_eq!(grid.put_str(3, 0, "xyz", Style::PLAIN, 10), 2);
        assert_eq!(row(&grid, 0), "ab xy");
    }

    #[test]
    fn writes_outside_are_ignored() {
        let mut grid = Grid::new(3, 2);
        assert_eq!(grid.put_str(0, 2, "abc", Style::PLAIN, 3), 0);
        assert_eq!(grid.put_str(3, 0, "abc", Style::PLAIN, 3), 0);
        assert_eq!(
            grid.put_str(u16::MAX, u16::MAX, "a", Style::PLAIN, u16::MAX),
            0
        );
        grid.set_style(2, 5, 9, Style::PLAIN.fg(Color::Red));
        assert_eq!(row(&grid, 0), "   ");
        assert!(grid.cell(3, 0).is_none());
        let mut empty = Grid::new(0, 0);
        assert_eq!(empty.put_str(0, 0, "a", Style::PLAIN, 1), 0);
        assert_eq!(empty.diff(&Grid::new(0, 0)).count(), 0);
    }

    #[test]
    fn widest_grid_never_wraps_a_wide_grapheme() {
        let mut grid = Grid::new(u16::MAX, 1);
        assert_eq!(grid.put_str(u16::MAX - 1, 0, "日", Style::PLAIN, 9), 1);
        assert_eq!(grid.cell(u16::MAX - 1, 0).unwrap().symbol(), " ");
    }

    #[test]
    fn wide_graphemes_take_two_cells() {
        let mut grid = Grid::new(5, 1);
        assert_eq!(grid.put_str(0, 0, "日本x", Style::PLAIN, 5), 5);
        assert_eq!(row(&grid, 0), "日本x");
        assert_eq!(widths(&grid, 0), [2, 0, 2, 0, 1]);
    }

    #[test]
    fn wide_grapheme_that_does_not_fit_becomes_a_space() {
        let mut grid = Grid::new(3, 1);
        assert_eq!(grid.put_str(0, 0, "a日b", Style::PLAIN, 2), 2);
        assert_eq!(row(&grid, 0), "a  ");
        assert_eq!(widths(&grid, 0), [1, 1, 1]);
    }

    #[test]
    fn overwriting_half_of_a_wide_grapheme_blanks_the_other_half() {
        let mut grid = Grid::new(4, 1);
        grid.put_str(0, 0, "日本", Style::PLAIN, 4);
        // Right half of 日 and left half of 本.
        grid.put_str(1, 0, "ab", Style::PLAIN, 2);
        assert_eq!(row(&grid, 0), " ab ");
        assert_eq!(widths(&grid, 0), [1, 1, 1, 1]);
        // A wide grapheme over the middle of two others.
        grid.put_str(0, 0, "日本", Style::PLAIN, 4);
        grid.put_str(1, 0, "語", Style::PLAIN, 2);
        assert_eq!(row(&grid, 0), " 語 ");
        assert_eq!(widths(&grid, 0), [1, 2, 0, 1]);
    }

    #[test]
    fn graphemes_stay_together() {
        let mut grid = Grid::new(4, 1);
        // e + combining acute; a family emoji (ZWJ sequence); a flag.
        assert_eq!(grid.put_str(0, 0, "e\u{301}", Style::PLAIN, 4), 1);
        assert_eq!(grid.cell(0, 0).unwrap().symbol(), "e\u{301}");
        let mut grid = Grid::new(4, 1);
        assert_eq!(grid.put_str(0, 0, "🇩🇪", Style::PLAIN, 4), 2);
        assert_eq!(grid.cell(0, 0).unwrap().symbol(), "🇩🇪");
    }

    #[test]
    fn forbidden_and_oversized_graphemes_become_replacements() {
        let zalgo = format!("a{}", "\u{301}".repeat(20));
        let cases = [
            "\u{1b}", "\u{7}", "\u{0}", "\u{7f}", "\u{9b}", "\u{85}", "\t", "\r", "\n", "\u{202E}",
            "\u{2066}", "\u{200F}", "\u{061C}", &zalgo,
        ];
        for text in cases {
            let mut grid = Grid::new(3, 1);
            assert_eq!(grid.put_str(0, 0, text, Style::PLAIN, 3), 1, "{text:?}");
            assert_eq!(row(&grid, 0), "?  ", "{text:?}");
        }
        // An escape sequence: ESC is replaced, the rest is plain text.
        let mut grid = Grid::new(6, 1);
        grid.put_str(0, 0, "\u{1b}[2J", Style::PLAIN, 6);
        assert_eq!(row(&grid, 0), "?[2J  ");
    }

    #[test]
    fn no_cell_ever_holds_a_forbidden_character() {
        // Every char from U+0000 to U+30FF, alone and after a letter.
        let mut grid = Grid::new(8, 1);
        for c in (0..=0x30FF).filter_map(char::from_u32) {
            for text in [c.to_string(), format!("a{c}b")] {
                grid.reset();
                grid.put_str(0, 0, &text, Style::PLAIN, 8);
                for x in 0..8 {
                    let symbol = grid.cell(x, 0).unwrap().symbol();
                    assert!(!symbol.contains(is_forbidden), "{c:?}");
                }
            }
        }
    }

    #[test]
    fn display_width_matches_what_is_drawn() {
        for text in [
            "",
            "abc",
            "日本x",
            "e\u{301}",
            "a\u{1b}b",
            "\u{200D}a",
            "🇩🇪!",
        ] {
            let mut grid = Grid::new(40, 1);
            let drawn = grid.put_str(0, 0, text, Style::PLAIN, 40);
            assert_eq!(display_width(text), usize::from(drawn), "{text:?}");
        }
    }

    #[test]
    fn zero_width_graphemes_are_left_out() {
        let mut grid = Grid::new(3, 1);
        assert_eq!(grid.put_str(0, 0, "\u{200D}a", Style::PLAIN, 3), 1);
        assert_eq!(row(&grid, 0), "a  ");
    }

    #[test]
    fn forbidden_characters_are_exactly_these() {
        for c in (0u32..=0x1F).chain(0x7F..=0x9F).filter_map(char::from_u32) {
            assert!(is_forbidden(c), "{c:?}");
        }
        for c in [
            '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}',
            '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ] {
            assert!(is_forbidden(c), "{c:?}");
        }
        for c in [
            ' ', 'a', '~', '\u{A0}', 'é', '日', '\u{200D}', '\u{FFFD}', '🙂',
        ] {
            assert!(!is_forbidden(c), "{c:?}");
        }
    }

    #[test]
    fn diff_lists_changed_cells_without_continuations() {
        let shown = Grid::new(4, 2);
        let mut next = Grid::new(4, 2);
        next.put_str(1, 0, "日", Style::PLAIN, 2);
        next.put_str(0, 1, "a", Style::PLAIN.fg(Color::Red), 1);
        let changed: Vec<(u16, u16, String)> = next
            .diff(&shown)
            .map(|(x, y, cell)| (x, y, cell.symbol().to_owned()))
            .collect();
        assert_eq!(changed, [(1, 0, "日".to_owned()), (0, 1, "a".to_owned())]);
        assert_eq!(next.diff(&next).count(), 0);
    }

    #[test]
    fn diff_against_another_size_lists_everything() {
        let next = Grid::new(2, 2);
        assert_eq!(next.diff(&Grid::new(3, 2)).count(), 4);
    }

    #[test]
    fn style_changes_count_as_changes() {
        let shown = Grid::new(3, 1);
        let mut next = Grid::new(3, 1);
        next.set_style(1, 0, 1, Style::PLAIN.mods(Mods::BOLD | Mods::UNDERLINED));
        let changed: Vec<u16> = next.diff(&shown).map(|(x, _, _)| x).collect();
        assert_eq!(changed, [1]);
        let style = next.cell(1, 0).unwrap().style();
        assert!(style.mods.contains(Mods::BOLD) && style.mods.contains(Mods::UNDERLINED));
        assert!(!style.mods.contains(Mods::ITALIC));
    }

    #[test]
    fn reset_blanks_and_wipes() {
        let mut grid = Grid::new(3, 1);
        grid.put_str(0, 0, "日a", Style::PLAIN.fg(Color::Red), 3);
        grid.reset();
        assert_eq!(row(&grid, 0), "   ");
        assert_eq!(grid.diff(&Grid::new(3, 1)).count(), 0);
        assert!(
            grid.cells
                .iter()
                .all(|c| c.bytes[1..].iter().all(|&b| b == 0))
        );
    }
}
