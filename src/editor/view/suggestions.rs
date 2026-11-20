//! The list of link suggestions (`suggest`; its own file since step
//! RF5), drawn over the frame after it (`View::draw_suggestions`): a gray
//! box of at most `suggest::ROWS` names under the cursor's row, or above
//! it when there is more room there, starting at the cursor's column and
//! moved left to fit, at most half the screen wide. Folders are dimmed,
//! the highlight is reversed, a name too long ends in `…`. Names go
//! through `Grid::put_str` like the text, so the grid checks every
//! character.
//!
//! Where the list was drawn is kept (`View::list`) only so the mouse
//! finds the entry under a cell (`View::suggestion_at`); the next frame
//! without the list forgets it. These are `View`'s methods, as they use
//! its last frame: the cursor's cell and the text's area.

use super::View;
use crate::editor::Editor;
use crate::editor::suggest::ROWS;
use crate::notes::Notes;
use crate::ui::grid::{Color, Grid, Mods, Style, display_width};
use crate::ui::layout::Rect;

impl View {
    /// Draws the editor's suggestion list over the frame `draw` just made
    /// (see the module comment), its names from `notes`. Nothing when the
    /// list is closed, the cursor was not drawn, or there is no room.
    pub fn draw_suggestions(&mut self, grid: &mut Grid, editor: &Editor, notes: &Notes) {
        /// The suggestion list's rows: the gray of code blocks in the viewer.
        const LIST: Style = Style::PLAIN.bg(Color::Indexed(236));
        const LIST_FOLDER: Style = LIST.mods(Mods::DIM);
        const LIST_HIGHLIGHT: Style = LIST.mods(Mods::REVERSED);
        const LIST_HIGHLIGHT_FOLDER: Style = LIST.mods(Mods::REVERSED.union(Mods::DIM));
        /// The narrowest list: a space each side and two columns, so a
        /// name cut short still shows a character and `…`.
        const LIST_MIN_WIDTH: u16 = 4;
        let (Some(list), Some(area), Some((cursor_x, cursor_y))) =
            (editor.suggestions(), self.area, self.cursor)
        else {
            return;
        };
        let screen = grid.area();
        // Rows below the cursor's row and above it, inside the text area.
        let below = area.bottom().saturating_sub(cursor_y + 1);
        let above = cursor_y.saturating_sub(area.y);
        let wanted = u16::try_from(list.entries.len().saturating_sub(list.top).min(ROWS))
            .unwrap_or(u16::MAX);
        let (rows, y) = if below >= wanted || below >= above {
            (wanted.min(below), cursor_y + 1)
        } else {
            (wanted.min(above), cursor_y - wanted.min(above))
        };
        if rows == 0 {
            return;
        }
        // The first row shown keeps the highlight in view when fewer
        // than `ROWS` rows fit.
        let first = list
            .top
            .max((list.highlight + 1).saturating_sub(usize::from(rows)));
        // Each row's text from the editor: a dimmed part (a folder, `# `),
        // the text, and a dimmed end (a block's ` ^id`). An entry without one stays a blank row, so rows
        // and `suggestion_at` agree.
        let shown = (first..list.entries.len())
            .take(usize::from(rows))
            .map(|index| {
                (
                    index,
                    editor
                        .suggestion_label(index, notes)
                        .unwrap_or(("", "", "")),
                )
            });
        let longest = shown
            .clone()
            .map(|(_, (dim, text, end))| {
                display_width(dim) + display_width(text) + display_width(end)
            })
            .max()
            .unwrap_or(0);
        let width = u16::try_from(longest.saturating_add(2))
            .unwrap_or(u16::MAX)
            .max(LIST_MIN_WIDTH)
            .min(screen.width / 2);
        if width < LIST_MIN_WIDTH {
            return;
        }
        let x = cursor_x.min(screen.right() - width);
        let mut drawn = 0;
        for (row, (index, (folder, title, end))) in (y..).zip(shown) {
            drawn += 1;
            let (plain, folder_style) = if index == list.highlight {
                (LIST_HIGHLIGHT, LIST_HIGHLIGHT_FOLDER)
            } else {
                (LIST, LIST_FOLDER)
            };
            grid.fill(Rect::new(x, row, width, 1), plain);
            let room = width - 2;
            let fits = display_width(folder) + display_width(title) + display_width(end)
                <= usize::from(room);
            // A name too long keeps its last column for `…`.
            let mut left = if fits { room } else { room - 1 };
            let mut column = x + 1;
            for (part, style) in [(folder, folder_style), (title, plain), (end, folder_style)] {
                let used = grid.put_str(column, row, part, style, left);
                column += used;
                left -= used;
            }
            if !fits {
                grid.put_str(x + width - 2, row, "…", plain, 1);
            }
        }
        if drawn > 0 {
            self.list = Some((Rect::new(x, y, width, drawn), first));
        }
    }

