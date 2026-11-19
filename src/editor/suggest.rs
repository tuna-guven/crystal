//! Link suggestions in the editor (roadmap W1 to W6): the name typed
//! after an open `[[`, the notes it matches, and the text that links to
//! a chosen note, and the editor's list of them (`List`, `Editor::key_in`).
//! Everything works on ranges of the text and note indexes, never on
//! copies of names.
//!
//! The list opens after a key that types or deletes (a character,
//! Backspace, Delete) leaves the cursor in a link's name with notes
//! matching; it is computed again after each such key. While it is open,
//! Up and Down move the highlight (wrapping), Enter and Tab link the
//! highlighted note, Esc closes it; any other key closes it and acts as
//! always. With the mouse (decision 0073, `Editor::mouse_in`), a click on
//! an entry links that note and the wheel over the list moves the
//! highlight; anything else closes it and acts as always.

use std::ops::Range;
use std::time::Instant;

use super::{Editor, EditorAction};
use crate::md::blocks::{Block, blocks};
use crate::md::render::{Kind, kind_of};
use crate::notes::{Found, Notes, cmp_ci, headings_in};
use crate::shell::keys::{Key, MouseEvent};
use crate::ui::clipboard::Clipboard;
use zeroize::Zeroizing;

/// Rows the list shows at once; the rest scroll.
pub const ROWS: usize = 8;
/// The most notes the list holds: the best matches; typing more finds
/// the others.
pub const MAX_MATCHES: usize = 100;

/// What the open list's entries index into.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The vault's notes (`Notes`).
    Notes,
    /// The headings of a saved note (`Notes::headings`), or with `None`
    /// those of the note being edited, scanned from its text.
    Headings(Option<usize>),
    /// The blocks (`md::blocks`) of a saved note, or with `None` of the
    /// editor's text, kept as ranges in `Editor::blocks`.
    Blocks(Option<usize>),
}

/// Whose blocks `Editor::blocks` holds, for which link, and how long the
/// editor's text was then: typing in that link reuses them, shifted.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BlocksFor {
    note: Option<usize>,
    link: usize,
    len: usize,
}

/// Shown when a block without an id is chosen, until roadmap steps J5
/// and J6 give it one.
const NO_ID: &str = "That paragraph has no ^id to link to yet.";

/// The open list. No Debug, like the editor that holds it.
pub struct List {
    /// What `entries` index into.
    pub source: Source,
    /// The matches, best first, as indexes into `source`; never empty.
    pub entries: Vec<usize>,
    /// The highlighted row, an index into `entries`.
    pub highlight: usize,
    /// The first row shown; the highlight is always within `ROWS` of it.
    pub top: usize,
}

impl List {
    /// Moves the highlight one row, wrapping at either end, and scrolls
    /// just enough to keep it shown.
    fn step(&mut self, down: bool) {
        let len = self.entries.len();
        if len == 0 {
            return;
        }
        self.highlight = if down {
            (self.highlight + 1) % len
        } else {
            (self.highlight + len - 1) % len
        };
        if self.highlight < self.top {
            self.top = self.highlight;
        } else if self.highlight >= self.top + ROWS {
            self.top = self.highlight + 1 - ROWS;
        }
    }
}

/// Characters a heading must not hold to be linked as written: they end
/// or split a link (decision 0075).
const LINK_CHARS: &[char] = &['[', ']', '|', '#', '^'];

impl Editor {
    /// Which note this editor shows (`None` for one not saved yet) and
    /// whether it is Markdown: `[[#` lists its headings from the text,
    /// and a plain-text note has none. The session sets it on opening
    /// and after the first save.
    pub fn set_note(&mut self, own: Option<usize>, markdown: bool) {
        self.own = own;
        self.markdown = markdown;
    }

    /// `key` with link suggestions over `notes` (see the module comment).
    /// A key the list takes ends a typing run, clears the notice and
    /// disarms Ctrl+Q, like any key that is not typing.
    pub fn key_in(&mut self, key: Key, clipboard: &mut Clipboard, notes: &Notes) -> EditorAction {
        let list_key = matches!(key, Key::Up | Key::Down | Key::Enter | Key::Tab | Key::Esc);
        if list_key && let Some(list) = &mut self.suggestions {
            // Cleared first: choosing can leave a notice.
            self.notice = None;
            match key {
                Key::Up | Key::Down => list.step(key == Key::Down),
                Key::Enter | Key::Tab => self.accept(notes),
                _ => self.close_list(),
            }
            self.quit_armed = false;
            self.last_press = None;
            self.buffer.commit();
            return EditorAction::Stay;
        }
        let typing = matches!(key, Key::Char(_) | Key::Backspace | Key::Delete);
        // Typing or deleting backwards inside a link cannot change the
        // headings elsewhere: the ones scanned for it are kept (the scan
        // of an 8 MiB note took 185 ms a key). Delete can join the next
        // line, so it scans again, like any other key.
        let keeps_headings = matches!(key, Key::Char(_) | Key::Backspace);
        let action = self.key(key, clipboard);
        if keeps_headings {
            self.suggestions = None;
        } else {
            self.close_list();
        }
        if typing {
            self.refresh(notes);
        }
        if self.suggestions.is_none() {
            self.close_list();
        }
        action
    }

    /// `mouse` with link suggestions: `entry` is the list entry under the
    /// pointer (`View::suggestion_at`), if any. A press on an entry links
    /// it, the wheel there moves the highlight; like a key, they end a
    /// typing run, clear the notice and disarm Ctrl+Q. Any other event
    /// goes to `mouse`, which closes the list.
    pub fn mouse_in(
        &mut self,
        event: MouseEvent,
        at: Option<usize>,
        entry: Option<usize>,
        now: Instant,
        notes: &Notes,
    ) {
        let on_list = entry
            .zip(self.suggestions.as_mut())
            .filter(|(i, list)| *i < list.entries.len());
        let Some((entry, list)) = on_list else {
            self.mouse(event, at, now);
            return;
        };
        // Cleared first: choosing can leave a notice.
        self.notice = None;
        match event {
            MouseEvent::Press => {
                list.highlight = entry;
                self.accept(notes);
            }
            MouseEvent::WheelUp | MouseEvent::WheelDown => {
                list.step(event == MouseEvent::WheelDown)
            }
            // No drag starts on the list: nothing to do.
            MouseEvent::Drag | MouseEvent::Release => {}
        }
        self.quit_armed = false;
        self.last_press = None;
        self.pressed = false;
        self.buffer.commit();
    }

    /// The open list, for the view.
    pub fn suggestions(&self) -> Option<&List> {
        self.suggestions.as_ref()
    }

