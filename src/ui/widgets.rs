//! The few widgets the viewer needs (decision 0031): a border with a
//! title, a list with a selection, a scrolling text pane and a status
//! line. Each draws only inside the area it is given, through
//! `Grid::put_str`, so every text is checked by the grid and clipped.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the viewer loop (Phase 3, V6)")
)]

use super::grid::{Grid, Mods, Style, display_width};
use super::layout::Rect;
use super::wrap::{Line, Span, draw_line};

/// Draws a box around `area` with `title` in its top edge, and returns the
/// area inside it. An area too small for a box is returned unchanged.
pub fn border(grid: &mut Grid, area: Rect, title: &str, style: Style) -> Rect {
    if area.width < 2 || area.height < 2 {
        return area;
    }
    let (right, bottom) = (area.right() - 1, area.bottom() - 1);
    for x in area.x + 1..right {
        grid.put_str(x, area.y, "─", style, 1);
        grid.put_str(x, bottom, "─", style, 1);
    }
    for y in area.y + 1..bottom {
        grid.put_str(area.x, y, "│", style, 1);
        grid.put_str(right, y, "│", style, 1);
    }
    for (x, y, corner) in [
        (area.x, area.y, "┌"),
        (right, area.y, "┐"),
        (area.x, bottom, "└"),
        (right, bottom, "┘"),
    ] {
        grid.put_str(x, y, corner, style, 1);
    }
    // " title " after the corner, never over the right corner.
    if !title.is_empty() && area.width > 4 {
        let room = area.width - 4;
        let x = area.x + 1;
        grid.put_str(x, area.y, " ", style, 1);
        let used = grid.put_str(x + 1, area.y, title, style.mods(Mods::BOLD), room);
        grid.put_str(x + 1 + used, area.y, " ", style, 1);
    }
    area.inner(1)
}

/// The first item to show so that `selected` is visible in `rows` rows,
/// moving as little as possible from `offset`.
pub fn list_offset(offset: usize, selected: usize, rows: u16) -> usize {
    let rows = usize::from(rows.max(1));
    if selected < offset {
        selected
    } else if selected >= offset + rows {
        selected + 1 - rows
    } else {
        offset
    }
}

/// Draws `items` one per row from `offset`, the selected one reversed
/// across the whole row. Items are cut at the right edge.
pub fn list(
    grid: &mut Grid,
    area: Rect,
    items: &[&str],
    offset: usize,
    selected: Option<usize>,
    style: Style,
) {
    for (row, (index, item)) in (area.y..area.bottom()).zip(items.iter().enumerate().skip(offset)) {
        let style = if selected == Some(index) {
            style.mods(style.mods | Mods::REVERSED)
        } else {
            style
        };
        grid.fill(Rect::new(area.x, row, area.width, 1), style);
        grid.put_str(area.x, row, item, style, area.width);
    }
}

/// The largest useful scroll position for `lines` lines in `rows` rows.
pub fn max_scroll(lines: usize, rows: u16) -> usize {
    lines.saturating_sub(usize::from(rows))
}

/// Draws wrapped `lines` from line `scroll` down, one per row.
pub fn text_pane(grid: &mut Grid, area: Rect, lines: &[Line], spans: &[Span], scroll: usize) {
    for (row, line) in (area.y..area.bottom()).zip(lines.iter().skip(scroll)) {
        draw_line(grid, area.x, row, area.width, line, spans);
    }
}

