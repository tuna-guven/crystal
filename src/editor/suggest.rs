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

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "wired into the session in roadmap step W5")
)]

use std::ops::Range;
use std::time::Instant;

use super::{Editor, EditorAction};
use crate::notes::{Found, Notes, cmp_ci};
use crate::shell::keys::{Key, MouseEvent};
use crate::ui::clipboard::Clipboard;

/// Rows the list shows at once; the rest scroll.
pub const ROWS: usize = 8;

/// The open list. No Debug, like the editor that holds it.
pub struct List {
    /// The matching notes, as indexes into `Notes`, best first; never
    /// empty.
    pub notes: Vec<usize>,
    /// The highlighted row, an index into `notes`.
    pub highlight: usize,
    /// The first row shown; the highlight is always within `ROWS` of it.
    pub top: usize,
}

impl List {
    /// Moves the highlight one row, wrapping at either end, and scrolls
    /// just enough to keep it shown.
    fn step(&mut self, down: bool) {
        let len = self.notes.len();
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

impl Editor {
    /// `key` with link suggestions over `notes` (see the module comment).
    /// A key the list takes ends a typing run, clears the notice and
    /// disarms Ctrl+Q, like any key that is not typing.
    pub fn key_in(&mut self, key: Key, clipboard: &mut Clipboard, notes: &Notes) -> EditorAction {
        let list_key = matches!(key, Key::Up | Key::Down | Key::Enter | Key::Tab | Key::Esc);
        if list_key && let Some(list) = &mut self.suggestions {
            match key {
                Key::Up | Key::Down => list.step(key == Key::Down),
                Key::Enter | Key::Tab => self.accept(notes),
                _ => self.suggestions = None,
            }
            self.notice = None;
            self.quit_armed = false;
            self.last_press = None;
            self.buffer.commit();
            return EditorAction::Stay;
        }
        let typing = matches!(key, Key::Char(_) | Key::Backspace | Key::Delete);
        let action = self.key(key, clipboard);
        self.suggestions = None;
        if typing {
            self.refresh(notes);
        }
        action
    }

    /// `mouse` with link suggestions: `entry` is the list entry under the
    /// pointer (`View::suggestion_at`), if any. A press on an entry links
    /// that note, the wheel there moves the highlight; like a key, they
    /// end a typing run, clear the notice and disarm Ctrl+Q. Any other
    /// event goes to `mouse`, which closes the list.
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
            .filter(|(i, list)| *i < list.notes.len());
        let Some((entry, list)) = on_list else {
            self.mouse(event, at, now);
            return;
        };
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
        self.notice = None;
        self.quit_armed = false;
        self.last_press = None;
        self.pressed = false;
        self.buffer.commit();
    }

    /// The open list, for the view.
    pub fn suggestions(&self) -> Option<&List> {
        self.suggestions.as_ref()
    }

    /// Opens the list for the cursor's place: only with nothing selected,
    /// in a link's name, and with at least one note matching.
    fn refresh(&mut self, notes: &Notes) {
        if self.selection().is_some() {
            return;
        }
        let Some(range) = query(self.text(), self.cursor) else {
            return;
        };
        let found = matches(&self.text()[range], notes);
        if !found.is_empty() {
            self.suggestions = Some(List {
                notes: found,
                highlight: 0,
                top: 0,
            });
        }
    }

    /// Enter or Tab: the query becomes the highlighted note's link text
    /// and the cursor goes past the `]]` after it, added if missing. One
    /// undo step (the caller commits before; this commits after).
    fn accept(&mut self, notes: &Notes) {
        let list = self.suggestions.take();
        let (Some(list), Some(range)) = (list, query(self.text(), self.cursor)) else {
            return;
        };
        // The list was made from these notes; the check keeps a stale
        // index from panicking.
        let Some(&note) = list.notes.get(list.highlight).filter(|&&i| i < notes.len()) else {
            return;
        };
        self.buffer.commit();
        self.anchor = Some(range.start);
        if self.replace_selection(link_text(notes, note)) {
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

/// Characters that end the name part of a link: `]` closes it, `|`
/// starts its text, `#` a heading, `^` a block. No name holds any of
/// them, nor `[` (decision 0034).
const ENDS_NAME: &[char] = &['[', ']', '|', '#', '^'];

/// The query: the bytes from just after the last `[[` before `cursor`
/// on its line, up to `cursor`, when none of `ENDS_NAME` lies between.
/// `![[` (an embed) counts too, since it ends in `[[`. None when the
/// cursor is not inside a link's name, or not on a char boundary.
pub fn query(text: &str, cursor: usize) -> Option<Range<usize>> {
    let before = text.get(..cursor)?;
    let line = before.rfind('\n').map_or(0, |at| at + 1);
    let start = line + before[line..].rfind("[[")? + 2;
    (!before[start..].contains(ENDS_NAME)).then_some(start..cursor)
}

/// The notes whose full name holds every word of `query`, ignoring case,
/// as indexes: those whose title starts with the whole query first, then
/// by name (`cmp_ci`, as `ls` sorts). A blank query matches every note.
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
    found.sort_by(|&a, &b| {
        let late = |i: usize| !starts_ci(notes.note(i).title.as_str(), whole);
        late(a)
            .cmp(&late(b))
            .then_with(|| cmp_ci(notes.name(a), notes.name(b)))
    });
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
            list.notes.iter().map(|&i| n.name(i).to_owned()).collect()
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