    /// The text of the open list's entry at `row` (an index into
    /// `entries`), for the view: a dimmed part, the text, and a dimmed end.
    /// A note is its folder and title, a heading `# ` and its text, a
    /// block its first line and its ` ^id`. None past the list or for an
    /// index its source does not have.
    pub fn suggestion_label<'a>(
        &'a self,
        row: usize,
        notes: &'a Notes,
    ) -> Option<(&'a str, &'a str, &'a str)> {
        let list = self.suggestions.as_ref()?;
        let &entry = list.entries.get(row)?;
        match list.source {
            Source::Notes => {
                let name = (entry < notes.len()).then(|| notes.name(entry))?;
                let (folder, title) = match name.rfind('/') {
                    Some(at) => name.split_at(at + 1),
                    None => ("", name),
                };
                Some((folder, title, ""))
            }
            Source::Headings(note) => Some(("# ", self.heading(note, entry, notes)?, "")),
            Source::Blocks(note) => {
                let body = self.blocks_text(note, notes)?;
                let block = self.blocks.get(entry)?;
                let text = body.get(block.text.clone())?;
                let first = text.split('\n').next().unwrap_or("").trim_end();
                // ` ^id`: the id and the ` ^` before it.
                let id = block
                    .id
                    .clone()
                    .and_then(|id| body.get(id.start.checked_sub(2)?..id.end));
                let first = match (id, text.contains('\n')) {
                    // On one line, the text stops before its id.
                    (Some(id), false) => first.strip_suffix(id).unwrap_or(first).trim_end(),
                    _ => first,
                };
                Some(("", first, id.unwrap_or("")))
            }
        }
    }

    /// The text `Source::Blocks(note)` ranges point into: a saved note's
    /// body, or the editor's text.
    fn blocks_text<'a>(&'a self, note: Option<usize>, notes: &'a Notes) -> Option<&'a str> {
        match note {
            Some(note) if note < notes.len() => Some(notes.note(note).body.as_str()),
            Some(_) => None,
            None => Some(self.text()),
        }
    }

    /// Heading `entry` of `note` (saved) or of the editor's text (`None`).
    fn heading<'a>(
        &'a self,
        note: Option<usize>,
        entry: usize,
        notes: &'a Notes,
    ) -> Option<&'a str> {
        let all = match note {
            Some(note) if note < notes.len() => notes.headings(note),
            Some(_) => return None,
            None => &self.own_headings,
        };
        all.get(entry).map(|h| h.as_str())
    }

    /// Closes the list and drops the headings and blocks scanned for it.
    pub(super) fn close_list(&mut self) {
        self.suggestions = None;
        self.own_headings = Vec::new();
        self.own_headings_at = None;
        self.blocks = Vec::new();
        self.blocks_for = None;
    }

    /// Loads the blocks of `note` (or of the editor's text) for the link
    /// starting at `link`: parsed once per link; while typing in that
    /// link the editor's text changes only after `link`, so the kept
    /// ranges past it move by the change in length (each use checks its
    /// range again). A plain-text note has none.
    fn load_blocks(&mut self, note: Option<usize>, link: usize, notes: &Notes) {
        let len = self.text().len();
        match self.blocks_for {
            Some(f) if f.note == note && f.link == link => {
                if note.is_none() && f.len != len {
                    // The line typed in, as the parser ends lines (`\n`,
                    // `\r`); it ended `len - f.len` earlier before.
                    let text = self.text();
                    let ends = ['\n', '\r'];
                    let line_start = text[..link].rfind(ends).map_or(0, |at| at + 1);
                    let line_end = text[self.cursor..]
                        .find(ends)
                        .map_or(len, |at| self.cursor + at);
                    let old_line_end = (line_end + f.len).saturating_sub(len);
                    let trimmed_end = line_start + text[line_start..line_end].trim_end().len();
                    let moved = |at: usize| match at {
                        at if at <= link => at,
                        at if len > f.len => at + (len - f.len),
                        at => at.saturating_sub(f.len - len),
                    };
                    for block in &mut self.blocks {
                        // A block ending on that line ends where the line's
                        // text does, trailing space left out, as `blocks`
                        // finds it: typing a space there moves nothing,
                        // typing after spaces moves it past them (found by
                        // the editor's property test).
                        let end = block.text.end;
                        let end = if end > link && end <= old_line_end {
                            trimmed_end
                        } else {
                            moved(end)
                        };
                        block.text = moved(block.text.start)..end;
                        block.id = block.id.clone().map(|id| moved(id.start)..moved(id.end));
                    }
                }
            }
            _ => {
                self.blocks = match note {
                    Some(note) if note < notes.len() => {
                        let saved = notes.note(note);
                        if kind_of(saved.title.as_str()) == Kind::Markdown {
                            blocks(saved.body.as_str())
                        } else {
                            Vec::new()
                        }
                    }
                    Some(_) => Vec::new(),
                    None if self.markdown => blocks(self.text()),
                    None => Vec::new(),
                };
            }
        }
        self.blocks_for = Some(BlocksFor { note, link, len });
    }

    /// Opens the list for the cursor's place: only with nothing selected,
    /// in a link's name or after its `#`, and with at least one match.
    fn refresh(&mut self, notes: &Notes) {
        if self.selection().is_some() {
            return;
        }
        let Some(q) = target(self.text(), self.cursor) else {
            return;
        };
        let (source, found) = match q.fragment {
            None => (Source::Notes, matches(&self.text()[q.name], notes)),
            Some((Part::Heading, typed)) => {
                let Some(note) = self.fragment_note(&self.text()[q.name.clone()], notes) else {
                    return;
                };
                // Scanned once per link (`key_in` keeps them while typing
                // in it); a plain-text note has none.
                if note.is_none() && self.own_headings_at != Some(q.name.start) {
                    self.own_headings = if self.markdown {
                        headings_in(self.text())
                    } else {
                        Vec::new()
                    };
                    self.own_headings_at = Some(q.name.start);
                }
                let all = match note {
                    Some(note) => notes.headings(note),
                    None => &self.own_headings,
                };
                let found = heading_matches(&self.text()[typed], all);
                (Source::Headings(note), found)
            }
            Some((Part::Block, typed)) => {
                let Some(note) = self.fragment_note(&self.text()[q.name.clone()], notes) else {
                    return;
                };
                self.load_blocks(note, q.name.start, notes);
                let Some(body) = self.blocks_text(note, notes) else {
                    return;
                };
                let found = block_matches(&self.text()[typed], body, &self.blocks);
                (Source::Blocks(note), found)
            }
        };
        if found.is_empty() {
            self.close_list();
        } else {
            self.suggestions = Some(List {
                source,
                entries: found,
                highlight: 0,
                top: 0,
            });
        }
    }

    /// Whose headings or blocks `[[name#` lists: `Some(None)` for the
    /// note being edited (an empty name, or its own), `Some(Some(note))`
    /// for the one other note `name` finds, `None` when it finds none or
    /// several.
    fn fragment_note(&self, name: &str, notes: &Notes) -> Option<Option<usize>> {
        if name.is_empty() {
            return Some(None);
        }
        match notes.find(name) {
            Found::One(note) if Some(note) == self.own => Some(None),
            Found::One(note) => Some(Some(note)),
            _ => None,
        }
    }

    /// Enter, Tab or a click: the typed name, or the text after `#`,
    /// becomes the highlighted entry's link text, and the cursor goes past
    /// the `]]` after it, added if missing. One undo step (the caller
    /// commits before; this commits after).
    fn accept(&mut self, notes: &Notes) {
        let list = self.suggestions.take();
        let own_headings = std::mem::take(&mut self.own_headings);
        let all_blocks = std::mem::take(&mut self.blocks);
        self.close_list();
        let (Some(list), Some(q)) = (list, target(self.text(), self.cursor)) else {
            return;
        };
        // A block's id, copied out of the text it may be in (zeroizing).
        let id: Zeroizing<String>;
        // The list was made from these notes and this text; the checks
        // keep a stale index from panicking.
        let Some(&entry) = list.entries.get(list.highlight) else {
            return;
        };
        let (range, link) = match (list.source, q.fragment) {
            (Source::Notes, None) if entry < notes.len() => (q.name, link_text(notes, entry)),
            (Source::Headings(note), Some((Part::Heading, typed))) => {
                let heading = match note {
                    Some(note) if note < notes.len() => notes.headings(note).get(entry),
                    Some(_) => None,
                    None => own_headings.get(entry),
                };
                let Some(heading) = heading else {
                    return;
                };
                (typed, heading.as_str())
            }
            (Source::Blocks(note), Some((Part::Block, typed))) => {
                let Some(block) = all_blocks.get(entry) else {
                    return;
                };
                let found = self
                    .blocks_text(note, notes)
                    .zip(block.id.clone())
                    .and_then(|(body, range)| body.get(range));
                let Some(found) = found else {
                    // Roadmap steps J5 and J6 give it one.
                    self.notice = Some(NO_ID);
                    return;
                };
                id = Zeroizing::new(found.to_owned());
                (typed, id.as_str())
            }
            _ => return,
        };
        self.buffer.commit();
        self.anchor = Some(range.start);
        if self.replace_selection(link) {
            if self.text()[self.cursor..].starts_with("]]") {
                // `collapse` snaps: a `]` can carry an accent.
                self.collapse(self.cursor + 2);
            } else {
                self.replace_selection("]]");
            }
        }
        self.buffer.commit();
    }
}

