//! `view NAME`: a note, rendered full screen (decision 0033).
//!
//! The title on the top row, the note below, a key line at the bottom.
//! There is no cursor (decision 0076): j/k and the arrows scroll a row,
//! Space and PgDn/PgUp a screen, g/G or Home/End to the ends. n and N
//! select the next and previous `[[link]]` and scroll it into view,
//! Enter follows it (to a note, a heading or a `^block`), Backspace goes
//! back. Tab folds the section the top row is in (hides the lines up to
//! the next heading of its level or higher) or unfolds it; a folded
//! heading ends in ` …`. Folds last while the note is shown and are never
//! saved; a jump or a link selected inside a fold opens it. b lists the
//! notes linking here. y copies the first note line shown, as written
//! (Markdown included), into Crystal's own clipboard, Y the whole note
//! (decision 0041); the editor's Ctrl+V
//! pastes it. q or Esc
//! returns to the shell. Ctrl+E asks to edit the note shown
//! (decision 0045).
//!
//! From the editor, Ctrl+E opens a preview: the editor's current text,
//! unsaved changes included, rendered in place of the saved note and
//! marked "preview" on the top row. Notes reached by following a link
//! show their saved text, and Ctrl+E there edits that note (once the
//! editor's changes are saved). Links resolve against the saved notes.
//!
//! Like the shell, it is terminal-free: keys in, frames out. A page is
//! rendered for the current width and again after a resize.

use zeroize::Zeroizing;

use crate::md::render::{Clickable, LinkTarget, Page, Section, kind_of, render, shown};
use crate::notes::{Fragment, Notes};
use crate::shell::keys::{Key, MouseEvent};
use crate::ui::grid::{Color, Grid, Mods, Style};
use crate::ui::layout::Rect;
use crate::ui::widgets::status_line;

