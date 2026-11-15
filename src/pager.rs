//! `view NAME`: a note, rendered full screen (docs/decisions/0033-shell.md).
//!
//! The title on the top row, the note below, a key line at the bottom.
//! A `>` in the gutter on the left marks the cursor's line; the cursor
//! changes nothing, it only points. j/k and
//! the arrows move it by a line, Space and PgDn/PgUp by a screen, g/G or
//! Home/End to the ends, and the screen scrolls to keep it in view. n and
//! N select the next and previous `[[link]]` and move the cursor to it,
//! Enter follows it (to a note, a heading or a `^block`), Backspace goes
//! back. Tab on a heading folds its section (hides the lines up to the
//! next heading of its level or higher) or unfolds it; a folded heading
//! ends in ` …`. Folds last while the note is shown and are never saved;
//! a jump or a link selected inside a fold opens it. b lists the notes
//! linking here. y copies the note line the cursor is on, as written
//! (Markdown included), into Crystal's own clipboard, Y the whole note
//! (docs/decisions/0041-internal-clipboard-only.md); the editor's Ctrl+V
//! pastes it. q or Esc
//! returns to the shell. Ctrl+E asks to edit the note shown
//! (docs/decisions/0045-simple-editor.md).
//!
//! From the editor, Ctrl+E opens a preview: the editor's current text,
//! unsaved changes included, rendered in place of the saved note and
//! marked "preview" on the top row. Notes reached by following a link
//! show their saved text. Links resolve against the saved notes.
//!
//! Like the shell, it is terminal-free: keys in, frames out. A page is
//! rendered for the current width and again after a resize.

use zeroize::Zeroizing;

use crate::md::render::{Page, Section, kind_of, render};
use crate::notes::{Fragment, Notes};
use crate::shell::keys::Key;
use crate::ui::grid::{Color, Grid, Mods, Style};
use crate::ui::layout::Rect;
use crate::ui::widgets::status_line;

const TITLE: Style = Style::PLAIN.mods(Mods::REVERSED.union(Mods::BOLD));
const HINT: Style = Style::PLAIN.mods(Mods::DIM);
const NOTICE: Style = Style::PLAIN.fg(Color::Yellow);
const SELECTED: Style = Style::PLAIN.mods(Mods::REVERSED);
/// The cursor's mark, in the gutter left of the note.
const MARKER: Style = Style::PLAIN.mods(Mods::BOLD);
/// Columns left of the note without line numbers: the marker and a space.
const GUTTER: u16 = 2;
/// Line numbers are shown only if this many columns stay for the note.
const MIN_TEXT: u16 = 20;
const HINTS: &str = "q shell · j/k move · Tab fold · n/N links · Enter follow · \
                     Backspace previous · b backlinks · y/Y copy line/note · Ctrl+E edit";

/// Line `number` (from 1) of `text`, as the editor counts lines (`\n`
/// ends one, `\r\n` too), without its line break; empty past the end.
pub fn line_in(text: &str, number: u32) -> &str {
    let index = usize::try_from(number.saturating_sub(1)).unwrap_or(usize::MAX);
    let line = text.split('\n').nth(index).unwrap_or("");
    line.strip_suffix('\r').unwrap_or(line)
}

/// After a key: stay, or go back to the shell.
#[derive(Debug, PartialEq, Eq)]
pub enum PagerAction {
    Stay,
    Close,
    /// Copy this note into Crystal's own clipboard.
    Copy(usize),
    /// Copy this line (from 1) of this note into Crystal's own clipboard.
    CopyLine(usize, u32),
    /// Ctrl+E: edit this note (or go back to the editor it previews).
    Edit(usize),
}

/// Where to scroll once the page is rendered.
enum Jump {
    Heading(Zeroizing<String>),
    Block(Zeroizing<String>),
}

/// The list `b` shows: the notes linking here, and which is selected.
struct Backlinks {
    notes: Vec<usize>,
    selected: usize,
}