/// The headings among `all` that hold every word of `query`, ignoring
/// case, as indexes: those starting with the whole query first, then in
/// the note's order; at most `MAX_MATCHES`. Headings that cannot be
/// linked as written (`LINK_CHARS`, or empty) are left out.
pub fn heading_matches(query: &str, all: &[Zeroizing<String>]) -> Vec<usize> {
    let whole = query.trim();
    let mut found: Vec<usize> = (0..all.len())
        .filter(|&i| {
            let h = all[i].as_str();
            !h.trim().is_empty()
                && !h.contains(LINK_CHARS)
                && query.split_whitespace().all(|word| contains_ci(h, word))
        })
        .collect();
    // Stable: the note's order within each group.
    found.sort_by_key(|&i| !starts_ci(&all[i], whole));
    found.truncate(MAX_MATCHES);
    found
}

/// The blocks among `all` (ranges into `body`) whose text holds every
/// word of `query`, ignoring case, as indexes: those starting with the
/// whole query first, then in the note's order; at most `MAX_MATCHES`.
pub fn block_matches(query: &str, body: &str, all: &[Block]) -> Vec<usize> {
    let whole = query.trim();
    let text = |i: usize| body.get(all[i].text.clone());
    let mut found: Vec<usize> = (0..all.len())
        .filter(|&i| {
            text(i).is_some_and(|t| query.split_whitespace().all(|word| contains_ci(t, word)))
        })
        .collect();
    found.sort_by_key(|&i| !text(i).is_some_and(|t| starts_ci(t, whole)));
    found.truncate(MAX_MATCHES);
    found
}

/// Characters that end the name part of a link: `]` closes it, `|`
/// starts its text, `#` a heading, `^` a block. No name holds any of
/// them, nor `[` (decision 0034).
const ENDS_NAME: &[char] = &['[', ']', '|', '#', '^'];

/// What a link's fragment points into (decision 0075).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// `#text`: a heading.
    Heading,
    /// `#^text`: a block.
    Block,
}

/// The link the cursor is typing (`target`): ranges into the text, never
/// copies. No Debug, as it points into note text.
pub struct Query {
    /// The note's name as typed; empty before `#` for the note being
    /// edited.
    pub name: Range<usize>,
    /// After `#` (or `#^`): what kind, and the text typed after it, which
    /// ends at the cursor.
    pub fragment: Option<(Part, Range<usize>)>,
}

/// The link the cursor is typing: from just after the last `[[` before
/// `cursor` on its line (`![[` counts too) up to `cursor`. A name is
/// followed by at most one `#`: `name#text` is a heading, `name#^text` a
/// block, with an empty name for the note being edited. None when the
/// cursor is not inside such a link, or not on a char boundary: `[`, `]`
/// or `|` anywhere, a second `#`, or a `^` anywhere else.
pub fn target(text: &str, cursor: usize) -> Option<Query> {
    let before = text.get(..cursor)?;
    let line = before.rfind('\n').map_or(0, |at| at + 1);
    let start = line + before[line..].rfind("[[")? + 2;
    let typed = &before[start..];
    let Some(hash) = typed.find('#') else {
        return (!typed.contains(ENDS_NAME)).then_some(Query {
            name: start..cursor,
            fragment: None,
        });
    };
    let name = start..start + hash;
    let after = start + hash + 1;
    let (part, from) = if before[after..].starts_with('^') {
        (Part::Block, after + 1)
    } else {
        (Part::Heading, after)
    };
    let valid = !text[name.clone()].contains(ENDS_NAME) && !before[from..].contains(ENDS_NAME);
    valid.then_some(Query {
        name,
        fragment: Some((part, from..cursor)),
    })
}

/// The notes whose full name holds every word of `query`, ignoring case,
/// as indexes: those whose title starts with the whole query first, then
/// by name (`cmp_ci`, as `ls` sorts), at most `MAX_MATCHES` of them. A
/// blank query matches every note.
pub fn matches(query: &str, notes: &Notes) -> Vec<usize> {
    let whole = query.trim();
    let mut found: Vec<usize> = (0..notes.len())
        .filter(|&i| {
            let name = notes.name(i);
            query.split_whitespace().all(|word| contains_ci(name, word))
        })
        .collect();
    // `true` sorts after `false`: put "starts with" first by comparing
    // the negation.
    let order = |&a: &usize, &b: &usize| {
        let late = |i: usize| !starts_ci(notes.note(i).title.as_str(), whole);
        late(a)
            .cmp(&late(b))
            .then_with(|| cmp_ci(notes.name(a), notes.name(b)))
    };
    // Only the best `MAX_MATCHES` are kept, found without sorting the rest:
    // in a vault of 100 000 notes, sorting every match cost 170 ms a key.
    if found.len() > MAX_MATCHES {
        found.select_nth_unstable_by(MAX_MATCHES - 1, order);
        found.truncate(MAX_MATCHES);
    }
    found.sort_by(order);
    found
}

/// The text a link to note `index` is written with: its title when that
/// title finds this note alone (`Notes::find`, the resolver's own rule),
/// else its full name `folder/title`.
pub fn link_text(notes: &Notes, index: usize) -> &str {
    let title = notes.note(index).title.as_str();
    if notes.find(title) == Found::One(index) {
        title
    } else {
        notes.name(index)
    }
}

/// `hay` starts with `prefix`, ignoring case, without lowercase copies.
fn starts_ci(hay: &str, prefix: &str) -> bool {
    let mut hay = hay.chars().flat_map(char::to_lowercase);
    prefix
        .chars()
        .flat_map(char::to_lowercase)
        .all(|c| hay.next() == Some(c))
}