/// The title bar: the note's name in bold on the terminal's background.
const TITLE: Style = Style::PLAIN.mods(Mods::BOLD);
const HINT: Style = Style::PLAIN.mods(Mods::DIM);
const NOTICE: Style = Style::PLAIN.fg(Color::Yellow);
const SELECTED: Style = Style::PLAIN.mods(Mods::REVERSED);
/// Columns left of the note: a heading's marker and a space. The
/// editor's live mode leaves the same columns, so lines start where the
/// viewer draws them.
pub const GUTTER: u16 = 2;
/// A heading's marker in the gutter, shown only while the mouse is over
/// it: `v` for an open section, `>` for a folded one; a click folds or
/// unfolds it. Bold orange (256-color 208), to find at a glance
/// (decision 0076).
const MARKER: Style = Style::PLAIN.fg(Color::Indexed(208)).mods(Mods::BOLD);
/// Shown on a click on a web link: the terminal opens it, never Crystal
/// (decision 0076).
const WEB_LINK: &str = "Web links open with Ctrl+click: the terminal opens them, not Crystal.";
/// Rows one step of the mouse wheel scrolls (as in the editor, 0073).
const WHEEL_ROWS: usize = 3;
const HINTS: &str = "q shell · j/k scroll · Tab fold · n/N links · Enter follow · \
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
    /// Notes left by following links, with their scroll.
    history: Vec<(usize, usize)>,
    page: Option<Page>,
    /// The note area's width the page was rendered for, gutter included.
    width: u16,
    /// The shown line on the top row.
    scroll: usize,
    /// Folded sections, as indices into the page's sections.
    folded: Vec<usize>,
    /// Rows the note had when last drawn.
    rows: usize,
    /// The shown line on each row of the note as last drawn, from
    /// `top_row` down: what a mouse report's row points at.
    drawn: Vec<usize>,
    top_row: u16,
    /// The screen row whose heading marker the mouse is over.
    hover: Option<u16>,
    /// The links as last drawn, for clicks.
    clickable: Vec<Clickable>,
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
            folded: Vec::new(),
            rows: 1,
            drawn: Vec::new(),
            top_row: 0,
            hover: None,
            clickable: Vec::new(),
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
        // Mouse reports (decision 0076).
        if let Key::Mouse { event, column, row } = key {
            return self.mouse(event, column, row, notes);
        }
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
            Key::Char('y') => {
                let page = self.page.as_ref();
                match page.and_then(|p| self.on_screen().find_map(|line| p.note_line(line))) {
                    Some(line) => return PagerAction::CopyLine(self.note, line),
                    None => self.notice = Some("No line here to copy."),
                }
            }
            Key::Char('Y') => return PagerAction::Copy(self.note),
            Key::Ctrl('e') => return PagerAction::Edit(self.note),
            Key::Char('j') | Key::Down => self.scroll_by(1, true),
            Key::Char('k') | Key::Up => self.scroll_by(1, false),
            Key::Char(' ') | Key::PageDown => self.scroll_by(page, true),
            Key::PageUp => self.scroll_by(page, false),
            Key::Char('g') | Key::Home => self.scroll = 0,
            Key::Char('G') | Key::End => self.scroll = self.max_scroll(),
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

    /// Scrolls `count` shown lines down (or up), within the page.
    fn scroll_by(&mut self, count: usize, down: bool) {
        self.scroll = self.step(self.scroll, count, down).min(self.max_scroll());
    }

    /// Scrolls as little as needed to show shown line `line`.
    fn show_line(&mut self, line: usize) {
        if line < self.scroll {
            self.scroll = line;
        } else if self.step(self.scroll, self.rows - 1, true) < line {
            self.scroll = self.step(line, self.rows - 1, false);
        }
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// The shown lines on the screen, from the top row down.
    fn on_screen(&self) -> impl Iterator<Item = usize> + '_ {
        let top = Some(self.scroll).filter(|&line| line < self.lines());
        std::iter::successors(top, |&line| self.next_shown(line)).take(self.rows)
    }

    /// Tab: folds the section the top row is in (the innermost one whose
    /// heading is at or above it), else the first heading on the screen;
    /// or unfolds it.
    fn fold(&mut self) {
        let Some(page) = &self.page else { return };
        let sections = page.sections();
        let top = self.scroll;
        let i = sections
            .iter()
            .rposition(|s| s.line <= top && top < s.end)
            .or_else(|| {
                self.on_screen()
                    .find_map(|line| sections.iter().position(|s| s.line == line))
            });
        match i {
            Some(i) => self.toggle(i),
            None => self.notice = Some("No heading here to fold."),
        }
    }

    /// Folds section `i` (an index into the page's sections), or unfolds
    /// it.
    fn toggle(&mut self, i: usize) {
        match self.folded.iter().position(|&f| f == i) {
            Some(at) => {
                self.folded.swap_remove(at);
            }
            None => self.folded.push(i),
        }
        // A fold over the top row: its heading becomes the top.
        self.scroll = self.shown_at(self.scroll).min(self.max_scroll());
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

    /// n: the next link (the first at or after the top row if none is
    /// selected); N: the previous one (the last at or before the bottom
    /// row). The screen scrolls to show it.
    fn select(&mut self, forward: bool) {
        let Some(page) = &self.page else { return };
        let count = page.link_count();
        if count == 0 {
            self.notice = Some("This note has no [[links]].");
            return;
        }
        let bottom = self.on_screen().last().unwrap_or(self.scroll);
        let next = match (self.selected, forward) {
            (Some(i), true) => (i + 1) % count,
            (Some(i), false) => (i + count - 1) % count,
            (None, true) => (0..count)
                .find(|&i| page.link_line(i).is_some_and(|l| l >= self.scroll))
                .unwrap_or(0),
            (None, false) => (0..count)
                .rev()
                .find(|&i| page.link_line(i).is_some_and(|l| l <= bottom))
                .unwrap_or(count - 1),
        };
        self.selected = Some(next);
        if let Some(line) = page.link_line(next) {
            self.unfold_to(line);
            self.show_line(line);
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
            self.history.push((self.note, self.scroll));
            self.note = note;
            self.page = None;
            self.scroll = 0;
            self.folded.clear();
        }
        self.selected = None;
        self.jump = jump;
    }

    fn back(&mut self) {
        match self.history.pop() {
            Some((note, scroll)) => {
                self.note = note;
                self.page = None;
                self.scroll = scroll;
                self.folded.clear();
                self.selected = None;
                self.jump = None;
            }
            None => self.notice = Some("This is the first note you opened."),
        }
    }

    /// A mouse report (decision 0076), at a cell counted from 0: moving
    /// over a heading's row shows its marker, a press on the row folds or
    /// unfolds its section (a link in it excepted), a press on a `[[link]]` follows it as Enter
    /// does (on a web link it says to Ctrl+click), the wheel scrolls
    /// `WHEEL_ROWS`. Anything else does nothing. A press, like a key,
    /// clears the notice.
    fn mouse(&mut self, event: MouseEvent, column: u16, row: u16, notes: &Notes) -> PagerAction {
        // On a heading's row (its marker or its text): its section.
        let heading = row
            .checked_sub(self.top_row)
            .and_then(|at| self.drawn.get(usize::from(at)))
            .and_then(|&line| {
                let page = self.page.as_ref()?;
                page.sections().iter().position(|s| s.line == line)
            })
            .filter(|_| self.backlinks.is_none());
        match event {
            MouseEvent::Move => self.hover = heading.map(|_| row),
            MouseEvent::Press => {
                self.notice = None;
                let link = self
                    .clickable
                    .iter()
                    .find(|c| c.y == row && c.x <= column && column - c.x < c.columns)
                    .map(|c| c.target)
                    .filter(|_| self.backlinks.is_none());
                // A link in a heading takes the click on its own cells.
                match (link, heading) {
                    (Some(LinkTarget::Note(link)), _) => {
                        self.selected = Some(link);
                        self.follow(notes);
                    }
                    (Some(LinkTarget::Web), _) => self.notice = Some(WEB_LINK),
                    (None, Some(i)) => self.toggle(i),
                    (None, None) => {}
                }
            }
            MouseEvent::WheelUp | MouseEvent::WheelDown if self.backlinks.is_none() => {
                self.scroll_by(WHEEL_ROWS, event == MouseEvent::WheelDown);
            }
            _ => {}
        }
        PagerAction::Stay
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
        // What the mouse can point at: only what this frame draws.
        self.drawn.clear();
        self.clickable.clear();
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
                }
                None => self.notice = Some("That heading or block is not on the page."),
            }
        }
        // After a resize or re-render, the top row is a shown line, and
        // the last screen is full.
        let last = self.lines().saturating_sub(1);
        self.scroll = self.shown_at(self.scroll.min(last)).min(self.max_scroll());

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
            // The gutter on the left, a gray `>` on each heading that opens
            // a section; the note in the rest.
            let (gutter, text_area) = body.split_left(GUTTER);
            let mut line = Some(self.scroll).filter(|&l| l < page.line_count());
            self.top_row = body.y;
            for y in body.y..body.bottom() {
                let Some(index) = line else { break };
                self.drawn.push(index);
                let section = page.sections().iter().position(|s| s.line == index);
                if let Some(i) = section.filter(|_| self.hover == Some(y)) {
                    let mark = if self.folded.contains(&i) { ">" } else { "v" };
                    grid.put_str(gutter.x, y, mark, MARKER, gutter.width);
                }
                let (x, width) = (text_area.x, text_area.width);
                let used = page.draw_line(
                    grid,
                    (x, y),
                    width,
                    index,
                    self.selected,
                    &mut self.clickable,
                );
                let folded = self
                    .folded
                    .iter()
                    .any(|&i| page.sections().get(i).is_some_and(|s| s.line == index));
                if folded {
                    grid.put_str(x + used, y, " …", HINT, width - used);
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
        assert_eq!(shown[1], "  Start at b or b#Second or b#^k.");
        assert!(shown[5].starts_with("q shell · j/k scroll · Tab fold"));
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

    /// Keys scroll the page, row by row or a screen at a time, never past
    /// its ends; the last screen is full.
    #[test]
    fn scrolling_stays_in_bounds() {
        let n = sample();
        let mut p = Pager::open(1);
        rows(&mut p, &n, 40, 10);
        p.key(Key::End, &n);
        let bottom = p.scroll;
        assert_eq!(bottom, p.max_scroll());
        p.key(Key::Char('j'), &n);
        assert_eq!(p.scroll, bottom, "j stops at the last screen");
        p.key(Key::Char('g'), &n);
        assert_eq!(p.scroll, 0);
        p.key(Key::Char('k'), &n);
        assert_eq!(p.scroll, 0, "k stops at the top");
        p.key(Key::PageDown, &n);
        assert_eq!(p.scroll, 8);
        // j and k go a row, blank rows too.
        p.key(Key::Char('k'), &n);
        assert_eq!(p.scroll, 7);
        p.key(Key::Down, &n);
        p.key(Key::Down, &n);
        assert_eq!(p.scroll, 9);
        p.key(Key::Up, &n);
        assert_eq!(p.scroll, 8);
        p.key(Key::PageUp, &n);
        assert_eq!(p.scroll, 0);
    }

    /// No cursor (decision 0076): without the mouse over a heading the
    /// gutter is blank, and the note keeps its own colors after it. j
    /// scrolls the screen a row.
    #[test]
    fn no_cursor_is_drawn() {
        let n = sample();
        let mut p = Pager::open(1);
        let mut grid = Grid::new(40, 10);
        p.draw(&mut grid, &n, None);
        assert!((1..9).all(|y| grid.cell(0, y).unwrap().symbol() == " "));
        assert_eq!(grid.cell(1, 1).unwrap().symbol(), " ");
        assert_eq!(grid.cell(2, 1).unwrap().symbol(), "◉");
        assert_eq!(grid.cell(2, 1).unwrap().style().fg, Color::Yellow);
        assert_eq!(grid.cell(2, 1).unwrap().style().bg, Color::Default);
        p.key(Key::Char('j'), &n);
        let shown = rows(&mut p, &n, 40, 10);
        assert_eq!(p.scroll, 1);
        assert_eq!(shown[1], "");
        assert_eq!(shown[2], "    line 1");
    }

    /// n picks the first link at or after the top row, N the last at or
    /// before the bottom row; a selected link off the screen is scrolled
    /// into view, as little as needed.
    #[test]
    fn n_and_n_select_links_from_the_screen() {
        let n = sample();
        let mut p = Pager::open(0);
        rows(&mut p, &n, 60, 10);
        p.key(Key::Char('n'), &n);
        assert_eq!(p.selected, Some(0));
        let mut p = Pager::open(0);
        rows(&mut p, &n, 60, 10);
        p.key(Key::Char('N'), &n);
        assert_eq!(p.selected, Some(3), "[[gone]], on the last row shown");
        // A link far down: n scrolls it onto the bottom row.
        let body = format!("{}[[home]]", "x\n\n".repeat(20));
        let n = notes(vec![note(1, "", "home", &body)]);
        let mut p = Pager::open(0);
        rows(&mut p, &n, 30, 6);
        p.key(Key::Char('n'), &n);
        let line = p.page.as_ref().unwrap().link_line(0).unwrap();
        assert_eq!(p.scroll, line - 3, "four rows of note");
        assert_eq!(rows(&mut p, &n, 30, 6)[4], "  home");
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
            shown.contains(&"    ○ Second".to_owned()),
            "jumped to the heading: {shown:?}"
        );
        assert_eq!(p.scroll, p.max_scroll());
        // Back: the note as it was left, scrolled to the top.
        p.key(Key::Backspace, &n);
        let shown = rows(&mut p, &n, 60, 10);
        assert_eq!(shown[0], " home");
        assert_eq!(p.scroll, 0);
        // The block link, the third.
        for _ in 0..3 {
            p.key(Key::Char('n'), &n);
        }
        assert_eq!(p.selected, Some(2));
        p.key(Key::Enter, &n);
        let shown = rows(&mut p, &n, 60, 10);
        assert!(
            shown.contains(&"      Last.".to_owned()),
            "jumped to the block, ^id hidden: {shown:?}"
        );
        // A broken link says why: N from the bottom row picks it.
        p.key(Key::Backspace, &n);
        rows(&mut p, &n, 60, 10);
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

    /// Tab folds the innermost section the top row is in, up to the next
    /// peer, and marks it ` …`; the top becomes its heading. Tab again
    /// shows it. Above every heading, Tab takes the first one on the
    /// screen; with none, it says so.
    #[test]
    fn tab_folds_the_section_at_the_top() {
        let n = outline();
        let mut p = Pager::open(0);
        rows(&mut p, &n, 40, 12);
        p.key(Key::Tab, &n);
        let shown = rows(&mut p, &n, 40, 12);
        assert_eq!(shown[1..5], ["  ◉ A …", "  ◉ D", "", "    d1"]);
        p.key(Key::Tab, &n);
        let shown = rows(&mut p, &n, 40, 12);
        assert_eq!(shown[1..4], ["  ◉ A", "", "    a1"]);
        // b1 at the top (two rows of note): inside B, inside A; B folds,
        // its heading on top.
        let mut p = Pager::open(0);
        rows(&mut p, &n, 40, 4);
        for _ in 0..6 {
            p.key(Key::Char('j'), &n);
        }
        assert_eq!(p.scroll, 6);
        p.key(Key::Tab, &n);
        assert_eq!(p.scroll, 4);
        let shown = rows(&mut p, &n, 40, 4);
        assert_eq!(shown[1..3], ["    ○ B …", "    ○ C"]);
        // Above every heading: the first on the screen.
        let n = notes(vec![note(1, "", "i", "intro\n\n# H\n\nx")]);
        let mut p = Pager::open(0);
        rows(&mut p, &n, 40, 12);
        p.key(Key::Tab, &n);
        assert_eq!(rows(&mut p, &n, 40, 12)[3], "  ◉ H …");
        let n = notes(vec![note(1, "", "p", "plain\n\ntext")]);
        let mut p = Pager::open(0);
        rows(&mut p, &n, 40, 12);
        p.key(Key::Tab, &n);
        assert_eq!(p.notice, Some("No heading here to fold."));
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
        assert_eq!(p.scroll, 4);
        p.key(Key::Tab, &n);
        p.key(Key::Home, &n);
        p.key(Key::Tab, &n);
        p.key(Key::Tab, &n);
        let shown = rows(&mut p, &n, 40, 12);
        assert_eq!(shown[1..6], ["  ◉ A", "", "    a1", "", "    ○ B …"]);
        assert_eq!(shown[6], "    ○ C");
        p.key(Key::Tab, &n);
        p.key(Key::Char('n'), &n);
        assert_eq!(p.selected, Some(0));
        assert!(p.folded.is_empty());
        assert!(p.on_screen().any(|line| line == 6), "b1 shown");
        // The link stays selected while A is folded over it; following
        // it jumps to C, inside A, which opens.
        p.key(Key::Home, &n);
        p.key(Key::Tab, &n);
        assert_eq!(p.folded.len(), 1);
        p.key(Key::Enter, &n);
        rows(&mut p, &n, 40, 12);
        assert!(p.folded.is_empty());
        assert_eq!(p.scroll, p.max_scroll(), "C as high as it goes");
        assert!(p.on_screen().any(|line| line == 8));
    }

    /// A mouse report at a cell (decision 0076).
    fn at(event: MouseEvent, column: u16, row: u16) -> Key {
        Key::Mouse { event, column, row }
    }

    /// A heading's marker shows only while the mouse is over its gutter,
    /// bold orange: `v` while its section is open, `>` once folded.
    #[test]
    fn heading_markers_show_on_hover() {
        let n = outline();
        let mut p = Pager::open(0);
        let mut grid = Grid::new(40, 12);
        let gutter = |p: &mut Pager, grid: &mut Grid| -> Vec<String> {
            grid.reset();
            p.draw(grid, &n, None);
            (1..11)
                .map(|y| grid.cell(0, y).unwrap().symbol().to_owned())
                .collect()
        };
        // Rows 1 to 10: lines 0 to 9; A 0, B 4, C 8. No mouse: no marker.
        assert!(gutter(&mut p, &mut grid).iter().all(|s| s == " "));
        // Over B's gutter: `v`, bold orange, there only.
        p.key(at(MouseEvent::Move, 0, 5), &n);
        let shown = gutter(&mut p, &mut grid);
        assert_eq!(shown[4], "v");
        assert_eq!(shown.iter().filter(|s| *s != " ").count(), 1);
        assert_eq!(grid.cell(0, 5).unwrap().style(), MARKER);
        assert_eq!(MARKER.fg, Color::Indexed(208));
        assert!(MARKER.mods.contains(Mods::BOLD));
        // Folded by a click: `>`; unfolded again: `v`.
        p.key(at(MouseEvent::Press, 0, 5), &n);
        assert_eq!(gutter(&mut p, &mut grid)[4], ">");
        p.key(at(MouseEvent::Press, 0, 5), &n);
        assert_eq!(gutter(&mut p, &mut grid)[4], "v");
        for (column, row) in [(4, 4), (0, 3), (0, 11), (0, 0)] {
            p.key(at(MouseEvent::Move, column, row), &n);
            assert_eq!(p.hover, None, "{column},{row}");
        }
        assert!(gutter(&mut p, &mut grid).iter().all(|s| s == " "));
        // Anywhere on the heading's row shows it: its text, the end.
        for column in [1, 6, 39] {
            p.key(at(MouseEvent::Move, column, 9), &n);
            assert_eq!(p.hover, Some(9), "{column}");
        }
        assert_eq!(gutter(&mut p, &mut grid)[8], "v");
    }

    /// A press on a heading's row, its marker or its text, folds its
    /// section, another unfolds it; presses on other rows, drags and
    /// releases change nothing. The wheel scrolls three rows.
    #[test]
    fn a_click_on_a_marker_folds_and_the_wheel_scrolls() {
        let n = outline();
        let mut p = Pager::open(0);
        rows(&mut p, &n, 40, 12);
        p.key(at(MouseEvent::Press, 0, 5), &n);
        let shown = rows(&mut p, &n, 40, 12);
        assert_eq!(shown[5..7], ["    ○ B …", "    ○ C"]);
        // On its text: unfolds; past its end: folds again.
        p.key(at(MouseEvent::Press, 6, 5), &n);
        assert!(p.folded.is_empty());
        p.key(at(MouseEvent::Press, 30, 5), &n);
        assert_eq!(p.folded.len(), 1);
        p.key(at(MouseEvent::Press, 0, 5), &n);
        assert!(p.folded.is_empty());
        for event in [MouseEvent::Press, MouseEvent::Drag, MouseEvent::Release] {
            for (column, row) in [(4, 2), (0, 3), (0, 0), (0, 11), (39, 4)] {
                p.key(at(event, column, row), &n);
            }
        }
        assert!(p.folded.is_empty());
        assert_eq!(p.scroll, 0);
        p.key(at(MouseEvent::WheelDown, 10, 3), &n);
        assert_eq!(p.scroll, 3);
        p.key(at(MouseEvent::WheelDown, 10, 3), &n);
        assert_eq!(p.scroll, p.max_scroll(), "stops at the last screen");
        p.key(at(MouseEvent::WheelUp, 10, 3), &n);
        assert_eq!(p.scroll, 2);
        // Scrolled: the marker on a row is that row's heading's. Row 3 is
        // line 4, B.
        rows(&mut p, &n, 40, 12);
        p.key(at(MouseEvent::Press, 0, 3), &n);
        assert_eq!(p.folded.len(), 1);
        assert_eq!(p.page.as_ref().unwrap().sections()[p.folded[0]].line, 4);
        // With the backlinks list open, the mouse does nothing.
        let n = sample();
        let mut p = Pager::open(0);
        rows(&mut p, &n, 60, 10);
        p.key(Key::Char('b'), &n);
        rows(&mut p, &n, 60, 10);
        p.key(at(MouseEvent::WheelDown, 5, 5), &n);
        p.key(at(MouseEvent::Press, 0, 1), &n);
        assert!(p.backlinks.is_some() && p.folded.is_empty());
    }

    /// A press on a `[[link]]` follows it as Enter does, a broken one
    /// says why; on a web link it only says to Ctrl+click; next to a link,
    /// nothing.
    #[test]
    fn a_click_follows_a_link() {
        let n = notes(vec![
            note(
                1,
                "",
                "home",
                "See [[b]] or [web](https://example.org).\n\n[[gone]]",
            ),
            note(2, "", "b", "# B\n\nbody"),
        ]);
        let mut p = Pager::open(0);
        let shown = rows(&mut p, &n, 60, 10);
        // `  See b or web.`: `b` at column 6, `web` from 11.
        assert_eq!(shown[1], "  See b or web.");
        for column in [5, 7] {
            p.key(at(MouseEvent::Press, column, 1), &n);
        }
        assert_eq!(p.note, 0, "beside the link");
        p.key(at(MouseEvent::Press, 11, 1), &n);
        assert_eq!((p.note, p.notice), (0, Some(WEB_LINK)));
        p.key(at(MouseEvent::Press, 6, 1), &n);
        assert_eq!(rows(&mut p, &n, 60, 10)[0], " b");
        p.key(Key::Backspace, &n);
        rows(&mut p, &n, 60, 10);
        p.key(at(MouseEvent::Press, 4, 3), &n);
        assert_eq!(p.notice, Some("no note with that name"));
        assert_eq!(p.note, 0);
        // A link in a heading: on the link it follows, elsewhere on the
        // row the section folds.
        let n = notes(vec![
            note(1, "", "h", "# To [[b]]\n\ntext"),
            note(2, "", "b", "B"),
        ]);
        let mut p = Pager::open(0);
        assert_eq!(rows(&mut p, &n, 40, 8)[1], "  ◉ To b");
        p.key(at(MouseEvent::Press, 3, 1), &n);
        assert_eq!(p.folded.len(), 1);
        p.key(at(MouseEvent::Press, 7, 1), &n);
        assert_eq!(rows(&mut p, &n, 40, 8)[0], " b");
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
        assert_eq!(p.scroll, 3);
        p.key(Key::PageDown, &n);
        assert_eq!(p.scroll, 9);
        p.key(Key::End, &n);
        assert_eq!(p.scroll, 12);
        assert_eq!(p.max_scroll(), 12);
        p.key(Key::PageUp, &n);
        assert_eq!(p.scroll, 9);
        let shown = rows(&mut p, &n, 40, 5);
        assert_eq!(shown[1..4], ["", "      c1", ""]);
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

    /// The gutter is blank (headings' markers aside): no line numbers,
    /// however wide the screen (the editor shows them).
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
        assert_eq!(shown[1], "  l1");
        assert_eq!(shown[9], "  l9");
        assert_eq!(shown[10], "  words words words words");
        assert_eq!(shown[11], "  words");
        let shown = rows(&mut p, &n, 80, 14);
        assert_eq!(shown[10], "  words words words words words");
    }

    /// y copies the first note line shown (the whole note line, from a
    /// wrapped row too); with none on the screen it says so.
    #[test]
    fn y_copies_the_first_line_shown() {
        let n = notes(vec![note(
            1,
            "",
            "y",
            "# T\r\n\nwords words words words words",
        )]);
        let mut p = Pager::open(0);
        // One row of note: rows 0 `# T`, 1 blank, 2 and 3 the words.
        rows(&mut p, &n, 30, 3);
        assert_eq!(p.key(Key::Char('y'), &n), PagerAction::CopyLine(0, 1));
        p.key(Key::Char('j'), &n);
        assert_eq!(p.key(Key::Char('y'), &n), PagerAction::Stay);
        assert_eq!(p.notice, Some("No line here to copy."));
        p.key(Key::Char('j'), &n);
        assert_eq!(p.key(Key::Char('y'), &n), PagerAction::CopyLine(0, 2));
        p.key(Key::Char('j'), &n);
        assert_eq!(p.scroll, 3, "on the wrapped row");
        assert_eq!(p.key(Key::Char('y'), &n), PagerAction::CopyLine(0, 2));
        // Two rows: below a blank top, the first line with text.
        rows(&mut p, &n, 30, 4);
        p.key(Key::Char('g'), &n);
        p.key(Key::Char('j'), &n);
        assert_eq!(p.key(Key::Char('y'), &n), PagerAction::CopyLine(0, 2));
        let text = n.note(0).body.as_str();
        assert_eq!(lines::numbered(text, 1), "# T");
        assert_eq!(lines::numbered(text, 2), "words words words words words");
    }

    proptest::proptest! {
        /// Any keys and mouse events on any note and screen: the top row
        /// is a shown line of the page and the last screen full; a link n
        /// or N selects is on the screen; a marker shows only where the
        /// mouse is, on a heading, `v` or `>` by its fold.
        #[test]
        fn any_keys_keep_the_scroll_in_bounds(
            parts in proptest::collection::vec(0usize..8, 0..12),
            keys in proptest::collection::vec(0usize..19, 0..40),
            cells in proptest::collection::vec((0u16..4, 0u16..14), 40),
            width in 8u16..40,
            height in 3u16..12,
        ) {
            let pieces = [
                "# A".to_owned(),
                "## B".to_owned(),
                "text [[b]]".to_owned(),
                "[[b#H]] and [[b#^k]]".to_owned(),
                "word ".repeat(12),
                "| x |\n|-|\n| y |".to_owned(),
                "- item [[nope]]".to_owned(),
                "x".to_owned(),
            ];
            let body: Vec<&str> = parts.iter().map(|&i| pieces[i].as_str()).collect();
            let n = notes(vec![
                note(1, "", "a", &body.join("\n\n")),
                note(2, "", "b", "# H\n\nx ^k"),
            ]);
            let mut p = Pager::open(0);
            rows(&mut p, &n, width, height);
            for (k, (column, row)) in keys.into_iter().zip(cells) {
                let mouse = |event| Key::Mouse { event, column, row };
                let key = [
                    Key::Char('j'), Key::Char('k'), Key::Char(' '), Key::PageUp,
                    Key::PageDown, Key::Char('g'), Key::Char('G'), Key::Tab,
                    Key::Char('n'), Key::Char('N'), Key::Enter, Key::Backspace,
                    Key::Char('y'),
                    mouse(MouseEvent::Press), mouse(MouseEvent::Move), mouse(MouseEvent::Drag),
                    mouse(MouseEvent::Release), mouse(MouseEvent::WheelUp),
                    mouse(MouseEvent::WheelDown),
                ]
                .into_iter()
                .nth(k)
                .unwrap();
                let selects = matches!(key, Key::Char('n' | 'N'));
                p.key(key, &n);
                if selects && let (Some(i), Some(page)) = (p.selected, &p.page) {
                    let line = page.link_line(i);
                    proptest::prop_assert!(line.is_none_or(|l| p.on_screen().any(|s| s == l)));
                }
                rows(&mut p, &n, width, height);
                proptest::prop_assert!(p.scroll <= p.max_scroll());
                proptest::prop_assert!(p.lines() == 0 || p.scroll < p.lines());
                proptest::prop_assert_eq!(p.shown_at(p.scroll), p.scroll);
                // A marker shows only on the row the mouse is over, on a
                // heading's row: `>` if its section is folded, else `v`.
                let mut grid = Grid::new(width, height);
                p.draw(&mut grid, &n, None);
                for y in 1..height.saturating_sub(1) {
                    let symbol = grid.cell(0, y).unwrap().symbol();
                    if symbol != " " && p.backlinks.is_none() {
                        proptest::prop_assert_eq!(p.hover, Some(y));
                        let line = y.checked_sub(p.top_row).and_then(|r| p.drawn.get(usize::from(r)));
                        let page = p.page.as_ref().unwrap();
                        let i = line.and_then(|&l| page.sections().iter().position(|s| s.line == l));
                        proptest::prop_assert!(i.is_some());
                        let folded = i.is_some_and(|i| p.folded.contains(&i));
                        proptest::prop_assert_eq!(symbol, if folded { ">" } else { "v" });
                    }
                }
            }
        }
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