pub struct Pager {
    note: usize,
    /// Notes left by following links, with their scroll and cursor
    /// positions.
    history: Vec<(usize, usize, usize)>,
    page: Option<Page>,
    /// The note area's width the page was rendered for, gutter included.
    width: u16,
    /// Digits of the rendered note's last line number.
    digits: u16,
    scroll: usize,
    /// The line the cursor is on; always a shown line, on the screen
    /// once drawn. `scroll` is a shown line too.
    cursor: usize,
    /// Folded sections, as indices into the page's sections.
    folded: Vec<usize>,
    /// Rows the note had when last drawn.
    rows: usize,
    selected: Option<usize>,
    jump: Option<Jump>,
    backlinks: Option<Backlinks>,
    notice: Option<&'static str>,
    /// The note whose text comes from the editor, not from the vault.
    preview: Option<usize>,
}

impl Pager {
    pub fn open(note: usize) -> Self {
        Self {
            note,
            history: Vec::new(),
            page: None,
            width: 0,
            digits: 1,
            scroll: 0,
            cursor: 0,
            folded: Vec::new(),
            rows: 1,
            selected: None,
            jump: None,
            backlinks: None,
            notice: None,
            preview: None,
        }
    }

    /// From the editor: shows `note` with the text `draw` is given, the
    /// editor's, instead of the saved one.
    pub fn preview(note: usize) -> Self {
        Self {
            preview: Some(note),
            ..Self::open(note)
        }
    }

    fn lines(&self) -> usize {
        self.page.as_ref().map_or(0, Page::line_count)
    }

