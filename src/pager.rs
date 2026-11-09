//! `view NAME`: a note, rendered full screen (docs/decisions/0033-shell.md).
//!
//! The title on the top row, the note below, a key line at the bottom.
//! j/k and the arrows scroll by a line, Space and PgDn/PgUp by a screen,
//! g/G or Home/End to the ends. Tab and Shift+Tab select `[[links]]`, Enter
//! follows one (to a note, a heading or a `^block`), Backspace goes back. b
//! lists the notes linking here. y copies the note into Crystal's own
//! clipboard (docs/decisions/0041-internal-clipboard-only.md). q or Esc
//! returns to the shell.
//!
//! Like the shell, it is terminal-free: keys in, frames out. A page is
//! rendered for the current width and again after a resize.

use zeroize::Zeroizing;

use crate::md::render::{Page, kind_of, render};
use crate::notes::{Fragment, Notes};
use crate::shell::keys::Key;
use crate::ui::grid::{Color, Grid, Mods, Style};
use crate::ui::layout::Rect;

const TITLE: Style = Style::PLAIN.mods(Mods::REVERSED.union(Mods::BOLD));
const HINT: Style = Style::PLAIN.mods(Mods::DIM);
const NOTICE: Style = Style::PLAIN.fg(Color::Yellow);
const SELECTED: Style = Style::PLAIN.mods(Mods::REVERSED);
const HINTS: &str = "q back to shell · j/k scroll · Tab links · Enter follow · Backspace \
                     previous · b backlinks · y copy";

/// After a key: stay, or go back to the shell.
#[derive(Debug, PartialEq, Eq)]
pub enum PagerAction {
    Stay,
    Close,
    /// Copy this note into Crystal's own clipboard.
    Copy(usize),
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
    /// Notes left by following links, with their scroll positions.
    history: Vec<(usize, usize)>,
    page: Option<Page>,
    width: u16,
    scroll: usize,
    /// Rows the note had when last drawn.
    rows: usize,
    selected: Option<usize>,
    jump: Option<Jump>,
    backlinks: Option<Backlinks>,
    notice: Option<&'static str>,
}

impl Pager {
    pub fn open(note: usize) -> Self {
        Self {
            note,
            history: Vec::new(),
            page: None,
            width: 0,
            scroll: 0,
            rows: 1,
            selected: None,
            jump: None,
            backlinks: None,
            notice: None,
        }
    }

    fn lines(&self) -> usize {
        self.page.as_ref().map_or(0, Page::line_count)
    }

    fn max_scroll(&self) -> usize {
        self.lines().saturating_sub(self.rows)
    }