    /// The suggestion list's entry (an index into `List::notes`) drawn at
    /// the cell (`column`, `row`) in the last frame, if the list was drawn
    /// there (decision 0073's mouse).
    pub fn suggestion_at(&self, column: u16, row: u16) -> Option<usize> {
        let (area, first) = self.list?;
        let inside =
            (area.x..area.right()).contains(&column) && (area.y..area.bottom()).contains(&row);
        inside.then(|| first + usize::from(row - area.y))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{cursor_cell, drawn, rows};
    use super::*;
    use crate::shell::keys::Key;
    use crate::ui::clipboard::Clipboard;
    use proptest::prelude::*;

    /// Notes named `names` (`folder/title`), and an editor on `text`
    /// with the cursor at its end that has typed `typed` through
    /// `key_in`, so the suggestion list is open.
    fn listing(names: &[&str], text: &str, typed: &str) -> (Editor, crate::notes::Notes) {
        listing_with(names, "", text, typed)
    }

    /// `listing` with `first_body` as the first note's text.
    fn listing_with(
        names: &[&str],
        first_body: &str,
        text: &str,
        typed: &str,
    ) -> (Editor, crate::notes::Notes) {
        use crate::notes::tests::{note, notes};
        let n = notes(
            names
                .iter()
                .zip(1u8..)
                .map(|(name, id)| {
                    let (folder, title) = name.rsplit_once('/').unwrap_or(("", name));
                    note(id, folder, title, if id == 1 { first_body } else { "" })
                })
                .collect(),
        );
        let mut ed = Editor::new(text).unwrap();
        let mut clip = Clipboard::default();
        ed.key(Key::Ctrl('a'), &mut clip);
        ed.key(Key::Right, &mut clip);
        for c in typed.chars() {
            ed.key_in(Key::Char(c), &mut clip, &n);
        }
        (ed, n)
    }

    /// The frame with the list drawn over it.
    fn with_list(ed: &mut Editor, n: &crate::notes::Notes, width: u16, height: u16) -> Grid {
        let mut view = View::default();
        let mut grid = drawn(ed, &mut view, width, height);
        view.draw_suggestions(&mut grid, ed, n);
        grid
    }

    /// The cells of row `y` in the list's gray, as (first column, text).
    fn gray_run(grid: &Grid, y: u16) -> Option<(u16, String)> {
        let gray = |x: u16| grid.cell(x, y).unwrap().style().bg == Color::Indexed(236);
        let first = (0..grid.width()).find(|&x| gray(x))?;
        let text = (first..grid.width())
            .take_while(|&x| gray(x))
            .map(|x| grid.cell(x, y).unwrap().symbol())
            .collect();
        Some((first, text))
    }

    #[test]
    fn list_opens_under_the_cursor() {
        let (mut ed, n) = listing(&["plan", "work/plan", "Restic Backup"], "", "[[");
        let grid = with_list(&mut ed, &n, 40, 10);
        // `1 [[]]`: the cursor is after `[[`, column 4 on row 1.
        assert_eq!(rows(&grid)[1], "1 [[]]");
        assert_eq!(gray_run(&grid, 2), Some((4, " plan          ".into())));
        assert_eq!(gray_run(&grid, 3), Some((4, " Restic Backup ".into())));
        assert_eq!(gray_run(&grid, 4), Some((4, " work/plan     ".into())));
        assert_eq!(gray_run(&grid, 5), None);
        // The first row is highlighted; folders are dimmed.
        let style = |x, y| grid.cell(x, y).unwrap().style().mods;
        assert!(style(5, 2).contains(Mods::REVERSED));
        assert!(!style(5, 3).contains(Mods::REVERSED));
        assert!(style(5, 4).contains(Mods::DIM));
        assert!(!style(10, 4).contains(Mods::DIM));
        // The text is untouched; the key line names the list.
        assert!(rows(&grid)[9].starts_with("Notes · Up/Down"));
    }

    /// The key line names the open list's kind, and is the usual one
    /// again once it closes.
    #[test]
    fn key_line_names_the_list() {
        let body = "# Plan\n\nA paragraph\n";
        let line = |typed: &str| {
            let (mut ed, n) = listing_with(&["plan"], body, "", typed);
            let grid = with_list(&mut ed, &n, 100, 12);
            rows(&grid)[11].clone()
        };
        assert!(line("[[pl").starts_with("Notes · "));
        assert!(line("[[plan#").starts_with("Headings · "));
        assert!(line("[[plan#^").starts_with("Blocks · "));
        assert!(line("[[plan#^").contains("an ^id is added if missing"));
        assert!(line("[[zz").starts_with("Ctrl+S save"), "no list");
    }

    #[test]
    fn list_opens_above_when_there_is_more_room() {
        let text = "\n".repeat(7);
        let (mut ed, n) = listing(&["a", "b", "c"], &text, "[[");
        // Rows 1 to 8 hold text, the cursor on row 8: no room below.
        let grid = with_list(&mut ed, &n, 30, 10);
        // Short names: the list is padded to its narrowest, 4 columns.
        assert_eq!(gray_run(&grid, 5).map(|r| r.1), Some(" a  ".into()));
        assert_eq!(gray_run(&grid, 7).map(|r| r.1), Some(" c  ".into()));
        assert_eq!(gray_run(&grid, 8), None);
        assert_eq!(gray_run(&grid, 9), None);
    }

    #[test]
    fn list_moves_left_to_fit_and_cuts_long_names() {
        let long = "a".repeat(30);
        let (mut ed, n) = listing(&[&long, "b"], &"x".repeat(25), "[[");
        // The cursor is at column 29; the list is half of 40 columns wide,
        // so it ends at the right edge.
        let grid = with_list(&mut ed, &n, 40, 6);
        let (x, row) = gray_run(&grid, 2).unwrap();
        assert_eq!(x, 20);
        assert_eq!(row, format!(" {}… ", "a".repeat(17)));
        assert_eq!(
            gray_run(&grid, 3).unwrap().1,
            format!(" b{} ", " ".repeat(17))
        );
    }

    #[test]
    fn wide_and_combining_names_cut_cleanly() {
        let (mut ed, n) = listing(&["日本語のノート名", "e\u{301}te\u{301}"], "", "[[");
        let grid = with_list(&mut ed, &n, 20, 6);
        // No gutter (too narrow); 10 columns: a space, 7 for the name,
        // `…`, a space. `é` (with a combining accent) sorts first; a wide
        // character that does not fit whole becomes a space.
        assert_eq!(
            gray_run(&grid, 2).unwrap(),
            (2, " e\u{301}te\u{301}      ".into())
        );
        assert_eq!(gray_run(&grid, 3).unwrap(), (2, " 日本語 … ".into()));
    }

    #[test]
    fn fewer_rows_keep_the_highlight_shown() {
        let names: Vec<String> = (0..12).map(|i| format!("n{i:02}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (mut ed, n) = listing(&names, "", "[[");
        let mut clip = Clipboard::default();
        for _ in 0..5 {
            ed.key_in(Key::Down, &mut clip, &n);
        }
        // Three rows of text area, the cursor on the first: two below.
        let grid = with_list(&mut ed, &n, 30, 5);
        assert_eq!(gray_run(&grid, 2).unwrap().1, " n04 ");
        assert_eq!(gray_run(&grid, 3).unwrap().1, " n05 ");
        assert!(
            grid.cell(6, 3)
                .unwrap()
                .style()
                .mods
                .contains(Mods::REVERSED)
        );
    }

    #[test]
    fn suggestion_at_maps_cells_to_entries() {
        let names: Vec<String> = (0..12).map(|i| format!("n{i:02}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (mut ed, n) = listing(&names, "", "[[");
        let mut view = View::default();
        let mut grid = drawn(&mut ed, &mut view, 40, 12);
        // Before the list is drawn, and with it drawn: rows 2 to 9 from
        // column 4, 5 columns wide.
        assert_eq!(view.suggestion_at(4, 2), None);
        view.draw_suggestions(&mut grid, &ed, &n);
        assert_eq!(view.suggestion_at(4, 2), Some(0));
        assert_eq!(view.suggestion_at(8, 9), Some(7));
        for (x, y) in [(3, 2), (9, 2), (4, 1), (4, 10)] {
            assert_eq!(view.suggestion_at(x, y), None, "({x}, {y})");
        }
        // Scrolled: the first row shows entry 4.
        let mut clip = Clipboard::default();
        for _ in 0..11 {
            ed.key_in(Key::Down, &mut clip, &n);
        }
        let mut grid = drawn(&mut ed, &mut view, 40, 12);
        view.draw_suggestions(&mut grid, &ed, &n);
        assert_eq!(view.suggestion_at(4, 2), Some(4));
        assert_eq!(view.suggestion_at(4, 9), Some(11));
        // A frame without the list forgets it.
        ed.key_in(Key::Esc, &mut clip, &n);
        drawn(&mut ed, &mut view, 40, 12);
        assert_eq!(view.suggestion_at(4, 2), None);
    }

    #[test]
    fn no_list_without_room() {
        let (mut ed, n) = listing(&["plan"], "", "[[");
        for (width, height) in [(1, 1), (1, 5), (7, 5), (40, 2), (40, 3)] {
            let grid = with_list(&mut ed, &n, width, height);
            for y in 0..height {
                assert_eq!(gray_run(&grid, y), None, "{width}x{height}");
            }
        }
        // A closed list draws nothing.
        ed.key_in(Key::Esc, &mut Clipboard::default(), &n);
        let grid = with_list(&mut ed, &n, 40, 10);
        assert_eq!(gray_run(&grid, 2), None);
    }

    proptest! {
        /// Any screen size, names (wide, combining, emoji) and highlight,
        /// for a list of notes or of the first note's headings (the names
        /// again, as `# NAME` lines): the list stays off the title and key
        /// lines, never covers the cursor, shows the highlight when drawn,
        /// and no cell holds a control character.
        #[test]
        fn suggestion_list_stays_in_bounds(
            width in 1u16..50,
            height in 1u16..14,
            names in prop::collection::btree_set("[a-c日é😀]{1,3}(\u{301})?[a-z]{0,20}", 1..15),
            lines in 0usize..12,
            downs in 0usize..20,
            kind in 0u8..3,
            hostile in prop::collection::vec(
                "[a-c\u{0}-\u{1f}\u{7f}-\u{9f}\u{202e}\u{2066}\u{200b}日é😀\t]{1,12}",
                1..8,
            ),
        ) {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let text = "x\n".repeat(lines);
            // Its headings: the names; its blocks: hostile paragraphs.
            let mut body: String = names.iter().map(|name| format!("# {name}\n\n")).collect();
            body.push_str(&hostile.join("\n\n"));
            let typed = match kind {
                0 => "[[".to_owned(),
                1 => format!("[[{}#", names[0]),
                _ => format!("[[{}#^", names[0]),
            };
            let (mut ed, n) = listing_with(&names, &body, &text, &typed);
            let mut clip = Clipboard::default();
            for _ in 0..downs {
                ed.key_in(Key::Down, &mut clip, &n);
            }
            let mut view = View::default();
            let mut grid = drawn(&mut ed, &mut view, width, height);
            let cursor = cursor_cell(&grid);
            view.draw_suggestions(&mut grid, &ed, &n);
            prop_assert_eq!(cursor_cell(&grid), cursor);
            let mut highlighted = false;
            let mut drawn_any = false;
            use crate::editor::suggest::Source;
            let Some(list) = ed.suggestions() else {
                // Hostile paragraphs can all be code or HTML: no blocks.
                prop_assert_eq!(kind, 2);
                return Ok(());
            };
            let expected = match kind {
                0 => matches!(list.source, Source::Notes),
                1 => matches!(list.source, Source::Headings(_)),
                _ => matches!(list.source, Source::Blocks(_)),
            };
            prop_assert!(expected);
            for y in 0..height {
                for x in 0..width {
                    let cell = grid.cell(x, y).unwrap();
                    prop_assert!(!cell.symbol().chars().any(char::is_control));
                    prop_assert!(!cell.symbol().chars().any(crate::ui::grid::is_forbidden));
                    let gray = cell.style().bg == Color::Indexed(236);
                    // The mouse finds an entry exactly where one is drawn,
                    // and the highlighted entry is the reversed row.
                    let entry = view.suggestion_at(x, y);
                    prop_assert_eq!(gray, entry.is_some());
                    if let Some(entry) = entry {
                        drawn_any = true;
                        prop_assert!(entry < list.entries.len());
                        prop_assert!(y > 0 && y + 1 < height, "list on row {y}");
                        let reversed = cell.style().mods.contains(Mods::REVERSED);
                        prop_assert_eq!(reversed, entry == list.highlight);
                        highlighted |= reversed;
                    }
                }
            }
            prop_assert_eq!(drawn_any, highlighted);
        }
    }
}
