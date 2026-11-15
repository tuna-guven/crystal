//! The shell's output, in memory only (decision 0033): at
//! most `MAX_ENTRIES` entries, the oldest dropped (and wiped) first. Each
//! entry is sanitized text with a style, wrapped when drawn, so a resize
//! re-wraps everything. Never written to the terminal's scrollback, a file
//! or stdout.

use std::collections::VecDeque;

use crate::md::safe::{SafeText, sanitize};
use crate::ui::grid::{Grid, Style};
use crate::ui::layout::Rect;
use crate::ui::wrap::{Line, Span, draw_line, wrap_each};

pub const MAX_ENTRIES: usize = 10_000;

pub struct Scrollback {
    entries: VecDeque<(SafeText, Style)>,
}

impl Default for Scrollback {
    fn default() -> Self {
        Self::new()
    }
}

impl Scrollback {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }

    /// Adds `text` (sanitized here) as one entry; it may hold line breaks.
    pub fn push(&mut self, text: &str, style: Style) {
        if self.entries.len() == MAX_ENTRIES {
            // SafeText wipes itself when dropped.
            self.entries.pop_front();
        }
        self.entries.push_back((sanitize(text), style));
    }

    /// Drops every entry (each wipes itself).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Draws the newest lines that fit in `area`, `scroll` lines up from
    /// the end, from its top row down (decision 0049).
    /// Returns the scroll actually used (never past the oldest line) and
    /// the rows drawn, so the prompt can follow them.
    pub fn draw(&self, grid: &mut Grid, area: Rect, scroll: usize) -> (usize, u16) {
        if area.is_empty() {
            return (0, 0);
        }
        let wanted = usize::from(area.height) + scroll;
        // The last `wanted` wrapped lines, newest entry first, each
        // entry's lines in order.
        let mut blocks: Vec<(usize, Vec<Line>)> = Vec::new();
        let mut collected = 0;
        for (index, (text, style)) in self.entries.iter().enumerate().rev() {
            let mut lines = Vec::new();
            let spans = text.spans(*style);
            wrap_each(&spans, area.width, |line| lines.push(line.to_vec()));
            collected += lines.len();
            blocks.push((index, lines));
            if collected >= wanted {
                break;
            }
        }
        let scroll = scroll.min(collected.saturating_sub(usize::from(area.height)));
        // Oldest first, then skip what lies above the window.
        let all: Vec<(usize, &Line)> = blocks
            .iter()
            .rev()
            .flat_map(|(index, lines)| lines.iter().map(move |l| (*index, l)))
            .collect();
        let end = all.len() - scroll;
        let start = end.saturating_sub(usize::from(area.height));
        let shown = &all[start..end];
        for (row, (index, line)) in (area.y..).zip(shown) {
            let (text, style) = &self.entries[*index];
            let spans: Vec<Span<'_>> = text.spans(*style);
            draw_line(grid, area.x, row, area.width, line, &spans);
        }
        // At most `area.height` lines are shown.
        let drawn = u16::try_from(shown.len()).unwrap_or(area.height);
        (scroll, drawn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(back: &Scrollback, width: u16, height: u16, scroll: usize) -> (Vec<String>, usize) {
        let mut grid = Grid::new(width, height);
        let area = grid.area();
        let (used, _) = back.draw(&mut grid, area, scroll);
        let rows = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| grid.cell(x, y).unwrap().symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect();
        (rows, used)
    }

    #[test]
    fn newest_lines_and_scrolling() {
        let mut back = Scrollback::new();
        for line in ["one", "two", "three four five"] {
            back.push(line, Style::PLAIN);
        }
        assert_eq!(rows(&back, 10, 3, 0).0, ["two", "three four", "five"]);
        assert_eq!(rows(&back, 10, 3, 1).0, ["one", "two", "three four"]);
        // Past the oldest line, scroll stops there.
        assert_eq!(
            rows(&back, 10, 3, 9),
            (vec!["one".into(), "two".into(), "three four".into()], 1)
        );
        // Few lines start at the top, and the rows drawn are counted
        // (a wrapped entry counts each of its rows).
        let mut short = Scrollback::new();
        short.push("only", Style::PLAIN);
        assert_eq!(rows(&short, 10, 3, 0).0, ["only", "", ""]);
        let mut grid = Grid::new(10, 3);
        let area = grid.area();
        assert_eq!(short.draw(&mut grid, area, 0), (0, 1));
        short.push("three four five", Style::PLAIN);
        assert_eq!(short.draw(&mut grid, area, 0), (0, 3));
        assert_eq!(Scrollback::new().draw(&mut grid, area, 0), (0, 0));
    }

    #[test]
    fn entries_are_sanitized_and_bounded() {
        let mut back = Scrollback::new();
        back.push("a\u{1b}[2Jb", Style::PLAIN);
        assert_eq!(rows(&back, 20, 1, 0).0, ["a<U+001B>[2Jb"]);
        for i in 0..MAX_ENTRIES + 5 {
            back.push(&i.to_string(), Style::PLAIN);
        }
        assert_eq!(back.len(), MAX_ENTRIES);
        assert_eq!(rows(&back, 10, 1, 0).0, [(MAX_ENTRIES + 4).to_string()]);
        back.clear();
        assert_eq!(back.len(), 0);
    }
}