    /// The folded sections that hide `line`: those it is inside, below
    /// their heading.
    fn hiding(&self, line: usize) -> impl Iterator<Item = Section> + '_ {
        let sections = self.page.as_ref().map_or(&[][..], Page::sections);
        self.folded
            .iter()
            .filter_map(|&i| sections.get(i).copied())
            .filter(move |s| s.line < line && line < s.end)
    }

    /// The first shown line after `line`: past every fold in the way.
    fn next_shown(&self, line: usize) -> Option<usize> {
        let mut next = line + 1;
        while next < self.lines() {
            match self.hiding(next).map(|s| s.end).max() {
                Some(end) => next = end,
                None => return Some(next),
            }
        }
        None
    }

    /// The last shown line before `line`: a folded heading stands for
    /// the lines it hides.
    fn prev_shown(&self, line: usize) -> Option<usize> {
        let mut prev = line.checked_sub(1)?;
        while let Some(heading) = self.hiding(prev).map(|s| s.line).min() {
            prev = heading;
        }
        Some(prev)
    }

    /// `count` shown lines on from `line` (or back), stopping at the ends.
    fn step(&self, mut line: usize, count: usize, forward: bool) -> usize {
        for _ in 0..count {
            let next = if forward {
                self.next_shown(line)
            } else {
                self.prev_shown(line)
            };
            match next {
                Some(next) => line = next,
                None => break,
            }
        }
        line
    }

    /// `line` if it is shown, else the folded heading hiding it.
    fn shown_at(&self, line: usize) -> usize {
        match self.hiding(line).map(|s| s.line).min() {
            Some(heading) => self.shown_at(heading),
            None => line,
        }
    }

    /// The last shown line (0 for an empty page).
    fn last_shown(&self) -> usize {
        self.prev_shown(self.lines()).unwrap_or(0)
    }

    /// The highest scroll: the last screen is full, if the page is long
    /// enough to fill it.
    fn max_scroll(&self) -> usize {
        self.step(self.last_shown(), self.rows - 1, false)
    }

    pub fn key(&mut self, key: Key, notes: &Notes) -> PagerAction {
        let key = key.without_shift();
        self.notice = None;
        if let Some(list) = &mut self.backlinks {
            match key {
                Key::Up | Key::Char('k') => list.selected = list.selected.saturating_sub(1),
                Key::Down | Key::Char('j') => {
                    list.selected = (list.selected + 1).min(list.notes.len().saturating_sub(1));
                }
                Key::Enter => {
                    if let Some(&note) = list.notes.get(list.selected) {
                        self.backlinks = None;
                        self.go(note, None);
                    }
                }
                Key::Esc | Key::Char('b' | 'q') => self.backlinks = None,
                _ => {}
            }
            return PagerAction::Stay;
        }
        let page = self.rows.max(1);
        match key {
            Key::Char('q') | Key::Esc => return PagerAction::Close,
            Key::Char('y') => match self.page.as_ref().and_then(|p| p.note_line(self.cursor)) {
                Some(line) => return PagerAction::CopyLine(self.note, line),
                None => self.notice = Some("No line here to copy."),
            },
            Key::Char('Y') => return PagerAction::Copy(self.note),
            Key::Ctrl('e') => return PagerAction::Edit(self.note),
            Key::Char('j') | Key::Down => self.move_cursor(self.step(self.cursor, 1, true)),
            Key::Char('k') | Key::Up => self.move_cursor(self.step(self.cursor, 1, false)),
            // A screen at a time: the page and the cursor move together.
            Key::Char(' ') | Key::PageDown => {
                self.scroll = self.step(self.scroll, page, true).min(self.max_scroll());
                self.move_cursor(self.step(self.cursor, page, true));
            }
            Key::PageUp => {
                self.scroll = self.step(self.scroll, page, false);
                self.move_cursor(self.step(self.cursor, page, false));
            }
            Key::Char('g') | Key::Home => self.move_cursor(0),
            Key::Char('G') | Key::End => self.move_cursor(self.last_shown()),
            Key::Tab => self.fold(),
            Key::Char('n') => self.select(true),
            Key::Char('N') => self.select(false),
            Key::Enter => self.follow(notes),
            Key::Backspace | Key::Left | Key::Char('h') => self.back(),
            Key::Char('b') => {
                let list = notes.backlinks(self.note);
                if list.is_empty() {
                    self.notice = Some("No note links here.");
                } else {
                    self.backlinks = Some(Backlinks {
                        notes: list,
                        selected: 0,
                    });
                }
            }
            _ => {}
        }
        PagerAction::Stay
    }

    /// Puts the cursor on shown line `line` and scrolls as little as
    /// needed to show it.
    fn move_cursor(&mut self, line: usize) {
        self.cursor = line;
        self.follow_cursor();
    }

    /// Scrolls as little as needed to show the cursor.
    fn follow_cursor(&mut self) {
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.step(self.scroll, self.rows - 1, true) < self.cursor {
            self.scroll = self.step(self.cursor, self.rows - 1, false);
        }
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// Tab: folds the section of the heading under the cursor, or
    /// unfolds it.
    fn fold(&mut self) {
        let Some(page) = &self.page else { return };
        let Some(i) = page.sections().iter().position(|s| s.line == self.cursor) else {
            self.notice = Some("Move the cursor to a heading to fold it.");
            return;
        };
        match self.folded.iter().position(|&f| f == i) {
            Some(at) => {
                self.folded.swap_remove(at);
            }
            None => self.folded.push(i),
        }
        self.follow_cursor();
    }

    /// Opens every fold hiding `line`, so it can be shown.
    fn unfold_to(&mut self, line: usize) {
        let sections = self.page.as_ref().map_or(&[][..], Page::sections);
        self.folded.retain(|&i| {
            sections
                .get(i)
                .is_some_and(|s| !(s.line < line && line < s.end))
        });
    }

    /// n: the next link (the first at or after the cursor if none is
    /// selected); N: the previous one (the last at or before the cursor).
    /// The cursor moves to it.
    fn select(&mut self, forward: bool) {
        let Some(page) = &self.page else { return };
        let count = page.link_count();
        if count == 0 {
            self.notice = Some("This note has no [[links]].");
            return;
        }
        let next = match (self.selected, forward) {
            (Some(i), true) => (i + 1) % count,
            (Some(i), false) => (i + count - 1) % count,
            (None, true) => (0..count)
                .find(|&i| page.link_line(i).is_some_and(|l| l >= self.cursor))
                .unwrap_or(0),
            (None, false) => (0..count)
                .rev()
                .find(|&i| page.link_line(i).is_some_and(|l| l <= self.cursor))
                .unwrap_or(count - 1),
        };
        self.selected = Some(next);
        if let Some(line) = page.link_line(next) {
            self.unfold_to(line);
            self.move_cursor(line);
        }
    }

    /// Enter: follow the selected link, or say why it leads nowhere.
    fn follow(&mut self, notes: &Notes) {
        let (Some(page), Some(selected)) = (&self.page, self.selected) else {
            self.notice = Some("Select a link with n first.");
            return;
        };
        let target = Zeroizing::new(page.link_target(selected).to_owned());
        match notes.resolve(self.note, &target) {
            Ok((note, fragment)) => {
                let jump = fragment.map(|f| match f {
                    Fragment::Heading(h) => Jump::Heading(Zeroizing::new(h.to_owned())),
                    Fragment::Block(id) => Jump::Block(Zeroizing::new(id.to_owned())),
                });
                self.go(note, jump);
            }
            Err(why) => self.notice = Some(why.describe()),
        }
    }

    /// Opens `note` (or jumps within this one), remembering where we were.
    fn go(&mut self, note: usize, jump: Option<Jump>) {
        if note != self.note {
            self.history.push((self.note, self.scroll, self.cursor));
            self.note = note;
            self.page = None;
            self.scroll = 0;
            self.cursor = 0;
            self.folded.clear();
        }
        self.selected = None;
        self.jump = jump;
    }

    fn back(&mut self) {
        match self.history.pop() {
            Some((note, scroll, cursor)) => {
                self.note = note;
                self.page = None;
                self.scroll = scroll;
                self.cursor = cursor;
                self.folded.clear();
                self.selected = None;
                self.jump = None;
            }
            None => self.notice = Some("This is the first note you opened."),
        }
    }

    /// The gutter's width in a note area `width` wide, and whether it
    /// shows line numbers: `> 12 ` (marker, space, number, space), or only
    /// `> ` when the numbers would leave the note less than `MIN_TEXT`.
    fn gutter(&self, width: u16) -> (u16, bool) {
        let numbered = GUTTER + self.digits + 1;
        if width >= numbered + MIN_TEXT {
            (numbered, true)
        } else {
            (GUTTER, false)
        }
    }

    /// Shows `notice` on the bottom line until the next key.
    pub fn tell(&mut self, notice: &'static str) {
        self.notice = Some(notice);
    }

    /// The title row, the note, the key line.
    /// Draws the pager. `editor_text` is the editor's current text while
    /// this pager previews it, else `None`.
    pub fn draw(&mut self, grid: &mut Grid, notes: &Notes, editor_text: Option<&str>) {
        let area = grid.area();
        let (top, rest) = area.split_top(1);
        let (body, bottom) = rest.split_bottom(1);
        self.rows = usize::from(body.height).max(1);
        let previewing = self.preview == Some(self.note) && editor_text.is_some();
        if self.page.is_none() || self.width != body.width {
            let note = notes.note(self.note);
            let text = match editor_text {
                Some(text) if previewing => text,
                _ => note.body.as_str(),
            };
            // The gutter is as wide as the note's last line number, known
            // before rendering, so it never changes while scrolling.
            let last = text.bytes().filter(|&b| b == b'\n').count() + 1;
            self.digits = u16::try_from(last.ilog10() + 1).unwrap_or(u16::MAX);
            let (columns, _) = self.gutter(body.width);
            let width = body.width.saturating_sub(columns);
            self.page = Some(render(text, kind_of(note.title.as_str()), width));
            self.width = body.width;
            self.selected = None;
            // Sections do not depend on the width; a fold past the end
            // (a shorter preview) is dropped.
            let count = self.page.as_ref().map_or(0, |p| p.sections().len());
            self.folded.retain(|&i| i < count);
        }
        if let (Some(jump), Some(page)) = (self.jump.take(), &self.page) {
            let line = match &jump {
                Jump::Heading(h) => page.heading_line(h),
                Jump::Block(id) => page.block_line(id),
            };
            match line {
                Some(line) => {
                    self.unfold_to(line);
                    self.scroll = line;
                    self.cursor = line;
                }
                None => self.notice = Some("That heading or block is not on the page."),
            }
        }
        // After a resize or re-render, the cursor stays on a shown line of
        // the page, and on the screen.
        let last = self.lines().saturating_sub(1);
        self.cursor = self.shown_at(self.cursor.min(last));
        self.scroll = self.shown_at(self.scroll.min(last)).min(self.max_scroll());
        self.follow_cursor();

        grid.fill(top, TITLE);
        let marker = if previewing { "preview " } else { "" };
        let title = Rect::new(top.x + 1, top.y, top.width.saturating_sub(1), 1);
        status_line(grid, title, notes.name(self.note), marker, TITLE);
        if let Some(list) = &self.backlinks {
            grid.put_str(
                body.x,
                body.y,
                "Linked from (Enter opens, Esc closes):",
                HINT,
                body.width,
            );
            for (row, (i, &note)) in (body.y + 1..body.bottom()).zip(list.notes.iter().enumerate())
            {
                let style = if i == list.selected {
                    SELECTED
                } else {
                    Style::PLAIN
                };
                grid.fill(Rect::new(body.x, row, body.width, 1), style);
                grid.put_str(
                    body.x + 1,
                    row,
                    notes.name(note),
                    style,
                    body.width.saturating_sub(1),
                );
            }
        } else if let Some(page) = &self.page {
            // The gutter on the left, the note in the rest.
            let (columns, numbered) = self.gutter(body.width);
            let (gutter, text_area) = body.split_left(columns);
            let digits = usize::from(self.digits);
            let mut line = Some(self.scroll).filter(|&l| l < page.line_count());
            for y in body.y..body.bottom() {
                let Some(index) = line else { break };
                let (x, width) = (text_area.x, text_area.width);
                let used = page.draw_line(grid, (x, y), width, index, self.selected);
                let folded = self
                    .folded
                    .iter()
                    .any(|&i| page.sections().get(i).is_some_and(|s| s.line == index));
                if folded {
                    grid.put_str(x + used, y, " …", HINT, width - used);
                }
                if index == self.cursor {
                    grid.put_str(gutter.x, y, ">", MARKER, gutter.width);
                }
                if let Some(number) = page.number(index).filter(|_| numbered) {
                    let number = format!("{number:>digits$}");
                    grid.put_str(gutter.x + GUTTER, y, &number, HINT, self.digits);
                }
                line = self.next_shown(index);
            }
        }
        match self.notice {
            Some(notice) => grid.put_str(bottom.x, bottom.y, notice, NOTICE, bottom.width),
            None => grid.put_str(bottom.x, bottom.y, HINTS, HINT, bottom.width),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::tests::{note, notes};

    fn sample() -> Notes {
        let long: String = (1..=40).map(|i| format!("line {i}\n\n")).collect();
        notes(vec![
            note(
                1,
                "",
                "home",
                "Start at [[b]] or [[b#Second]] or [[b#^k]].\n\n[[gone]]",
            ),
            note(
                2,
                "",
                "b",
                &format!("# First\n\n{long}## Second\n\nLast. ^k"),
            ),
            note(3, "", "c", "Back to [[home]]."),
        ])
    }

    fn rows(pager: &mut Pager, notes: &Notes, width: u16, height: u16) -> Vec<String> {
        let mut grid = Grid::new(width, height);
        pager.draw(&mut grid, notes, None);
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| grid.cell(x, y).unwrap().symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn shows_title_note_and_hints() {
        let n = sample();
        let mut p = Pager::open(0);
        let shown = rows(&mut p, &n, 60, 6);
        assert_eq!(shown[0], " home");
        assert_eq!(shown[1], "> 1 Start at b or b#Second or b#^k.");
        assert!(shown[5].starts_with("q shell · j/k move · Tab fold"));
    }

    /// From the editor: its text, unsaved, in place of the saved note, and
    /// only for that note; Ctrl+E goes back to editing.
    #[test]
    fn preview_shows_the_editors_text_for_its_note_only() {
        let n = sample();
        let draft = "# Draft\n\nNot saved yet, see [[c]].";
        let mut grid = Grid::new(60, 8);
        let mut p = Pager::preview(0);
        p.draw(&mut grid, &n, Some(draft));
        let shown = |grid: &Grid, y| -> String {
            (0..60).map(|x| grid.cell(x, y).unwrap().symbol()).collect()
        };
        assert!(shown(&grid, 0).starts_with(" home"));
        assert!(shown(&grid, 0).trim_end().ends_with("preview"));
        assert!((1..7).any(|y| shown(&grid, y).contains("Not saved yet")));
        // A followed link shows the saved note, not the draft.
        p.key(Key::Char('n'), &n);
        p.key(Key::Enter, &n);
        p.draw(&mut grid, &n, Some(draft));
        assert!(shown(&grid, 0).starts_with(" c"));
        assert!(!shown(&grid, 0).contains("preview"));
        assert!((1..7).any(|y| shown(&grid, y).contains("Back to home")));
        assert_eq!(p.key(Key::Ctrl('e'), &n), PagerAction::Edit(2));
        // A plain `view` ignores any text it is handed.
        let mut p = Pager::open(0);
        p.draw(&mut grid, &n, Some(draft));
        assert!(!(1..7).any(|y| shown(&grid, y).contains("Not saved yet")));
        assert_eq!(p.key(Key::Ctrl('e'), &n), PagerAction::Edit(0));
    }

    #[test]
    fn scrolling_stays_in_bounds() {
        let n = sample();
        let mut p = Pager::open(1);
        rows(&mut p, &n, 40, 10);
        p.key(Key::End, &n);
        let bottom = p.scroll;
        assert_eq!(bottom, p.max_scroll());
        assert_eq!(p.cursor, p.lines() - 1);
        p.key(Key::Char('j'), &n);
        assert_eq!((p.scroll, p.cursor), (bottom, p.lines() - 1));
        p.key(Key::Char('g'), &n);
        assert_eq!((p.scroll, p.cursor), (0, 0));
        p.key(Key::PageDown, &n);
        assert_eq!((p.scroll, p.cursor), (8, 8));
        p.key(Key::Char('k'), &n);
        assert_eq!((p.scroll, p.cursor), (7, 7));
        p.key(Key::PageUp, &n);
        assert_eq!((p.scroll, p.cursor), (0, 0));
    }

    /// The cursor moves line by line inside the screen, which scrolls
    /// only once it would leave; its line has `>` in the gutter, and the
    /// text keeps its own colors. n/N put it on the selected link.
    #[test]
    fn cursor_moves_and_the_screen_follows() {
        let n = sample();
        let mut p = Pager::open(1);
        let mut grid = Grid::new(40, 10);
        p.draw(&mut grid, &n, None);
        // Rows 1 to 8 show the note; the cursor starts on its first line.
        let marked = |grid: &Grid| -> Vec<u16> {
            (1..9)
                .filter(|&y| grid.cell(0, y).unwrap().symbol() == ">")
                .collect()
        };
        assert_eq!(marked(&grid), [1]);
        // 85 lines: two digits, so the note starts at column 5.
        assert_eq!(grid.cell(5, 1).unwrap().symbol(), "◉");
        assert_eq!(grid.cell(5, 1).unwrap().style().fg, Color::Yellow);
        assert_eq!(grid.cell(5, 1).unwrap().style().bg, Color::Default);
        for _ in 0..7 {
            p.key(Key::Down, &n);
        }
        // Blank, as the screen hands every frame a blank grid.
        grid.reset();
        p.draw(&mut grid, &n, None);
        assert_eq!((p.scroll, p.cursor), (0, 7));
        assert_eq!(marked(&grid), [8]);
        p.key(Key::Char('j'), &n);
        grid.reset();
        p.draw(&mut grid, &n, None);
        assert_eq!((p.scroll, p.cursor), (1, 8));
        assert_eq!(marked(&grid), [8]);
        // From the cursor, n finds the first link at or after it.
        let mut p = Pager::open(0);
        rows(&mut p, &n, 60, 10);
        p.key(Key::Char('j'), &n);
        p.key(Key::Char('j'), &n);
        p.key(Key::Char('n'), &n);
        assert_eq!(p.selected, Some(3));
        assert_eq!(p.cursor, 2);
        p.key(Key::Char('N'), &n);
        assert_eq!((p.selected, p.cursor), (Some(2), 0));
    }

    #[test]
    fn links_are_selected_followed_and_left() {
        let n = sample();
        let mut p = Pager::open(0);
        rows(&mut p, &n, 60, 10);
        assert_eq!(p.key(Key::Enter, &n), PagerAction::Stay);
        assert_eq!(p.notice, Some("Select a link with n first."));
        p.key(Key::Char('n'), &n);
        p.key(Key::Char('n'), &n);
        assert_eq!(p.selected, Some(1));
        p.key(Key::Enter, &n);
        let shown = rows(&mut p, &n, 60, 10);
        assert_eq!(shown[0], " b");
        // Near the end the page cannot scroll further, so the heading is
        // shown lower down, not on the first row.
        assert!(
            shown.contains(&"> 83   ○ Second".to_owned()),
            "jumped to the heading: {shown:?}"
        );
        assert_eq!(p.scroll, p.max_scroll());
        p.key(Key::Backspace, &n);
        let shown = rows(&mut p, &n, 60, 10);
        assert_eq!(shown[0], " home");
        // The block link: the last one on the cursor's line, which came
        // back with the note.
        assert_eq!(p.cursor, 0);
        p.key(Key::Char('N'), &n);
        assert_eq!(p.selected, Some(2));
        p.key(Key::Enter, &n);
        let shown = rows(&mut p, &n, 60, 10);
        assert!(
            shown.contains(&"> 85     Last.".to_owned()),
            "jumped to the block, ^id hidden: {shown:?}"
        );
        // A broken link says why.
        p.key(Key::Backspace, &n);
        rows(&mut p, &n, 60, 10);
        p.key(Key::End, &n);
        p.key(Key::Char('N'), &n);
        p.key(Key::Enter, &n);
        assert_eq!(p.notice, Some("no note with that name"));
        assert_eq!(p.key(Key::Char('Y'), &n), PagerAction::Copy(0));
        assert_eq!(p.key(Key::Char('q'), &n), PagerAction::Close);
    }

    /// Lines: A 0, a1 2, B 4, b1 6 (with a link), C 8, c1 10, D 12, d1
    /// 14, blank lines between. A holds B and C; D is A's peer.
    fn outline() -> Notes {
        notes(vec![note(
            1,
            "",
            "o",
            "# A\n\na1\n\n## B\n\nb1 [[#C]]\n\n## C\n\nc1\n\n# D\n\nd1",
        )])
    }

    /// Tab on a heading hides its section up to the next peer and marks
    /// it ` …`; the cursor steps over hidden lines; Tab again shows them.
    /// Tab elsewhere only says where to press it.
    #[test]
    fn tab_folds_the_heading_under_the_cursor() {
        let n = outline();
        let mut p = Pager::open(0);
        rows(&mut p, &n, 40, 12);
        p.key(Key::Tab, &n);
        let shown = rows(&mut p, &n, 40, 12);
        assert_eq!(shown[1..5], [">  1 ◉ A …", "  13 ◉ D", "", "  15   d1"]);
        p.key(Key::Char('j'), &n);
        assert_eq!(p.cursor, 12);
        p.key(Key::Char('k'), &n);
        assert_eq!(p.cursor, 0);
        p.key(Key::End, &n);
        assert_eq!(p.cursor, 14);
        p.key(Key::Home, &n);
        p.key(Key::Tab, &n);
        let shown = rows(&mut p, &n, 40, 12);
        assert_eq!(shown[1..4], [">  1 ◉ A", "", "   3   a1"]);
        p.key(Key::Char('j'), &n);
        p.key(Key::Tab, &n);
        assert_eq!(p.notice, Some("Move the cursor to a heading to fold it."));
        assert!(p.folded.is_empty());
    }

    /// A fold inside a folded section stays folded when the outer one
    /// opens; selecting a link inside folds opens them all, and so does a
    /// jump to a heading inside one.
    #[test]
    fn folds_nest_and_open_for_links() {
        let n = outline();
        let mut p = Pager::open(0);
        rows(&mut p, &n, 40, 12);
        for _ in 0..4 {
            p.key(Key::Char('j'), &n);
        }
        assert_eq!(p.cursor, 4);
        p.key(Key::Tab, &n);
        p.key(Key::Home, &n);
        p.key(Key::Tab, &n);
        p.key(Key::Tab, &n);
        let shown = rows(&mut p, &n, 40, 12);
        assert_eq!(
            shown[1..6],
            [">  1 ◉ A", "", "   3   a1", "", "   5   ○ B …"]
        );
        assert_eq!(shown[6], "   9   ○ C");
        p.key(Key::Tab, &n);
        p.key(Key::Char('n'), &n);
        assert_eq!((p.selected, p.cursor), (Some(0), 6));
        assert!(p.folded.is_empty());
        // The link stays selected while A is folded over it; following
        // it jumps to C, inside A, which opens.
        p.key(Key::Home, &n);
        p.key(Key::Tab, &n);
        assert_eq!(p.folded.len(), 1);
        p.key(Key::Enter, &n);
        rows(&mut p, &n, 40, 12);
        assert!(p.folded.is_empty());
        assert_eq!(p.cursor, 8);
    }

    /// With folds, paging and the ends count shown lines only, and the
    /// last screen is full.
    #[test]
    fn paging_skips_folded_lines() {
        let n = outline();
        let mut p = Pager::open(0);
        // Three rows of note.
        rows(&mut p, &n, 40, 5);
        for _ in 0..4 {
            p.key(Key::Char('j'), &n);
        }
        p.key(Key::Tab, &n);
        p.key(Key::Home, &n);
        // Shown: 0 1 2 3 4(B…) 8 9 10 11 12 13 14.
        p.key(Key::PageDown, &n);
        assert_eq!((p.scroll, p.cursor), (3, 3));
        p.key(Key::PageDown, &n);
        assert_eq!((p.scroll, p.cursor), (9, 9));
        p.key(Key::End, &n);
        assert_eq!((p.scroll, p.cursor), (12, 14));
        assert_eq!(p.max_scroll(), 12);
        p.key(Key::PageUp, &n);
        assert_eq!((p.scroll, p.cursor), (9, 11));
        let shown = rows(&mut p, &n, 40, 5);
        assert_eq!(shown[1..4], ["", "  11     c1", ">"]);
    }

    #[test]
    fn backlinks_list_opens_a_note() {
        let n = sample();
        let mut p = Pager::open(0);
        rows(&mut p, &n, 60, 10);
        p.key(Key::Char('b'), &n);
        let shown = rows(&mut p, &n, 60, 10);
        assert!(shown[1].starts_with("Linked from"));
        assert_eq!(shown[2], " c");
        p.key(Key::Enter, &n);
        assert_eq!(rows(&mut p, &n, 60, 10)[0], " c");
        let mut p = Pager::open(2);
        rows(&mut p, &n, 60, 10);
        p.key(Key::Char('b'), &n);
        assert_eq!(p.notice, Some("No note links here."));
    }

    /// The gutter: the editor's line numbers, right-aligned to the last
    /// line's digits, none on a wrapped row; on a screen too narrow for
    /// them, only the marker.
    #[test]
    fn gutter_shows_line_numbers() {
        let body: String = (1..=9).map(|i| format!("l{i}\n")).collect();
        let n = notes(vec![note(
            1,
            "",
            "w",
            &(body + "words words words words words"),
        )]);
        let mut p = Pager::open(0);
        let shown = rows(&mut p, &n, 30, 14);
        assert_eq!(shown[1], ">  1 l1");
        assert_eq!(shown[9], "   9 l9");
        assert_eq!(shown[10], "  10 words words words words");
        assert_eq!(shown[11], "     words");
        let shown = rows(&mut p, &n, 24, 14);
        assert_eq!(shown[1], "> l1");
        assert_eq!(shown[10], "  words words words");
    }

    /// y copies the line the cursor is on (the whole note line, on a
    /// wrapped row too); on a blank it says there is none.
    #[test]
    fn y_copies_the_cursors_line() {
        let n = notes(vec![note(
            1,
            "",
            "y",
            "# T\r\n\nwords words words words words",
        )]);
        let mut p = Pager::open(0);
        rows(&mut p, &n, 30, 10);
        assert_eq!(p.key(Key::Char('y'), &n), PagerAction::CopyLine(0, 1));
        p.key(Key::Char('j'), &n);
        assert_eq!(p.key(Key::Char('y'), &n), PagerAction::Stay);
        assert_eq!(p.notice, Some("No line here to copy."));
        p.key(Key::Char('j'), &n);
        p.key(Key::Char('j'), &n);
        assert_eq!(p.cursor, 3, "on the wrapped row");
        assert_eq!(p.key(Key::Char('y'), &n), PagerAction::CopyLine(0, 3));
        let text = n.note(0).body.as_str();
        assert_eq!(line_in(text, 1), "# T");
        assert_eq!(line_in(text, 2), "");
        assert_eq!(line_in(text, 3), "words words words words words");
        assert_eq!(line_in(text, 4), "");
    }

    #[test]
    fn resize_rerenders() {
        let n = sample();
        let mut p = Pager::open(0);
        let wide = rows(&mut p, &n, 60, 6);
        let narrow = rows(&mut p, &n, 12, 6);
        assert_ne!(wide[1], narrow[1]);
        assert!(narrow.iter().all(|r| r.chars().count() <= 12));
    }
}