/// Fills the first row of `area` with `style`, `left` at the start and
/// `right` at the end. When both do not fit, `right` wins: it holds the
/// state (locked, unsaved) that must stay visible.
pub fn status_line(grid: &mut Grid, area: Rect, left: &str, right: &str, style: Style) {
    if area.is_empty() {
        return;
    }
    let row = Rect::new(area.x, area.y, area.width, 1);
    grid.fill(row, style);
    let right_width = u16::try_from(display_width(right))
        .unwrap_or(u16::MAX)
        .min(area.width);
    let right_x = area.right() - right_width;
    grid.put_str(right_x, area.y, right, style, right_width);
    let gap = u16::from(right_width > 0);
    let room = (right_x - area.x).saturating_sub(gap);
    grid.put_str(area.x, area.y, left, style, room);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::grid::Color;
    use crate::ui::wrap::wrap;

    fn rows(grid: &Grid) -> Vec<String> {
        (0..grid.height())
            .map(|y| {
                (0..grid.width())
                    .map(|x| grid.cell(x, y).unwrap().symbol())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn border_draws_a_box_with_a_title() {
        let mut grid = Grid::new(10, 3);
        let area = grid.area();
        let inner = border(&mut grid, area, "Notes", Style::PLAIN);
        assert_eq!(inner, Rect::new(1, 1, 8, 1));
        assert_eq!(rows(&grid), ["┌ Notes ─┐", "│        │", "└────────┘"]);
    }

    #[test]
    fn border_title_is_cut_before_the_corner() {
        let mut grid = Grid::new(8, 2);
        let area = grid.area();
        border(&mut grid, area, "A long title", Style::PLAIN);
        assert_eq!(rows(&grid)[0], "┌ A lo ┐");
        let mut tiny = Grid::new(1, 1);
        let area = tiny.area();
        assert_eq!(border(&mut tiny, area, "x", Style::PLAIN), area);
    }

    #[test]
    fn list_offset_keeps_the_selection_visible() {
        assert_eq!(list_offset(0, 2, 5), 0);
        assert_eq!(list_offset(0, 7, 5), 3);
        assert_eq!(list_offset(4, 1, 5), 1);
        assert_eq!(list_offset(3, 5, 5), 3);
        assert_eq!(list_offset(0, 3, 0), 3);
    }

    #[test]
    fn list_draws_from_the_offset_and_marks_the_selection() {
        let mut grid = Grid::new(6, 2);
        let items = ["alpha", "beta", "gamma toolong"];
        let area = grid.area();
        list(&mut grid, area, &items, 1, Some(2), Style::PLAIN);
        assert_eq!(rows(&grid), ["beta  ", "gamma "]);
        let reversed = |x, y| {
            grid.cell(x, y)
                .unwrap()
                .style()
                .mods
                .contains(Mods::REVERSED)
        };
        assert!(!reversed(0, 0));
        assert!((0..6).all(|x| reversed(x, 1)));
    }

    #[test]
    fn text_pane_scrolls() {
        let spans = [Span {
            text: "one two three four",
            style: Style::PLAIN,
        }];
        let lines = wrap(&spans, 5);
        assert_eq!(lines.len(), 4);
        let mut grid = Grid::new(5, 2);
        let area = grid.area();
        text_pane(&mut grid, area, &lines, &spans, 1);
        assert_eq!(rows(&grid), ["two  ", "three"]);
        assert_eq!(max_scroll(lines.len(), 2), 2);
        assert_eq!(max_scroll(1, 5), 0);
    }

    #[test]
    fn status_line_keeps_the_right_side() {
        let style = Style::PLAIN.bg(Color::Blue);
        let mut grid = Grid::new(16, 1);
        let area = grid.area();
        status_line(&mut grid, area, "diary", "unsaved", style);
        assert_eq!(rows(&grid), ["diary    unsaved"]);
        assert!((0..16).all(|x| grid.cell(x, 0).unwrap().style() == style));
        let mut narrow = Grid::new(10, 1);
        let area = narrow.area();
        status_line(&mut narrow, area, "a long name", "unsaved", style);
        assert_eq!(rows(&narrow), ["a  unsaved"]);
        let mut tiny = Grid::new(4, 1);
        let area = tiny.area();
        status_line(&mut tiny, area, "name", "unsaved", style);
        assert_eq!(rows(&tiny), ["unsa"]);
    }

    #[test]
    fn widgets_never_draw_outside_their_area() {
        // Draw everything into a 4x3 area in the middle of a 10x7 grid,
        // with hostile and wide text; the frame around it stays blank.
        let hostile = "日本\u{1b}[2J\u{202E}語 long text that wraps";
        let spans = [Span {
            text: hostile,
            style: Style::PLAIN,
        }];
        let area = Rect::new(3, 2, 4, 3);
        for draw in 0..4 {
            let mut grid = Grid::new(10, 7);
            match draw {
                0 => {
                    border(&mut grid, area, hostile, Style::PLAIN);
                }
                1 => list(&mut grid, area, &[hostile; 9], 2, Some(3), Style::PLAIN),
                2 => text_pane(&mut grid, area, &wrap(&spans, 4), &spans, 0),
                _ => status_line(&mut grid, area, hostile, hostile, Style::PLAIN),
            }
            for y in 0..7 {
                for x in 0..10 {
                    let inside = (3..7).contains(&x) && (2..5).contains(&y);
                    let cell = grid.cell(x, y).unwrap();
                    if !inside {
                        assert_eq!(cell.symbol(), " ", "widget {draw} at ({x}, {y})");
                    }
                    assert!(!cell.symbol().contains(crate::ui::grid::is_forbidden));
                }
            }
        }
    }
}