/// `hay` holds `needle` somewhere, ignoring case.
fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.char_indices()
        .any(|(at, _)| starts_ci(&hay[at..], needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::tests::{note, notes};
    use proptest::prelude::*;

    /// A note name the cursor is typing, with no fragment after it (the
    /// W1 query).
    fn query(text: &str, cursor: usize) -> Option<Range<usize>> {
        target(text, cursor)
            .filter(|q| q.fragment.is_none())
            .map(|q| q.name)
    }

    /// The query as text, for short asserts.
    fn q(text: &str, cursor: usize) -> Option<&str> {
        query(text, cursor).map(|r| &text[r])
    }

    #[test]
    fn query_is_the_name_after_the_last_open_link() {
        assert_eq!(q("[[", 2), Some(""));
        assert_eq!(q("[[]]", 2), Some(""));
        assert_eq!(q("see [[pla]]", 9), Some("pla"));
        assert_eq!(q("see ![[pla", 10), Some("pla"));
        assert_eq!(q("[[a]] and [[b", 13), Some("b"));
        assert_eq!(q("[[work/pl an", 12), Some("work/pl an"));
        assert_eq!(q("[[[x", 4), Some("x"));
        assert_eq!(q("[[Ünï", "[[Ünï".len()), Some("Ünï"));
    }

    #[test]
    fn no_query_outside_a_links_name() {
        // Before the `[[`, inside it, and with no `[[` at all.
        assert_eq!(q("ab [[c", 2), None);
        assert_eq!(q("[[c", 1), None);
        assert_eq!(q("plain", 5), None);
        // A closed link, its text, a heading, a block, a stray `[`.
        for text in ["[[a]]x", "[[a|b", "[[a#b", "[[a^b", "[[a[b"] {
            assert_eq!(q(text, text.len()), None, "{text:?}");
        }
        // The `[[` is on an earlier line.
        assert_eq!(q("[[a\nb", 5), None);
        assert_eq!(q("[[a\r\nb", 6), None);
        // Not a char boundary.
        assert_eq!(q("[[é", 3), None);
        // Past the end.
        assert_eq!(q("[[", 3), None);
    }

    /// `target` as text: (name, fragment kind, fragment text).
    fn t(text: &str) -> Option<(&str, Option<(Part, &str)>)> {
        target(text, text.len())
            .map(|q| (&text[q.name], q.fragment.map(|(part, r)| (part, &text[r]))))
    }

    #[test]
    fn fragments_after_the_name() {
        use Part::{Block, Heading};
        assert_eq!(t("[[plan#"), Some(("plan", Some((Heading, "")))));
        assert_eq!(t("[[plan#Che"), Some(("plan", Some((Heading, "Che")))));
        assert_eq!(
            t("[[plan#two words"),
            Some(("plan", Some((Heading, "two words"))))
        );
        assert_eq!(t("[[plan#^"), Some(("plan", Some((Block, "")))));
        assert_eq!(t("[[plan#^ab"), Some(("plan", Some((Block, "ab")))));
        assert_eq!(t("[[#Che"), Some(("", Some((Heading, "Che")))));
        assert_eq!(t("[[#^x"), Some(("", Some((Block, "x")))));
        assert_eq!(
            t("see ![[work/plan#é"),
            Some(("work/plan", Some((Heading, "é"))))
        );
        // No fragment: as before.
        assert_eq!(t("[[pla"), Some(("pla", None)));
        // The W1 query is only the name with no fragment.
        assert_eq!(q("[[plan#x", 8), None);
        assert_eq!(q("[[plan", 6), Some("plan"));
    }

    #[test]
    fn what_ends_a_fragment() {
        for text in [
            "[[plan#a#b",
            "[[plan#a^b",
            "[[plan#^a^b",
            "[[plan#^a#b",
            "[[plan#a]",
            "[[plan#a|b",
            "[[plan#a[",
            "[[pl^an#a",
            "[[pl]an#a",
            "[[plan#a\nb",
        ] {
            assert_eq!(t(text), None, "{text:?}");
        }
    }

    fn sample() -> Notes {
        notes(vec![
            note(1, "", "Restic Backup", ""),
            note(2, "work", "plan", ""),
            note(3, "", "plan", ""),
            note(4, "home", "Garden plan", ""),
            note(5, "home", "todo.txt", ""),
            note(6, "", "Ünïcode", ""),
        ])
    }

    /// The names `matches` gives, in order.
    fn found(query: &str) -> Vec<String> {
        let n = sample();
        matches(query, &n)
            .into_iter()
            .map(|i| n.name(i).to_owned())
            .collect()
    }

    #[test]
    fn blank_query_lists_every_note_by_name() {
        let all = [
            "home/Garden plan",
            "home/todo.txt",
            "plan",
            "Restic Backup",
            "work/plan",
            "Ünïcode",
        ];
        assert_eq!(found(""), all);
        assert_eq!(found("  "), all);
    }

    #[test]
    fn every_word_must_match_ignoring_case() {
        assert_eq!(found("BACK"), ["Restic Backup"]);
        assert_eq!(found("back res"), ["Restic Backup"]);
        assert!(found("back x").is_empty());
        assert_eq!(found("üNÏ"), ["Ünïcode"]);
        // Folders and `.txt` are part of the name.
        assert_eq!(found("home/"), ["home/Garden plan", "home/todo.txt"]);
        assert_eq!(found(".txt"), ["home/todo.txt"]);
    }

    #[test]
    fn titles_starting_with_the_query_come_first() {
        assert_eq!(found("plan"), ["plan", "work/plan", "home/Garden plan"]);
        assert_eq!(found("Gar"), ["home/Garden plan"]);
    }

    #[test]
    fn only_the_best_matches_are_kept() {
        // Notes made in a scrambled order (37 is prime to 300), so the
        // cut must find the best ones, not the first ones.
        let list: Vec<_> = (0..300u16)
            .map(|i| {
                let k = i * 37 % 300;
                let folder = if k % 2 == 0 { "" } else { "x" };
                note(
                    u8::try_from(i % 250).unwrap(),
                    folder,
                    &format!("n{k:03}"),
                    "",
                )
            })
            .collect();
        let mut list = list;
        for (i, n) in list.iter_mut().enumerate() {
            let mut id = [0; 16];
            id[..2].copy_from_slice(&u16::try_from(i).unwrap().to_le_bytes());
            n.id = crate::model::NoteId::from_bytes(id);
        }
        let n = notes(list);
        let names = |query: &str| -> Vec<String> {
            matches(query, &n)
                .into_iter()
                .map(|i| n.name(i).to_owned())
                .collect()
        };
        // Blank: by name, top-level `n000`... before `x/...`.
        let all = names("");
        assert_eq!(all.len(), MAX_MATCHES);
        let expected: Vec<String> = (0..MAX_MATCHES).map(|k| format!("n{:03}", 2 * k)).collect();
        assert_eq!(all, expected);
        // `n1`: the 100 titles starting with it (n100 to n199), by full
        // name: the 50 top-level ones, then the 50 in `x/`.
        let ones = names("n1");
        assert_eq!(ones.len(), MAX_MATCHES);
        assert_eq!(ones[0], "n100");
        assert_eq!(ones[49], "n198");
        assert_eq!(ones[50], "x/n101");
        assert_eq!(ones[MAX_MATCHES - 1], "x/n199");
        // Fewer than the cap: all of them.
        assert_eq!(names("n29").len(), 10);
    }

    #[test]
    fn link_text_is_the_title_unless_another_note_has_it() {
        let n = sample();
        let text = |id: usize| link_text(&n, id);
        assert_eq!(text(0), "Restic Backup");
        // Two notes titled `plan`: the top-level one owns the bare title
        // (`Notes::find` takes a full name first), the other needs its
        // folder.
        assert_eq!(text(2), "plan");
        assert_eq!(text(1), "work/plan");
        assert_eq!(text(3), "Garden plan");
        assert_eq!(text(4), "todo.txt");
        // Two `plan`s in folders: neither owns the bare title.
        let n = notes(vec![note(1, "a", "plan", ""), note(2, "b", "plan", "")]);
        assert_eq!(link_text(&n, 0), "a/plan");
        assert_eq!(link_text(&n, 1), "b/plan");
    }

    /// An editor on `text` with the cursor at its end, and a clipboard.
    fn at_end(text: &str) -> (Editor, Clipboard) {
        let mut ed = Editor::new(text).unwrap();
        ed.key(Key::Ctrl('a'), &mut Clipboard::default());
        ed.key(Key::Right, &mut Clipboard::default());
        (ed, Clipboard::default())
    }

    /// Sends `keys` through `key_in` over `n`.
    fn send(ed: &mut Editor, clip: &mut Clipboard, n: &Notes, keys: impl IntoIterator<Item = Key>) {
        for key in keys {
            ed.key_in(key, clip, n);
        }
    }

    /// The names the open list shows, in order; empty when closed.
    fn listed(ed: &Editor, n: &Notes) -> Vec<String> {
        ed.suggestions().map_or_else(Vec::new, |list| {
            list.entries.iter().map(|&i| n.name(i).to_owned()).collect()
        })
    }

    fn chars(text: &str) -> Vec<Key> {
        text.chars().map(Key::Char).collect()
    }

    #[test]
    fn typing_brackets_opens_and_letters_filter() {
        let n = sample();
        let (mut ed, mut clip) = at_end("see ");
        send(&mut ed, &mut clip, &n, chars("[["));
        // The brackets closed themselves; the list holds every note.
        assert_eq!(ed.text(), "see [[]]");
        assert_eq!(listed(&ed, &n).len(), 6);
        send(&mut ed, &mut clip, &n, chars("pla"));
        assert_eq!(listed(&ed, &n), ["plan", "work/plan", "home/Garden plan"]);
        // Back to `p`, which `Restic Backup` holds too.
        send(&mut ed, &mut clip, &n, [Key::Backspace, Key::Backspace]);
        assert_eq!(
            listed(&ed, &n),
            ["plan", "work/plan", "home/Garden plan", "Restic Backup"]
        );
        // No match closes it; Enter is then a line break.
        send(&mut ed, &mut clip, &n, chars("zz"));
        assert!(ed.suggestions().is_none());
        send(&mut ed, &mut clip, &n, [Key::Enter]);
        assert_eq!(ed.text(), "see [[pzz\n]]");
    }

    #[test]
    fn enter_and_tab_link_the_highlighted_note() {
        let n = sample();
        for accept in [Key::Enter, Key::Tab] {
            let (mut ed, mut clip) = at_end("");
            send(&mut ed, &mut clip, &n, chars("[[pla"));
            send(&mut ed, &mut clip, &n, [Key::Down, accept]);
            // `work/plan` needs its folder: `plan` alone is the other note.
            assert_eq!(ed.text(), "[[work/plan]]");
            assert_eq!(ed.cursor(), ed.text().len());
            assert!(ed.suggestions().is_none());
            assert!(ed.is_dirty());
        }
        // An embed, and text after the link.
        let (mut ed, mut clip) = at_end("");
        send(&mut ed, &mut clip, &n, chars("![[gar"));
        send(&mut ed, &mut clip, &n, [Key::Enter]);
        send(&mut ed, &mut clip, &n, chars(" ok"));
        assert_eq!(ed.text(), "![[Garden plan]] ok");
    }

    #[test]
    fn missing_closers_are_added() {
        let n = sample();
        // A `[[` typed before a word does not close itself.
        let (mut ed, mut clip) = at_end("[[re");
        send(&mut ed, &mut clip, &n, chars("s"));
        send(&mut ed, &mut clip, &n, [Key::Enter]);
        assert_eq!(ed.text(), "[[Restic Backup]]");
        assert_eq!(ed.cursor(), ed.text().len());
        // One `]` there is not a closed link: both are added after the name.
        let (mut ed, mut clip) = at_end("[[re]");
        send(&mut ed, &mut clip, &n, [Key::Left]);
        send(&mut ed, &mut clip, &n, chars("s"));
        send(&mut ed, &mut clip, &n, [Key::Enter]);
        assert_eq!(ed.text(), "[[Restic Backup]]]");
    }

    #[test]
    fn one_undo_takes_the_link_back() {
        let n = sample();
        let (mut ed, mut clip) = at_end("");
        send(&mut ed, &mut clip, &n, chars("[[res"));
        send(&mut ed, &mut clip, &n, [Key::Enter]);
        assert_eq!(ed.text(), "[[Restic Backup]]");
        ed.key_in(Key::Ctrl('z'), &mut clip, &n);
        assert_eq!(ed.text(), "[[res]]");
        ed.key_in(Key::Ctrl('y'), &mut clip, &n);
        assert_eq!(ed.text(), "[[Restic Backup]]");
    }

    #[test]
    fn up_and_down_wrap_and_scroll() {
        let list: Vec<_> = (0..12u8)
            .map(|i| note(i, "", &format!("n{i:02}"), ""))
            .collect();
        let n = notes(list);
        let (mut ed, mut clip) = at_end("");
        send(&mut ed, &mut clip, &n, chars("[["));
        let place = |ed: &Editor| {
            let list = ed.suggestions().unwrap();
            (list.highlight, list.top)
        };
        assert_eq!(place(&ed), (0, 0));
        send(&mut ed, &mut clip, &n, [Key::Up]);
        assert_eq!(place(&ed), (11, 4));
        send(&mut ed, &mut clip, &n, [Key::Down]);
        assert_eq!(place(&ed), (0, 0));
        send(&mut ed, &mut clip, &n, (0..8).map(|_| Key::Down));
        assert_eq!(place(&ed), (8, 1));
        send(&mut ed, &mut clip, &n, [Key::Up, Key::Up]);
        assert_eq!(place(&ed), (6, 1));
        // Moving the highlight never touches the text.
        assert_eq!(ed.text(), "[[]]");
    }

    #[test]
    fn esc_and_other_keys_close_the_list() {
        let n = sample();
        let (mut ed, mut clip) = at_end("");
        send(&mut ed, &mut clip, &n, chars("[[p"));
        send(&mut ed, &mut clip, &n, [Key::Esc]);
        assert!(ed.suggestions().is_none());
        assert_eq!(ed.text(), "[[p]]");
        // Closed, Down and Tab do what they always do.
        send(&mut ed, &mut clip, &n, [Key::Tab]);
        assert_eq!(ed.text(), "[[p\t]]");
        assert!(ed.suggestions().is_none());
        // Typing opens it again.
        send(&mut ed, &mut clip, &n, [Key::Backspace]);
        assert!(ed.suggestions().is_some());
        // A key the list does not take closes it and acts as always.
        send(&mut ed, &mut clip, &n, [Key::Left]);
        assert!(ed.suggestions().is_none());
        assert_eq!(ed.cursor(), 2);
        // So does the mouse.
        send(&mut ed, &mut clip, &n, [Key::Right, Key::Char('l')]);
        assert!(ed.suggestions().is_some());
        ed.mouse(
            crate::shell::keys::MouseEvent::WheelUp,
            None,
            std::time::Instant::now(),
        );
        assert!(ed.suggestions().is_none());
    }

    /// Sends a mouse `event` through `mouse_in`.
    fn click(
        ed: &mut Editor,
        n: &Notes,
        event: MouseEvent,
        at: Option<usize>,
        entry: Option<usize>,
    ) {
        ed.mouse_in(event, at, entry, Instant::now(), n);
    }

    #[test]
    fn a_click_on_an_entry_links_it() {
        let n = sample();
        let (mut ed, mut clip) = at_end("");
        send(&mut ed, &mut clip, &n, chars("[[pla"));
        // `plan`, `work/plan`, `home/Garden plan`: the third, by a press
        // and its release.
        click(&mut ed, &n, MouseEvent::Press, Some(0), Some(2));
        click(&mut ed, &n, MouseEvent::Release, Some(0), Some(2));
        assert_eq!(ed.text(), "[[Garden plan]]");
        assert_eq!(ed.cursor(), ed.text().len());
        assert!(ed.suggestions().is_none());
        ed.key_in(Key::Ctrl('z'), &mut clip, &n);
        assert_eq!(ed.text(), "[[pla]]");
    }

    #[test]
    fn the_wheel_over_the_list_moves_the_highlight() {
        let n = sample();
        let (mut ed, mut clip) = at_end("");
        send(&mut ed, &mut clip, &n, chars("[[pla"));
        let highlight = |ed: &Editor| ed.suggestions().map(|l| l.highlight);
        click(&mut ed, &n, MouseEvent::WheelDown, None, Some(0));
        assert_eq!(highlight(&ed), Some(1));
        click(&mut ed, &n, MouseEvent::WheelUp, None, Some(0));
        click(&mut ed, &n, MouseEvent::WheelUp, None, Some(0));
        assert_eq!(highlight(&ed), Some(2));
        // A drag or a release over it changes nothing.
        click(&mut ed, &n, MouseEvent::Drag, Some(0), Some(1));
        click(&mut ed, &n, MouseEvent::Release, Some(0), Some(1));
        assert_eq!(highlight(&ed), Some(2));
        assert_eq!(ed.text(), "[[pla]]");
    }

    #[test]
    fn the_mouse_elsewhere_closes_the_list() {
        let n = sample();
        let (mut ed, mut clip) = at_end("");
        // A press on the text: the list closes, the cursor moves.
        send(&mut ed, &mut clip, &n, chars("[[pla"));
        click(&mut ed, &n, MouseEvent::Press, Some(0), None);
        assert!(ed.suggestions().is_none());
        assert_eq!(ed.cursor(), 0);
        // The wheel off the list: closes it, moves as always.
        send(
            &mut ed,
            &mut clip,
            &n,
            [Key::End, Key::Left, Key::Left, Key::Backspace],
        );
        assert!(ed.suggestions().is_some());
        click(&mut ed, &n, MouseEvent::WheelUp, None, None);
        assert!(ed.suggestions().is_none());
        // An entry past the list's end counts as off it.
        send(&mut ed, &mut clip, &n, chars("a"));
        click(&mut ed, &n, MouseEvent::Press, Some(1), Some(3));
        assert!(ed.suggestions().is_none());
        assert_eq!(ed.text(), "[[pla]]");
        assert_eq!(ed.cursor(), 1);
        // With the list closed, an entry is ignored: a plain click.
        click(&mut ed, &n, MouseEvent::Press, Some(2), Some(0));
        assert_eq!(ed.cursor(), 2);
        assert_eq!(ed.text(), "[[pla]]");
    }

    /// The open list's rows as the view shows them (dimmed part + rest).
    fn rows_of(ed: &Editor, n: &Notes) -> Vec<String> {
        ed.suggestions().map_or_else(Vec::new, |list| {
            (0..list.entries.len())
                .map(|i| {
                    let (dim, text, end) = ed.suggestion_label(i, n).unwrap();
                    format!("{dim}{text}{end}")
                })
                .collect()
        })
    }

    /// Notes with headings: `plan` (and `work/plan`), a text note, two
    /// `dup`s in folders.
    fn with_headings() -> Notes {
        notes(vec![
            note(
                1,
                "",
                "plan",
                "# Checks\n\n## Release checks\n\n## Sign `key`\n\n# Bad ]] one\n\n# a|b\n\n## Checklist\n",
            ),
            note(2, "work", "other", "# Other heading\n"),
            note(3, "", "todo.txt", "# not a heading\n"),
            note(4, "a", "dup", "# A\n"),
            note(5, "b", "dup", "# B\n"),
        ])
    }

    #[test]
    fn hash_lists_the_notes_headings() {
        let n = with_headings();
        let (mut ed, mut clip) = at_end("see ");
        send(&mut ed, &mut clip, &n, chars("[[plan#"));
        // In the note's order; the ones with `]` or `|` left out; inline
        // code as the index reads it.
        assert_eq!(
            rows_of(&ed, &n),
            ["# Checks", "# Release checks", "# Sign key", "# Checklist"]
        );
        assert!(matches!(
            ed.suggestions().unwrap().source,
            Source::Headings(Some(0))
        ));
        // Filtered by words; starting with the text first.
        send(&mut ed, &mut clip, &n, chars("check"));
        assert_eq!(
            rows_of(&ed, &n),
            ["# Checks", "# Checklist", "# Release checks"]
        );
        send(&mut ed, &mut clip, &n, [Key::Down, Key::Down, Key::Enter]);
        assert_eq!(ed.text(), "see [[plan#Release checks]]");
        assert_eq!(ed.cursor(), ed.text().len());
        assert!(n.resolve(1, "plan#Release checks").is_ok());
    }

    #[test]
    fn no_headings_without_one_markdown_note() {
        let n = with_headings();
        // Missing, ambiguous, a text note, no match.
        for typed in ["[[nope#", "[[dup#", "[[todo.txt#", "[[todo#", "[[plan#zzz"] {
            let (mut ed, mut clip) = at_end("");
            send(&mut ed, &mut clip, &n, chars(typed));
            assert!(ed.suggestions().is_none(), "{typed}");
        }
        // A folder disambiguates.
        let (mut ed, mut clip) = at_end("");
        send(&mut ed, &mut clip, &n, chars("[[a/dup#"));
        assert_eq!(rows_of(&ed, &n), ["# A"]);
        send(&mut ed, &mut clip, &n, [Key::Tab]);
        assert_eq!(ed.text(), "[[a/dup#A]]");
    }

    #[test]
    fn hash_alone_lists_the_edited_texts_headings() {
        let n = with_headings();
        // Unsaved text, headings typed just now.
        let (mut ed, mut clip) = at_end("# Intro\n\n## Next step\n\n");
        send(&mut ed, &mut clip, &n, chars("[[#"));
        assert_eq!(rows_of(&ed, &n), ["# Intro", "# Next step"]);
        assert!(matches!(
            ed.suggestions().unwrap().source,
            Source::Headings(None)
        ));
        send(&mut ed, &mut clip, &n, chars("ne"));
        send(&mut ed, &mut clip, &n, [Key::Enter]);
        assert!(ed.text().ends_with("[[#Next step]]"));
        // Its own name means its own text too, not the saved one.
        let (mut ed, mut clip) = at_end("# Fresh\n\n");
        ed.set_note(Some(0), true);
        send(&mut ed, &mut clip, &n, chars("[[plan#"));
        assert_eq!(rows_of(&ed, &n), ["# Fresh"]);
        // A plain-text note has none.
        let (mut ed, mut clip) = at_end("# Not one\n\n");
        ed.set_note(None, false);
        send(&mut ed, &mut clip, &n, chars("[[#"));
        assert!(ed.suggestions().is_none());
        // Closing the list drops the scanned headings.
        let (mut ed, mut clip) = at_end("# Intro\n\n");
        send(&mut ed, &mut clip, &n, chars("[[#"));
        send(&mut ed, &mut clip, &n, [Key::Esc]);
        assert!(ed.own_headings.is_empty());
    }

    #[test]
    fn own_headings_are_scanned_once_per_link() {
        let n = with_headings();
        let (mut ed, mut clip) = at_end("# Intro\n\n");
        send(&mut ed, &mut clip, &n, chars("[[#"));
        let at = ed.own_headings_at;
        assert!(at.is_some());
        // Typing and Backspace in the link keep them.
        send(&mut ed, &mut clip, &n, chars("In"));
        send(&mut ed, &mut clip, &n, [Key::Backspace]);
        assert_eq!(ed.own_headings_at, at);
        assert_eq!(rows_of(&ed, &n), ["# Intro"]);
        // Any other key drops them; a list after it scans again.
        send(&mut ed, &mut clip, &n, [Key::Delete]);
        assert!(ed.own_headings_at.is_some(), "Delete scans again");
        send(&mut ed, &mut clip, &n, [Key::Left]);
        assert!(ed.own_headings.is_empty());
        assert_eq!(ed.own_headings_at, None);
        // A note list never keeps them.
        send(&mut ed, &mut clip, &n, [Key::End]);
        send(&mut ed, &mut clip, &n, chars(" [[p"));
        assert!(ed.own_headings.is_empty());
    }

    /// Notes with blocks: `plan` (ids on some), a text note.
    fn with_blocks() -> Notes {
        notes(vec![
            note(
                1,
                "",
                "plan",
                "# Title\n\nFirst paragraph ^p1\n\nTwo lines\nend here ^p2\n\nNo id here\n\n- item ^i-1\n- other item\n",
            ),
            note(2, "", "todo.txt", "line ^t\n"),
        ])
    }

    #[test]
    fn caret_lists_the_notes_blocks() {
        let n = with_blocks();
        let (mut ed, mut clip) = at_end("see ");
        send(&mut ed, &mut clip, &n, chars("[[plan#^"));
        assert_eq!(
            rows_of(&ed, &n),
            [
                "First paragraph ^p1",
                "Two lines ^p2",
                "No id here",
                "item ^i-1",
                "other item"
            ]
        );
        assert!(matches!(
            ed.suggestions().unwrap().source,
            Source::Blocks(Some(0))
        ));
        // The id is the dimmed end, apart from the text.
        assert_eq!(
            ed.suggestion_label(1, &n).map(|l| (l.1, l.2)),
            Some(("Two lines", " ^p2"))
        );
        // Filtered by the words of the whole block.
        send(&mut ed, &mut clip, &n, chars("end"));
        assert_eq!(rows_of(&ed, &n), ["Two lines ^p2"]);
        send(&mut ed, &mut clip, &n, [Key::Enter]);
        assert_eq!(ed.text(), "see [[plan#^p2]]");
        assert!(n.resolve(1, "plan#^p2").is_ok());
    }

    #[test]
    fn a_block_without_id_is_not_linked_yet() {
        let n = with_blocks();
        let (mut ed, mut clip) = at_end("");
        send(&mut ed, &mut clip, &n, chars("[[plan#^no"));
        assert_eq!(rows_of(&ed, &n), ["No id here"]);
        send(&mut ed, &mut clip, &n, [Key::Enter]);
        assert_eq!(ed.text(), "[[plan#^no]]");
        assert!(ed.suggestions().is_none());
        assert_eq!(ed.notice(), Some(NO_ID));
        // No blocks in a text note, or for a missing note.
        for typed in ["[[todo.txt#^", "[[nope#^"] {
            let (mut ed, mut clip) = at_end("");
            send(&mut ed, &mut clip, &n, chars(typed));
            assert!(ed.suggestions().is_none(), "{typed}");
        }
    }

    #[test]
    fn caret_alone_lists_the_edited_texts_blocks_while_typing() {
        let n = with_blocks();
        let (mut ed, mut clip) = at_end("Kept one ^k\n\nAfter\n\nsee ");
        send(&mut ed, &mut clip, &n, chars("[[#^"));
        // Its own blocks, the one being typed in too.
        assert_eq!(rows_of(&ed, &n), ["Kept one ^k", "After", "see [[#^]]"]);
        let link = ed.blocks_for.map(|f| f.link);
        // Typed and deleted in the link: the same parse, its ranges moved.
        // All three hold an `e`; none starts with one: the note's order.
        send(&mut ed, &mut clip, &n, chars("e"));
        assert_eq!(rows_of(&ed, &n), ["Kept one ^k", "After", "see [[#^e]]"]);
        send(&mut ed, &mut clip, &n, chars("x"));
        assert_eq!(rows_of(&ed, &n), ["see [[#^ex]]"]);
        send(&mut ed, &mut clip, &n, [Key::Backspace, Key::Backspace]);
        assert_eq!(rows_of(&ed, &n), ["Kept one ^k", "After", "see [[#^]]"]);
        assert_eq!(ed.blocks_for.map(|f| f.link), link);
        // Without the `^` it is a heading list: none here, so closed.
        send(&mut ed, &mut clip, &n, [Key::Backspace]);
        assert!(ed.suggestions().is_none());
        assert!(ed.blocks.is_empty());
    }

    #[test]
    fn a_click_links_a_heading() {
        let n = with_headings();
        let (mut ed, mut clip) = at_end("");
        send(&mut ed, &mut clip, &n, chars("[[plan#"));
        click(&mut ed, &n, MouseEvent::Press, Some(0), Some(3));
        assert_eq!(ed.text(), "[[plan#Checklist]]");
        assert!(ed.suggestions().is_none());
    }

    #[test]
    fn list_keys_disarm_quit_and_clear_the_notice() {
        let n = sample();
        let (mut ed, mut clip) = at_end("");
        send(&mut ed, &mut clip, &n, chars("[[p"));
        // Ctrl+Q with unsaved changes arms and closes the list...
        send(&mut ed, &mut clip, &n, [Key::Ctrl('q')]);
        assert!(ed.notice().is_some());
        send(
            &mut ed,
            &mut clip,
            &n,
            [Key::Backspace, Key::Char('p'), Key::Down],
        );
        // ...and a list key disarmed it: Ctrl+Q only warns again.
        assert!(ed.notice().is_none());
        assert_eq!(ed.key_in(Key::Ctrl('q'), &mut clip, &n), EditorAction::Stay);
    }

    proptest! {
        /// Any text and cursor: no panic, and a query is a char-boundary
        /// range that ends at the cursor and holds no character that
        /// ends a name.
        #[test]
        fn query_is_a_slice_ending_at_the_cursor(
            text in "[ab \n\\[\\]|#^!é😀]{0,24}",
            cursor in 0usize..40,
        ) {
            if let Some(r) = query(&text, cursor) {
                prop_assert_eq!(r.end, cursor);
                prop_assert!(r.start <= r.end);
                let name = text.get(r.clone());
                prop_assert!(name.is_some());
                prop_assert!(!name.unwrap().contains(ENDS_NAME));
                prop_assert!(!name.unwrap().contains('\n'));
                prop_assert_eq!(&text[r.start - 2..r.start], "[[");
            }
        }

        /// Any text and cursor: no panic, and a target's name and
        /// fragment are char-boundary ranges after `[[`, in order, the
        /// last one ending at the cursor; neither holds a character that
        /// ends it, and only the fragment's own `#` (and `^`) lie between.
        #[test]
        fn target_ranges_are_valid(
            text in "[ab \n\\[\\]|#^!é😀]{0,24}",
            cursor in 0usize..40,
        ) {
            if let Some(Query { name, fragment }) = target(&text, cursor) {
                prop_assert_eq!(&text[name.start - 2..name.start], "[[");
                prop_assert!(!text[name.clone()].contains(ENDS_NAME));
                prop_assert!(!text[name.clone()].contains('\n'));
                match fragment {
                    None => prop_assert_eq!(name.end, cursor),
                    Some((part, r)) => {
                        prop_assert_eq!(r.end, cursor);
                        let between = &text[name.end..r.start];
                        let expected = match part { Part::Heading => "#", Part::Block => "#^" };
                        prop_assert_eq!(between, expected);
                        prop_assert!(!text[r.clone()].contains(ENDS_NAME));
                        prop_assert!(!text[r].contains('\n'));
                    }
                }
            }
        }

        /// Any headings: every heading the list offers, chosen with Enter,
        /// writes a link that resolves to that note, in another note
        /// (`[[t#`) and in the note being edited (`[[#`).
        #[test]
        fn heading_links_resolve(
            headings in prop::collection::vec("[a-cé \\[\\]|#^`*]{1,8}", 1..6),
            levels in prop::collection::vec(1usize..4, 6),
            pick in any::<prop::sample::Index>(),
            own in any::<bool>(),
        ) {
            let body: String = headings
                .iter()
                .zip(&levels)
                .map(|(h, &level)| format!("{} {h}\n\n", "#".repeat(level)))
                .collect();
            let (start, typed) = if own { (body.as_str(), "[[#") } else { ("", "[[t#") };
            let n = notes(vec![note(1, "", "t", &body)]);
            let (mut ed, mut clip) = at_end(start);
            ed.set_note(own.then_some(0), true);
            send(&mut ed, &mut clip, &n, chars(typed));
            let Some(list) = ed.suggestions() else {
                // Every heading held a link character, or none parsed.
                return Ok(());
            };
            let downs = pick.index(list.entries.len());
            send(&mut ed, &mut clip, &n, (0..downs).map(|_| Key::Down));
            send(&mut ed, &mut clip, &n, [Key::Enter]);
            let text = ed.text().to_owned();
            prop_assert!(text.ends_with("]]"), "{:?}", text);
            let link = &text[text.rfind("[[").unwrap() + 2..text.len() - 2];
            // Resolved as the saved vault would: the note holding the
            // final text when it is the edited one.
            let resolved = if own {
                notes(vec![note(1, "", "t", &text)]).resolve(0, link).map(|(to, f)| (to, f.is_some()))
            } else {
                n.resolve(0, link).map(|(to, f)| (to, f.is_some()))
            };
            prop_assert_eq!(resolved, Ok((0, true)), "{:?}", link);
        }

        /// Any blocks: a block chosen with Enter writes a link that
        /// resolves to that note when it has an id, and changes nothing
        /// (a notice) when it has none; in another note and in the edited
        /// one.
        #[test]
        fn block_links_resolve(
            paragraphs in prop::collection::vec("(- )?[a-cé `*]{1,8}( \\^[a-z0-9]{1,4})?", 1..6),
            pick in any::<prop::sample::Index>(),
            own in any::<bool>(),
        ) {
            let body = paragraphs.join("\n\n");
            let own_start = format!("{body}\n\n");
            let (start, typed) = if own { (own_start.as_str(), "[[#^") } else { ("", "[[t#^") };
            let n = notes(vec![note(1, "", "t", &body)]);
            let (mut ed, mut clip) = at_end(start);
            ed.set_note(own.then_some(0), true);
            send(&mut ed, &mut clip, &n, chars(typed));
            let Some(list) = ed.suggestions() else {
                return Ok(());
            };
            let downs = pick.index(list.entries.len());
            let has_id = !ed.suggestion_label(downs, &n).unwrap().2.is_empty();
            let before = ed.text().to_owned();
            send(&mut ed, &mut clip, &n, (0..downs).map(|_| Key::Down));
            send(&mut ed, &mut clip, &n, [Key::Enter]);
            let text = ed.text().to_owned();
            if !has_id {
                prop_assert_eq!(&text, &before);
                prop_assert_eq!(ed.notice(), Some(NO_ID));
                return Ok(());
            }
            let link = &text[text.rfind("[[").unwrap() + 2..text.len() - 2];
            let resolved = if own {
                notes(vec![note(1, "", "t", &text)]).resolve(0, link).map(|(to, f)| (to, f.is_some()))
            } else {
                n.resolve(0, link).map(|(to, f)| (to, f.is_some()))
            };
            prop_assert_eq!(resolved, Ok((0, true)), "{:?}", link);
        }

        /// Whatever the titles and folders, the text `link_text` gives
        /// finds its own note.
        #[test]
        fn link_text_finds_its_note(
            list in prop::collection::vec(
                (prop::sample::select(vec!["", "a", "b", "a/c"]),
                 prop::sample::select(vec!["x", "X", "y", "x.md", "z.txt"])),
                1..8,
            ),
        ) {
            // Names are unique ignoring case (decision 0034): keep the
            // first of each.
            let mut seen: Vec<String> = Vec::new();
            let mut list_notes = Vec::new();
            for (i, (folder, title)) in list.into_iter().enumerate() {
                let key = format!("{folder}/{title}").to_lowercase();
                if !seen.contains(&key) {
                    seen.push(key);
                    list_notes.push(note(u8::try_from(i).unwrap(), folder, title, ""));
                }
            }
            let n = notes(list_notes);
            for i in 0..n.len() {
                prop_assert_eq!(n.find(link_text(&n, i)), Found::One(i));
            }
        }
    }
}
