//! `view NAME`: a note, rendered full screen (decision 0033).
//!
//! The title on the top row, the note below, a key line at the bottom.
//! A `>` in the gutter on the left marks the cursor's line; the cursor
//! changes nothing, it only points. j/k and
//! the arrows move it from one note line to the next, as numbered by
//! `model::lines`, though the viewer shows no numbers (the editor does), Space and PgDn/PgUp by a screen, g/G or
//! Home/End to the ends, and the screen scrolls to keep it in view. n and
//! N select the next and previous `[[link]]` and move the cursor to it,
//! Enter follows it (to a note, a heading or a `^block`), Backspace goes
//! back. Tab on a heading folds its section (hides the lines up to the
//! next heading of its level or higher) or unfolds it; a folded heading
//! ends in ` …`. Folds last while the note is shown and are never saved;
//! a jump or a link selected inside a fold opens it. b lists the notes
//! linking here. y copies the note line the cursor is on, as written
//! (Markdown included), into Crystal's own clipboard, Y the whole note
//! (decision 0041); the editor's Ctrl+V
//! pastes it. q or Esc
//! returns to the shell. Ctrl+E asks to edit the note shown
//! (decision 0045).
//!
//! From the editor, Ctrl+E opens a preview: the editor's current text,
//! unsaved changes included, rendered in place of the saved note and
//! marked "preview" on the top row. Notes reached by following a link
//! show their saved text. Links resolve against the saved notes.
//!
//! Like the shell, it is terminal-free: keys in, frames out. A page is
//! rendered for the current width and again after a resize.

use zeroize::Zeroizing;

use crate::md::render::{Page, Section, kind_of, render, shown};
use crate::notes::{Fragment, Notes};
use crate::shell::keys::Key;
use crate::ui::grid::{Color, Grid, Mods, Style};
use crate::ui::layout::Rect;
use crate::ui::widgets::status_line;

/// The title bar: the note's name in bold on the terminal's background.
const TITLE: Style = Style::PLAIN.mods(Mods::BOLD);
const HINT: Style = Style::PLAIN.mods(Mods::DIM);
const NOTICE: Style = Style::PLAIN.fg(Color::Yellow);
const SELECTED: Style = Style::PLAIN.mods(Mods::REVERSED);
/// The cursor's mark, in the gutter left of the note: bold orange
/// (256-color 208), to find at a glance.
const MARKER: Style = Style::PLAIN.fg(Color::Indexed(208)).mods(Mods::BOLD);
/// Columns left of the note: the marker and a space.
const GUTTER: u16 = 2;
const HINTS: &str = "q shell · j/k move · Tab fold · n/N links · Enter follow · \
                     Backspace previous · b backlinks · y/Y copy line/note · Ctrl+E edit";

/// After a key: stay, or go back to the shell.
#[derive(Debug, PartialEq, Eq)]
pub enum PagerAction {
    Stay,
    Close,
    /// Copy this note into Crystal's own clipboard.
    Copy(usize),
    /// Copy this line (numbered as `model::lines` does) of this note into
    /// Crystal's own clipboard.
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
            // The terminal sends the mouse wheel as Up and Down.
            Key::Char('j') | Key::Down => self.step_line(true),
            Key::Char('k') | Key::Up => self.step_line(false),
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

    /// j and k: onto the next (or previous) numbered line. Past the last
    /// one (before the first) the cursor stays, and the screen shows the
    /// rows left that way (a wrapped line's rest, a table's bottom border)
    /// as far as the cursor stays on it.
    fn step_line(&mut self, forward: bool) {
        match self.next_numbered(forward) {
            Some(line) => {
                self.move_cursor(line);
                self.show_whole_line();
            }
            None if forward => self.scroll = self.max_scroll().min(self.cursor),
            None => {
                let top = self.step(self.cursor, self.rows.saturating_sub(1), false);
                self.scroll = self.scroll.min(top);
            }
        }
    }

    /// The next (or previous) shown row that starts a numbered line, past
    /// wrapped rows, blanks and table borders; `None` if there is none.
    fn next_numbered(&self, forward: bool) -> Option<usize> {
        let page = self.page.as_ref()?;
        let mut row = self.cursor;
        loop {
            row = if forward {
                self.next_shown(row)?
            } else {
                self.prev_shown(row)?
            };
            if page.number(row).is_some() {
                return Some(row);
            }
        }
    }