    pub fn key(&mut self, key: Key, notes: &Notes) -> PagerAction {
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
            Key::Char('y') => return PagerAction::Copy(self.note),
            Key::Char('j') | Key::Down => self.scroll_to(self.scroll + 1),
            Key::Char('k') | Key::Up => self.scroll_to(self.scroll.saturating_sub(1)),
            Key::Char(' ') | Key::PageDown => self.scroll_to(self.scroll + page),
            Key::PageUp => self.scroll_to(self.scroll.saturating_sub(page)),
            Key::Char('g') | Key::Home => self.scroll_to(0),
            Key::Char('G') | Key::End => self.scroll_to(usize::MAX),
            Key::Tab => self.select(true),
            Key::BackTab => self.select(false),
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

    fn scroll_to(&mut self, line: usize) {
        self.scroll = line.min(self.max_scroll());
    }

    /// Tab: the next link (from the first visible line if none is
    /// selected); Shift+Tab: the previous one. Scrolls to it.
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
                .find(|&i| page.link_line(i).is_some_and(|l| l >= self.scroll))
                .unwrap_or(0),
            (None, false) => count - 1,
        };
        self.selected = Some(next);
        if let Some(line) = page.link_line(next)
            && (line < self.scroll || line >= self.scroll + self.rows)
        {
            self.scroll_to(line.saturating_sub(2));
        }
    }

    /// Enter: follow the selected link, or say why it leads nowhere.
    fn follow(&mut self, notes: &Notes) {
        let (Some(page), Some(selected)) = (&self.page, self.selected) else {
            self.notice = Some("Select a link with Tab first.");
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
    pub fn draw(&mut self, grid: &mut Grid, notes: &Notes) {
        let area = grid.area();
        let (top, rest) = area.split_top(1);
        let (body, bottom) = rest.split_bottom(1);
        self.rows = usize::from(body.height).max(1);
        if self.page.is_none() || self.width != body.width {
            let note = notes.note(self.note);
            self.page = Some(render(
                note.body.as_str(),
                kind_of(note.title.as_str()),
                body.width,
            ));
            self.width = body.width;
            self.selected = None;
        }
        if let (Some(jump), Some(page)) = (self.jump.take(), &self.page) {
            let line = match &jump {
                Jump::Heading(h) => page.heading_line(h),
                Jump::Block(id) => page.block_line(id),
            };
            match line {
                Some(line) => self.scroll = line,
                None => self.notice = Some("That heading or block is not on the page."),
            }
        }
        self.scroll = self.scroll.min(self.max_scroll());

        grid.fill(top, TITLE);
        grid.put_str(
            top.x + 1,
            top.y,
            notes.name(self.note),
            TITLE,
            top.width.saturating_sub(2),
        );
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
            page.draw_selected(grid, body, self.scroll, self.selected);
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
        pager.draw(&mut grid, notes);
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
        assert_eq!(shown[1], "Start at b or b#Second or b#^k.");
        assert!(shown[5].starts_with("q back to shell"));
    }

    #[test]
    fn scrolling_stays_in_bounds() {
        let n = sample();
        let mut p = Pager::open(1);
        rows(&mut p, &n, 40, 10);
        p.key(Key::End, &n);
        let bottom = p.scroll;
        assert_eq!(bottom, p.max_scroll());
        p.key(Key::Char('j'), &n);
        assert_eq!(p.scroll, bottom);
        p.key(Key::Char('g'), &n);
        assert_eq!(p.scroll, 0);
        p.key(Key::PageDown, &n);
        assert_eq!(p.scroll, 8);
        p.key(Key::Char('k'), &n);
        assert_eq!(p.scroll, 7);
    }

    #[test]
    fn links_are_selected_followed_and_left() {
        let n = sample();
        let mut p = Pager::open(0);
        rows(&mut p, &n, 60, 10);
        assert_eq!(p.key(Key::Enter, &n), PagerAction::Stay);
        assert_eq!(p.notice, Some("Select a link with Tab first."));
        p.key(Key::Tab, &n);
        p.key(Key::Tab, &n);
        assert_eq!(p.selected, Some(1));
        p.key(Key::Enter, &n);
        let shown = rows(&mut p, &n, 60, 10);
        assert_eq!(shown[0], " b");
        // Near the end the page cannot scroll further, so the heading is
        // shown lower down, not on the first row.
        assert!(
            shown.contains(&"## Second".to_owned()),
            "jumped to the heading: {shown:?}"
        );
        assert_eq!(p.scroll, p.max_scroll());
        p.key(Key::Backspace, &n);
        let shown = rows(&mut p, &n, 60, 10);
        assert_eq!(shown[0], " home");
        // The block link.
        p.key(Key::BackTab, &n);
        p.key(Key::BackTab, &n);
        assert_eq!(p.selected, Some(2));
        p.key(Key::Enter, &n);
        let shown = rows(&mut p, &n, 60, 10);
        assert!(
            shown.contains(&"Last.".to_owned()),
            "jumped to the block, ^id hidden: {shown:?}"
        );
        // A broken link says why.
        p.key(Key::Backspace, &n);
        rows(&mut p, &n, 60, 10);
        p.key(Key::BackTab, &n);
        p.key(Key::Enter, &n);
        assert_eq!(p.notice, Some("no note with that name"));
        assert_eq!(p.key(Key::Char('y'), &n), PagerAction::Copy(0));
        assert_eq!(
            p.key(Key::Char('Y'), &n),
            PagerAction::Stay,
            "no system copy"
        );
        assert_eq!(p.key(Key::Char('q'), &n), PagerAction::Close);
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