    /// Scrolls on so that the rows the cursor's line wraps onto are shown
    /// too, if they fit: never so far that the cursor leaves the screen.
    fn show_whole_line(&mut self) {
        let Some(page) = &self.page else { return };
        let Some(line) = page.note_line(self.cursor) else {
            return;
        };
        let mut end = self.cursor;
        while let Some(next) = self.next_shown(end) {
            if page.number(next).is_some() || page.note_line(next) != Some(line) {
                break;
            }
            end = next;
        }
        let last_row = self.rows.saturating_sub(1);
        if self.step(self.scroll, last_row, true) < end {
            let scroll = self.step(end, last_row, false).min(self.cursor);
            self.scroll = scroll.min(self.max_scroll());
        }
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
            let width = body.width.saturating_sub(GUTTER);
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
        let (name, _) = shown(notes.name(self.note));
        status_line(grid, title, name, marker, TITLE);
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
                // The shown name, its type dimmed after it (decision 0038).
                let (name, kind) = shown(notes.name(note));
                let room = body.width.saturating_sub(1);
                let used = grid.put_str(body.x + 1, row, name, style, room);
                let dim = style.mods(style.mods | Mods::DIM);
                grid.put_str(body.x + 1 + used, row, kind.label(), dim, room - used);
            }
        } else if let Some(page) = &self.page {
            // The gutter on the left, the note in the rest.
            let (gutter, text_area) = body.split_left(GUTTER);
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
    use crate::model::lines;
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
        assert_eq!(shown[1], "> Start at b or b#Second or b#^k.");
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
        // k goes back a line, past the blank row between.
        p.key(Key::Char('k'), &n);
        assert_eq!((p.scroll, p.cursor), (6, 6));
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
        // The `>` is bold orange; no line numbers, so the note starts at
        // column 2, after a blank one, keeping its own colors.
        let style = grid.cell(0, 1).unwrap().style();
        assert_eq!(style.fg, Color::Indexed(208));
        assert!(style.mods.contains(Mods::BOLD));
        assert_eq!(grid.cell(1, 1).unwrap().symbol(), " ");
        assert_eq!(grid.cell(2, 1).unwrap().symbol(), "◉");
        assert_eq!(grid.cell(2, 1).unwrap().style().fg, Color::Yellow);
        assert_eq!(grid.cell(2, 1).unwrap().style().bg, Color::Default);
        // Lines alternate with blank rows, which Down skips.
        for _ in 0..3 {
            p.key(Key::Down, &n);
        }
        // Blank, as the screen hands every frame a blank grid.
        grid.reset();
        p.draw(&mut grid, &n, None);
        assert_eq!((p.scroll, p.cursor), (0, 6));
        assert_eq!(marked(&grid), [7]);
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
            shown.contains(&">   ○ Second".to_owned()),
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
            shown.contains(&">     Last.".to_owned()),
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
        assert_eq!(shown[1..5], ["> ◉ A …", "  ◉ D", "", "    d1"]);
        p.key(Key::Char('j'), &n);
        assert_eq!(p.cursor, 12);
        p.key(Key::Char('k'), &n);
        assert_eq!(p.cursor, 0);
        p.key(Key::End, &n);
        assert_eq!(p.cursor, 14);
        p.key(Key::Home, &n);
        p.key(Key::Tab, &n);
        let shown = rows(&mut p, &n, 40, 12);
        assert_eq!(shown[1..4], ["> ◉ A", "", "    a1"]);
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
        // j skips the blank rows: a1, then B.
        p.key(Key::Char('j'), &n);
        p.key(Key::Char('j'), &n);
        assert_eq!(p.cursor, 4);
        p.key(Key::Tab, &n);
        p.key(Key::Home, &n);
        p.key(Key::Tab, &n);
        p.key(Key::Tab, &n);
        let shown = rows(&mut p, &n, 40, 12);
        assert_eq!(shown[1..6], ["> ◉ A", "", "    a1", "", "    ○ B …"]);
        assert_eq!(shown[6], "    ○ C");
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
        // j skips the blank rows: a1, then B.
        p.key(Key::Char('j'), &n);
        p.key(Key::Char('j'), &n);
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
        assert_eq!(shown[1..4], ["", "      c1", ">"]);
    }

    /// The title bar shows only the name, without `.txt` or the type, in
    /// bold on the terminal's background (decision 0038).
    #[test]
    fn title_shows_only_the_name() {
        let n = notes(vec![note(1, "work", "todo.txt", "milk")]);
        let mut p = Pager::open(0);
        let shown = rows(&mut p, &n, 40, 4);
        assert_eq!(shown[0], " work/todo");
        let mut grid = Grid::new(40, 4);
        p.draw(&mut grid, &n, None);
        assert!((0..40).all(|x| grid.cell(x, 0).unwrap().style() == TITLE));
        assert!(!TITLE.mods.contains(Mods::REVERSED));
    }

    #[test]
    fn backlinks_list_opens_a_note() {
        let n = sample();
        let mut p = Pager::open(0);
        rows(&mut p, &n, 60, 10);
        p.key(Key::Char('b'), &n);
        let shown = rows(&mut p, &n, 60, 10);
        assert!(shown[1].starts_with("Linked from"));
        assert_eq!(shown[2], " c · markdown");
        p.key(Key::Enter, &n);
        assert_eq!(rows(&mut p, &n, 60, 10)[0], " c");
        let mut p = Pager::open(2);
        rows(&mut p, &n, 60, 10);
        p.key(Key::Char('b'), &n);
        assert_eq!(p.notice, Some("No note links here."));
    }

    /// The gutter is the cursor's `>` and a blank: no line numbers, however
    /// wide the screen (the editor shows them).
    #[test]
    fn gutter_shows_no_line_numbers() {
        let body: String = (1..=9).map(|i| format!("l{i}\n")).collect();
        let n = notes(vec![note(
            1,
            "",
            "w",
            &(body + "words words words words words"),
        )]);
        let mut p = Pager::open(0);
        let shown = rows(&mut p, &n, 30, 14);
        assert_eq!(shown[1], "> l1");
        assert_eq!(shown[9], "  l9");
        assert_eq!(shown[10], "  words words words words");
        assert_eq!(shown[11], "  words");
        let shown = rows(&mut p, &n, 80, 14);
        assert_eq!(shown[10], "  words words words words words");
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
        assert_eq!(p.cursor, 2, "past the blank row");
        assert_eq!(p.key(Key::Char('y'), &n), PagerAction::CopyLine(0, 2));
        // No line after it: j stays; on its wrapped row (End) y copies
        // it too.
        p.key(Key::Char('j'), &n);
        assert_eq!(p.cursor, 2, "j stays on the last line");
        p.key(Key::End, &n);
        assert_eq!(p.cursor, 3, "on the wrapped row");
        assert_eq!(p.key(Key::Char('y'), &n), PagerAction::CopyLine(0, 2));
        let text = n.note(0).body.as_str();
        assert_eq!(lines::numbered(text, 1), "# T");
        assert_eq!(lines::numbered(text, 2), "words words words words words");
        assert_eq!(lines::numbered(text, 3), "");
        // A row without note text: a table's bottom border.
        let n = notes(vec![note(1, "", "t", "a\n\n| x |\n|-|\n| y |")]);
        let mut p = Pager::open(0);
        rows(&mut p, &n, 30, 10);
        p.key(Key::End, &n);
        assert_eq!(p.key(Key::Char('y'), &n), PagerAction::Stay);
        assert_eq!(p.notice, Some("No line here to copy."));
    }

    /// j and k go from one numbered line to the next, past blank rows,
    /// wrapped rows and table borders (the mouse wheel too, which the
    /// terminal sends as Up and Down); past the last line, one row at a
    /// time. A wrapped line is shown whole when the cursor lands on it.
    #[test]
    fn j_and_k_move_by_numbered_line() {
        let long = "word ".repeat(15);
        let text = format!("x\n\n{long}\n\n| a |\n|---|\n| b |");
        let n = notes(vec![note(1, "", "j", &text)]);
        let mut p = Pager::open(0);
        rows(&mut p, &n, 30, 20);
        let mut seen = Vec::new();
        for _ in 0..5 {
            seen.push(p.page.as_ref().unwrap().number(p.cursor));
            p.key(Key::Down, &n);
        }
        // x, the long line, the table's two rows (its `|---|` line has no
        // number, so j skips the border under the head); then j stays on
        // the last line.
        assert_eq!(seen, [Some(1), Some(2), Some(3), Some(4), Some(4)]);
        for _ in 0..3 {
            p.key(Key::Char('k'), &n);
        }
        assert_eq!(p.cursor, 0);
        p.key(Key::Char('k'), &n);
        assert_eq!((p.scroll, p.cursor), (0, 0), "k stays on the first line");
        // Four rows of note on screen. j past the table's last line keeps
        // the cursor there and only scrolls its bottom border into view.
        let n = notes(vec![note(1, "", "t", "a\n\n| b |\n|---|\n| c |")]);
        let mut p = Pager::open(0);
        rows(&mut p, &n, 30, 6);
        for _ in 0..2 {
            p.key(Key::Char('j'), &n);
        }
        let cursor = p.cursor;
        assert_eq!(p.page.as_ref().unwrap().number(cursor), Some(3));
        p.key(Key::Char('j'), &n);
        assert_eq!(p.cursor, cursor, "j stays on the last line");
        assert_eq!(p.scroll, p.max_scroll());
        let shown = rows(&mut p, &n, 30, 6);
        assert_eq!(shown[4], "  └───┘");
        // Four rows of note on screen; the note is a, blank, b, blank,
        // then the long line's three rows. j onto it scrolls until all
        // three are shown.
        let n = notes(vec![note(1, "", "w", &format!("a\n\nb\n\n{long}"))]);
        let mut p = Pager::open(0);
        rows(&mut p, &n, 30, 6);
        p.key(Key::Char('j'), &n);
        assert_eq!((p.scroll, p.cursor), (0, 2));
        p.key(Key::Char('j'), &n);
        assert_eq!((p.scroll, p.cursor), (3, 4));
        let shown = rows(&mut p, &n, 30, 6);
        assert_eq!(
            shown[1..5],
            [
                "",
                "> word word word word word",
                "  word word word word word",
                "  word word word word word"
            ]
        );
        // The long line is the last: j leaves the cursor on it, not on
        // the rows it wraps onto.
        p.key(Key::Char('j'), &n);
        assert_eq!((p.scroll, p.cursor), (3, 4));
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
