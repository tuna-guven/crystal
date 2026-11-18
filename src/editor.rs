//! The note editor (decision 0045): modeless, with
//! the keys most editors share.
//!
//! The cursor is a caret between graphemes (it may sit at the end of the
//! text). A selection runs from an anchor to the cursor; Shift with a
//! movement key starts or extends it, a movement key alone drops it, and
//! typing replaces it (an opening bracket wraps it instead). Only Ctrl+C
//! and Ctrl+X copy, into Crystal's own clipboard; selecting copies
//! nothing.
//!
//! The brackets `()`, `[]` and `{}` close as you type: an opening one
//! brings its closer, a typed closer steps over the same one, and
//! Backspace in an empty pair deletes both.
//!
//! A run of typing (text, Enter, Tab, Backspace, Delete) is one undo step;
//! any other key ends it. A paste is a step of its own.
//!
//! Like the pager, it is terminal-free: keys in, an action out. The view
//! draws it; the session saves, closes, or shows the viewer.

pub mod buffer;
pub mod view;

use std::ops::Range;
use std::time::{Duration, Instant};

use unicode_segmentation::UnicodeSegmentation;

use crate::shell::keys::{Key, MouseEvent};
use crate::ui::clipboard::Clipboard;
use crate::vault::codec::MAX_BODY_BYTES;
use buffer::{Buffer, EditError};

/// What the session does after a key.
#[derive(Debug, PartialEq, Eq)]
pub enum EditorAction {
    Stay,
    /// Ctrl+S.
    Save,
    /// Ctrl+Q: close, discarding unsaved changes (the editor has warned).
    Close,
    /// Ctrl+E: show the current text in the viewer.
    View,
}

/// Lines one step of the mouse wheel moves the cursor (decision 0073).
const WHEEL_LINES: usize = 3;
/// Presses at the same place closer together than this make a double or
/// triple click: GTK's default (decision 0073).
const MULTI_CLICK: Duration = Duration::from_millis(400);

/// Rows a page moves until the view sets the real height.
const DEFAULT_PAGE: usize = 20;

const FULL: &str = "The note is at its size limit (8 MiB).";
const PASTE_TOO_LARGE: &str = "Not pasted: it is larger than a note can hold (8 MiB).";
/// After a paste the cleaning changed (decision 0047).
pub const PASTE_CLEANED: &str = "Untrusted input sanitized and pasted safely.";
const UNSAVED: &str = "Unsaved changes: Ctrl+S saves, Ctrl+Q again discards them.";

/// The brackets the editor closes as you type (decision 0045). Each is
/// two ASCII bytes, so the cursor fits between them. Quotes are left out:
/// `don't` must not open a pair.
const PAIRS: [&str; 3] = ["()", "[]", "{}"];

/// The pair an opening bracket starts.
fn pair_of(c: char) -> Option<&'static str> {
    PAIRS.iter().copied().find(|pair| pair.starts_with(c))
}

/// One note being edited. No Debug: it holds note text.
pub struct Editor {
    buffer: Buffer,
    /// A byte offset on a grapheme boundary.
    cursor: usize,
    /// The other end of the selection, if there is one.
    anchor: Option<usize>,
    /// The column Up and Down aim for, kept across short lines.
    goal: Option<usize>,
    dirty: bool,
    /// Ctrl+Q was pressed once with unsaved changes.
    quit_armed: bool,
    notice: Option<&'static str>,
    page: usize,
    /// The left button went down on the text and is not released yet:
    /// only then does a drag select.
    pressed: bool,
    /// The last press on the text: when, where, and its count (1 to 3),
    /// for double and triple clicks. Any key forgets it.
    last_press: Option<(Instant, usize, u8)>,
}

impl Editor {
    pub fn new(body: &str) -> Result<Self, EditError> {
        Ok(Self {
            buffer: Buffer::new(body)?,
            cursor: 0,
            anchor: None,
            goal: None,
            dirty: false,
            quit_armed: false,
            notice: None,
            page: DEFAULT_PAGE,
            pressed: false,
            last_press: None,
        })
    }

    pub fn text(&self) -> &str {
        self.buffer.as_str()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The selected bytes, if anything is selected.
    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        let range = anchor.min(self.cursor)..anchor.max(self.cursor);
        (!range.is_empty()).then_some(range)
    }

    pub fn notice(&self) -> Option<&'static str> {
        self.notice
    }

    /// Whether there are changes since opening or the last save.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The session saved the text.
    pub fn saved(&mut self) {
        self.dirty = false;
    }

    /// The mouse (decision 0073), its cell already turned into a byte
    /// offset by the view (`View::position_at`); `at` is `None` for a press
    /// outside the text; `now` is when it arrived. A press puts the cursor
    /// there and drops the selection; a drag after it selects from there;
    /// a second press at the same place within `MULTI_CLICK` selects the
    /// word there, a third the whole line with its line break, a fourth
    /// starts over; the wheel moves the cursor `WHEEL_LINES` lines. Like a
    /// key, it ends a run of typing for undo, clears the notice and
    /// disarms Ctrl+Q.
    pub fn mouse(&mut self, event: MouseEvent, at: Option<usize>, now: Instant) {
        self.notice = None;
        self.quit_armed = false;
        self.buffer.commit();
        // The view gives char boundaries; anything else is ignored.
        let at = at
            .map(|at| at.min(self.text().len()))
            .filter(|&at| self.text().is_char_boundary(at))
            .map(|at| self.snap(at));
        match (event, at) {
            (MouseEvent::Press, Some(at)) => {
                let count = match self.last_press {
                    Some((then, place, count))
                        if place == at
                            && count < 3
                            && now.saturating_duration_since(then) < MULTI_CLICK =>
                    {
                        count + 1
                    }
                    _ => 1,
                };
                self.last_press = Some((now, at, count));
                // Only a single click starts a drag: one that moved while
                // double-clicking would undo the word.
                self.pressed = count == 1;
                match count {
                    1 => self.move_to(at, false),
                    2 => self.select(self.word_at(at)),
                    _ => self.select(self.line_at(at)),
                }
            }
            (MouseEvent::Drag, Some(at)) if self.pressed => self.move_to(at, true),
            (MouseEvent::WheelUp, _) => self.vertical(false, WHEEL_LINES, false),
            (MouseEvent::WheelDown, _) => self.vertical(true, WHEEL_LINES, false),
            (MouseEvent::Press | MouseEvent::Release, _) => self.pressed = false,
            (MouseEvent::Drag, _) => {}
        }
    }

    /// Shows `notice` until the next key (the session's save results).
    pub fn tell(&mut self, notice: &'static str) {
        self.notice = Some(notice);
    }

    /// Rows the view shows, for PgUp and PgDn.
    pub fn set_page(&mut self, rows: usize) {
        self.page = rows.max(1);
    }

    pub fn key(&mut self, key: Key, clipboard: &mut Clipboard) -> EditorAction {
        // Mouse reports go to `mouse`, never here (decision 0073).
        if matches!(key, Key::Mouse { .. }) {
            return EditorAction::Stay;
        }
        // A key between two presses: no double click.
        self.last_press = None;
        self.notice = None;
        let quit_armed = std::mem::take(&mut self.quit_armed);
        let typing = matches!(
            key,
            Key::Char(_) | Key::Enter | Key::Tab | Key::Backspace | Key::Delete
        );
        if !typing {
            self.buffer.commit();
        }
        match key {
            // A closer typed before the same closer steps over it.
            Key::Char(c) if self.selection().is_none() && self.closer_follows(c) => {
                let next = self.buffer.next_grapheme(self.cursor);
                self.move_to(next, false);
            }
            Key::Char(c) => match (pair_of(c), self.selection()) {
                // An opening bracket wraps the selection...
                (Some(pair), Some(range)) => self.wrap(pair, range),
                // ...or brings its closer, the cursor between.
                (Some(pair), None) if self.closes_here() => {
                    if self.replace_selection(pair) {
                        self.cursor -= 1;
                    }
                }
                _ => {
                    let mut bytes = [0; 4];
                    self.replace_selection(c.encode_utf8(&mut bytes));
                }
            },
            Key::Enter => {
                self.replace_selection("\n");
            }
            Key::Tab => {
                self.replace_selection("\t");
            }
            Key::Backspace => self.delete(false),
            Key::Delete => self.delete(true),
            Key::Paste { text, cut, cleaned } => {
                // Longer than a note can hold: the decoder kept only its
                // start, which would look like the whole. Nothing is pasted.
                if cut {
                    self.notice = Some(PASTE_TOO_LARGE);
                // An empty paste (or one of dropped control characters
                // only) must not delete the selection.
                } else if !text.is_empty() {
                    self.replace_selection(&text);
                    self.buffer.commit();
                    // Said only when the text went in (not when the note
                    // was full) and differs from what was copied.
                    if cleaned && self.notice.is_none() {
                        self.notice = Some(PASTE_CLEANED);
                    }
                }
            }
            Key::Left | Key::ShiftLeft => {
                let at = self.buffer.prev_grapheme(self.cursor);
                self.move_to(at, key == Key::ShiftLeft);
            }
            Key::Right | Key::ShiftRight => {
                let at = self.buffer.next_grapheme(self.cursor);
                self.move_to(at, key == Key::ShiftRight);
            }
            Key::Up | Key::ShiftUp => self.vertical(false, 1, key == Key::ShiftUp),
            Key::Down | Key::ShiftDown => self.vertical(true, 1, key == Key::ShiftDown),
            Key::PageUp => self.vertical(false, self.page, false),
            Key::PageDown => self.vertical(true, self.page, false),
            Key::Home | Key::ShiftHome => {
                self.move_to(self.line_start(self.cursor), key == Key::ShiftHome);
            }
            Key::End | Key::ShiftEnd => {
                self.move_to(self.line_end(self.cursor), key == Key::ShiftEnd);
            }
            Key::Ctrl('a') => {
                self.anchor = Some(0);
                self.cursor = self.text().len();
                self.goal = None;
            }
            Key::Ctrl('c') => {
                self.copy(clipboard);
            }
            Key::Ctrl('x') => {
                if self.copy(clipboard) {
                    self.replace_selection("");
                    self.buffer.commit();
                }
            }
            Key::Ctrl('v') => match clipboard.get().filter(|t| !t.is_empty()) {
                Some(text) => {
                    self.replace_selection(text);
                    self.buffer.commit();
                }
                None => self.notice = Some("Crystal's clipboard is empty."),
            },
            Key::Ctrl('z') => self.undo_redo(true),
            Key::Ctrl('y') => self.undo_redo(false),
            Key::Ctrl('s') => return EditorAction::Save,
            Key::Ctrl('e') => return EditorAction::View,
            Key::Ctrl('q') if self.dirty && !quit_armed => {
                self.quit_armed = true;
                self.notice = Some(UNSAVED);
            }
            Key::Ctrl('q') => return EditorAction::Close,
            _ => {}
        }
        EditorAction::Stay
    }

    /// Replaces the selection, or inserts at the cursor, and leaves the
    /// cursor after the new text; true if the text went in. A refusal
    /// leaves everything as it was.
    fn replace_selection(&mut self, text: &str) -> bool {
        let range = self.selection().unwrap_or(self.cursor..self.cursor);
        if range.is_empty() && text.is_empty() {
            return false;
        }
        match self.buffer.replace(range.clone(), text) {
            Ok(()) => {
                self.dirty = true;
                self.collapse(range.start + text.len());
                true
            }
            Err(EditError::Full) => {
                self.notice = Some(FULL);
                false
            }
            Err(EditError::NotABoundary) => false,
        }
    }

    /// Puts the pair around the selection, which stays selected inside
    /// it. The closer goes in first, so `range.start` still holds for the
    /// opener; the room is checked first, so both go in or neither.
    fn wrap(&mut self, pair: &str, range: Range<usize>) {
        if self.text().len() + pair.len() > MAX_BODY_BYTES {
            self.notice = Some(FULL);
            return;
        }
        let (open, close) = pair.split_at(1);
        let wrapped = self
            .buffer
            .replace(range.end..range.end, close)
            .and_then(|()| self.buffer.replace(range.start..range.start, open));
        if wrapped.is_ok() {
            self.dirty = true;
            // A bracket can join an accent beside it into one grapheme.
            self.anchor = self.anchor.map(|at| self.snap(at + 1));
            self.cursor = self.snap(self.cursor + 1);
            self.goal = None;
        }
    }

    /// Whether an opening bracket typed here gets its closer: only before
    /// whitespace, a closing bracket or the end, so `(word` stays as typed.
    fn closes_here(&self) -> bool {
        self.text()[self.cursor..]
            .chars()
            .next()
            .is_none_or(|next| next.is_whitespace() || PAIRS.iter().any(|p| p.ends_with(next)))
    }

    /// Whether `c` is a closing bracket and the grapheme after the cursor
    /// is that bracket alone (not `)` with a combining accent on it).
    fn closer_follows(&self, c: char) -> bool {
        let next = self.buffer.next_grapheme(self.cursor);
        PAIRS.iter().any(|p| p.ends_with(c)) && self.text()[self.cursor..next].chars().eq([c])
    }

    /// Backspace and Delete: the selection, or one grapheme before or
    /// after the cursor.
    /// Backspace in an empty pair, `(|)`, takes the closer too.
    fn delete(&mut self, forward: bool) {
        if self.selection().is_none() {
            let other = if forward {
                self.buffer.next_grapheme(self.cursor)
            } else {
                self.buffer.prev_grapheme(self.cursor)
            };
            if !forward && self.in_empty_pair() {
                self.cursor = self.buffer.next_grapheme(self.cursor);
            }
            self.anchor = Some(other);
        }
        self.replace_selection("");
    }

    /// Whether the graphemes around the cursor are exactly one of the
    /// pairs: two bytes in all, so neither bracket carries an accent.
    fn in_empty_pair(&self) -> bool {
        let start = self.buffer.prev_grapheme(self.cursor);
        let end = self.buffer.next_grapheme(self.cursor);
        PAIRS.contains(&&self.text()[start..end])
    }

    /// Copies the selection into Crystal's clipboard; false if nothing is
    /// selected.
    fn copy(&mut self, clipboard: &mut Clipboard) -> bool {
        let Some(range) = self.selection() else {
            self.notice = Some("Nothing is selected.");
            return false;
        };
        clipboard.set(&self.text()[range]);
        self.notice = Some("Copied to Crystal's clipboard.");
        true
    }

    fn undo_redo(&mut self, undo: bool) {
        let at = if undo {
            self.buffer.undo()
        } else {
            self.buffer.redo()
        };
        match at {
            Some(at) => {
                self.dirty = true;
                self.collapse(at.min(self.text().len()));
            }
            None if undo => self.notice = Some("Nothing to undo."),
            None => self.notice = Some("Nothing to redo."),
        }
    }

    /// Puts the cursor at `at` with nothing selected. After an edit, `at`
    /// can be inside a grapheme (a letter typed before a combining accent
    /// joins it); the cursor then goes to that grapheme's end.
    fn collapse(&mut self, at: usize) {
        self.cursor = self.snap(at);
        self.anchor = None;
        self.goal = None;
    }

    /// `at` if it is on a grapheme boundary, else the end of the grapheme
    /// it is inside.
    fn snap(&self, at: usize) -> usize {
        if at == 0 {
            0
        } else {
            let start = self.buffer.prev_grapheme(at);
            self.buffer.next_grapheme(start).max(at)
        }
    }

    /// Moves the cursor; with Shift the selection grows from where it
    /// started, without it the selection is dropped.
    fn move_to(&mut self, at: usize, select: bool) {
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = at;
        self.goal = None;
    }

    /// Selects `range`, the cursor at its end; an empty one only moves
    /// the cursor.
    fn select(&mut self, range: Range<usize>) {
        let (start, end) = (self.snap(range.start), self.snap(range.end));
        self.move_to(start, false);
        if end > start {
            self.move_to(end, true);
        }
    }

    /// The word around `at` in its line, by Unicode word boundaries (a
    /// run of spaces or one punctuation mark counts as a word too); empty
    /// at the line's end.
    fn word_at(&self, at: usize) -> Range<usize> {
        let (start, end) = (self.line_start(at), self.line_end(at));
        self.text()
            .get(start..end)
            .unwrap_or("")
            .split_word_bound_indices()
            .map(|(i, word)| start + i..start + i + word.len())
            .find(|word| word.contains(&at))
            .unwrap_or(at..at)
    }

    /// The line around `at`, with its line break if it has one.
    fn line_at(&self, at: usize) -> Range<usize> {
        let end = self.line_end(at);
        let end = if end < self.text().len() {
            self.buffer.next_grapheme(end)
        } else {
            end
        };
        self.line_start(at)..end
    }

    fn line_start(&self, at: usize) -> usize {
        self.text()
            .get(..at)
            .and_then(|head| head.rfind('\n'))
            .map_or(0, |i| i + 1)
    }

    /// Where the line's break is: its `\n`, or the `\r` of a `\r\n` (one
    /// grapheme, which the cursor never splits); or the end of the text.
    fn line_end(&self, at: usize) -> usize {
        let text = self.text();
        match text.get(at..).and_then(|tail| tail.find('\n')) {
            Some(i) if text[..at + i].ends_with('\r') && i > 0 => at + i - 1,
            Some(i) => at + i,
            None => text.len(),
        }
    }

    /// Up, Down, PgUp and PgDn: `count` lines, to the goal column (in
    /// graphemes) or the end of a shorter line.
    fn vertical(&mut self, down: bool, count: usize, select: bool) {
        let start = self.line_start(self.cursor);
        let goal = self.goal.unwrap_or_else(|| {
            self.text()
                .get(start..self.cursor)
                .map_or(0, |part| part.graphemes(true).count())
        });
        let mut at = self.cursor;
        for _ in 0..count {
            if down {
                let end = self.line_end(at);
                if end >= self.text().len() {
                    break;
                }
                // Past the whole line break, `\n` or `\r\n`.
                at = self.buffer.next_grapheme(end);
            } else {
                let line = self.line_start(at);
                if line == 0 {
                    break;
                }
                // The previous line's start: `line - 1` itself can be the
                // `\n` inside a `\r\n`.
                at = self.line_start(line - 1);
            }
        }
        let line = self.line_start(at);
        let text = self.text().get(line..self.line_end(at)).unwrap_or("");
        let offset: usize = text.graphemes(true).take(goal).map(str::len).sum();
        self.move_to(line + offset, select);
        self.goal = Some(goal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use zeroize::Zeroizing;

    fn editor(text: &str) -> (Editor, Clipboard) {
        (Editor::new(text).unwrap(), Clipboard::default())
    }

    fn typed(ed: &mut Editor, clip: &mut Clipboard, text: &str) {
        for c in text.chars() {
            ed.key(Key::Char(c), clip);
        }
    }

    fn keys(ed: &mut Editor, clip: &mut Clipboard, keys: impl IntoIterator<Item = Key>) {
        for key in keys {
            ed.key(key, clip);
        }
    }

    fn selected(ed: &Editor) -> &str {
        ed.selection().map_or("", |r| &ed.text()[r])
    }

    #[test]
    fn a_run_of_typing_undoes_as_one_step() {
        let (mut ed, mut clip) = editor("");
        typed(&mut ed, &mut clip, "hello");
        keys(&mut ed, &mut clip, [Key::Enter, Key::Backspace]);
        assert_eq!(ed.text(), "hello");
        ed.key(Key::Left, &mut clip);
        typed(&mut ed, &mut clip, "!");
        assert_eq!(ed.text(), "hell!o");
        ed.key(Key::Ctrl('z'), &mut clip);
        assert_eq!(ed.text(), "hello");
        ed.key(Key::Ctrl('z'), &mut clip);
        assert_eq!(ed.text(), "");
        ed.key(Key::Ctrl('y'), &mut clip);
        assert_eq!(ed.text(), "hello");
        assert!(ed.is_dirty());
    }

    #[test]
    fn opening_brackets_close_before_space_closers_and_the_end() {
        // At the end, and nested: a wikilink closes as it is opened.
        let (mut ed, mut clip) = editor("");
        typed(&mut ed, &mut clip, "[[Note");
        assert_eq!(ed.text(), "[[Note]]");
        assert_eq!(ed.cursor(), "[[Note".len());

        // Before whitespace, also a line break.
        let (mut ed, mut clip) = editor("a b\n");
        ed.key(Key::Right, &mut clip);
        typed(&mut ed, &mut clip, "{");
        ed.key(Key::End, &mut clip);
        typed(&mut ed, &mut clip, "(");
        assert_eq!(ed.text(), "a{} b()\n");
        assert_eq!(ed.cursor(), "a{} b(".len());

        // Before a word: no closer.
        let (mut ed, mut clip) = editor("word");
        typed(&mut ed, &mut clip, "(");
        assert_eq!(ed.text(), "(word");
    }

    #[test]
    fn a_typed_closer_steps_over_the_same_closer() {
        // Typed in full, a wikilink and a call come out once.
        let (mut ed, mut clip) = editor("");
        typed(&mut ed, &mut clip, "[[Note]] f(x)");
        assert_eq!(ed.text(), "[[Note]] f(x)");
        assert_eq!(ed.cursor(), ed.text().len());

        // Stepping over changes no text, so there is nothing to save.
        let (mut ed, mut clip) = editor("()");
        ed.key(Key::Right, &mut clip);
        typed(&mut ed, &mut clip, ")");
        assert_eq!(ed.text(), "()");
        assert_eq!(ed.cursor(), 2);
        assert!(!ed.is_dirty());

        // Another closer, or one with an accent on it, is typed.
        let (mut ed, mut clip) = editor(")\u{301}");
        typed(&mut ed, &mut clip, "]");
        typed(&mut ed, &mut clip, ")");
        assert_eq!(ed.text(), "]))\u{301}");
    }

    #[test]
    fn backspace_in_an_empty_pair_deletes_both() {
        // A wikilink opened by mistake goes with two Backspaces.
        let (mut ed, mut clip) = editor("");
        typed(&mut ed, &mut clip, "a[[");
        ed.key(Key::Backspace, &mut clip);
        assert_eq!((ed.text(), ed.cursor()), ("a[]", 2));
        ed.key(Key::Backspace, &mut clip);
        assert_eq!((ed.text(), ed.cursor()), ("a", 1));

        // Not in a pair: one grapheme, as before.
        for (text, right, key, left) in [
            ("()", 1, Key::Delete, "("),      // Delete takes only the closer
            ("()", 2, Key::Backspace, "("),   // after the pair
            ("(]", 1, Key::Backspace, "]"),   // brackets that don't match
            ("(x)", 2, Key::Backspace, "()"), // not empty
            ("()\u{301}", 1, Key::Backspace, ")\u{301}"), // an accented closer
        ] {
            let (mut ed, mut clip) = editor(text);
            for _ in 0..right {
                ed.key(Key::Right, &mut clip);
            }
            ed.key(key, &mut clip);
            assert_eq!(ed.text(), left, "{text:?}");
        }
    }

    #[test]
    fn an_opening_bracket_wraps_the_selection() {
        // `[` twice makes a wikilink; the name stays selected, either way.
        for forward in [true, false] {
            let (mut ed, mut clip) = editor("see Note now");
            let at = if forward { 4 } else { 8 };
            for _ in 0..at {
                ed.key(Key::Right, &mut clip);
            }
            for _ in 0..4 {
                let select = if forward {
                    Key::ShiftRight
                } else {
                    Key::ShiftLeft
                };
                ed.key(select, &mut clip);
            }
            typed(&mut ed, &mut clip, "[[");
            assert_eq!(ed.text(), "see [[Note]] now");
            assert_eq!(selected(&ed), "Note");
            assert_eq!(ed.cursor(), if forward { 10 } else { 6 });
        }

        // A bracket joining an accent: the selection ends stay on
        // grapheme boundaries.
        let (mut ed, mut clip) = editor("\u{301}");
        ed.key(Key::Ctrl('a'), &mut clip);
        typed(&mut ed, &mut clip, "(");
        assert_eq!(ed.text(), "(\u{301})");
        assert_eq!(ed.selection(), None);
        assert_eq!(ed.cursor(), "(\u{301}".len());
    }

    #[test]
    fn brackets_undo_with_the_typing_around_them() {
        // A pair and a step-over.
        let (mut ed, mut clip) = editor("");
        typed(&mut ed, &mut clip, "f(x) ");
        assert_eq!(ed.text(), "f(x) ");
        ed.key(Key::Ctrl('z'), &mut clip);
        assert_eq!(ed.text(), "");

        // A pair deleted by Backspace.
        let (mut ed, mut clip) = editor("");
        typed(&mut ed, &mut clip, "a[");
        ed.key(Key::Backspace, &mut clip);
        typed(&mut ed, &mut clip, "b");
        assert_eq!(ed.text(), "ab");
        ed.key(Key::Ctrl('z'), &mut clip);
        assert_eq!(ed.text(), "");

        // A wrap: two inserts, one step, both ways.
        let (mut ed, mut clip) = editor("Note");
        ed.key(Key::Ctrl('a'), &mut clip);
        typed(&mut ed, &mut clip, "[[");
        ed.key(Key::Ctrl('z'), &mut clip);
        assert_eq!(ed.text(), "Note");
        ed.key(Key::Ctrl('y'), &mut clip);
        assert_eq!(ed.text(), "[[Note]]");
    }

    #[test]
    fn a_full_note_takes_no_half_pair() {
        let (mut ed, mut clip) = editor(&"x".repeat(MAX_BODY_BYTES - 1));
        ed.key(Key::End, &mut clip);
        typed(&mut ed, &mut clip, "(");
        ed.key(Key::Ctrl('a'), &mut clip);
        typed(&mut ed, &mut clip, "[");
        assert_eq!(ed.text().len(), MAX_BODY_BYTES - 1);
        assert_eq!(ed.notice(), Some(FULL));
        assert!(!ed.is_dirty());
    }

    #[test]
    fn shift_selects_and_typing_replaces_the_selection() {
        let (mut ed, mut clip) = editor("one two");
        keys(
            &mut ed,
            &mut clip,
            [Key::ShiftRight, Key::ShiftRight, Key::ShiftRight],
        );
        assert_eq!(selected(&ed), "one");
        typed(&mut ed, &mut clip, "1");
        assert_eq!(ed.text(), "1 two");
        keys(&mut ed, &mut clip, [Key::ShiftEnd]);
        assert_eq!(selected(&ed), " two");
        ed.key(Key::Backspace, &mut clip);
        assert_eq!(ed.text(), "1");
        // A movement without Shift drops the selection.
        keys(&mut ed, &mut clip, [Key::ShiftHome, Key::Right]);
        assert_eq!(ed.selection(), None);
    }

    #[test]
    fn only_ctrl_c_and_ctrl_x_copy() {
        let (mut ed, mut clip) = editor("alpha beta");
        keys(&mut ed, &mut clip, [Key::ShiftEnd, Key::Ctrl('a')]);
        assert_eq!(clip.get(), None, "selecting copies nothing");
        ed.key(Key::Ctrl('c'), &mut clip);
        assert_eq!(clip.get(), Some("alpha beta"));
        keys(
            &mut ed,
            &mut clip,
            [Key::Home, Key::ShiftRight, Key::Ctrl('x')],
        );
        assert_eq!(clip.get(), Some("a"));
        assert_eq!(ed.text(), "lpha beta");
        keys(&mut ed, &mut clip, [Key::End, Key::Ctrl('v')]);
        assert_eq!(ed.text(), "lpha betaa");
        // Pasting replaces the selection, and is one undo step.
        keys(&mut ed, &mut clip, [Key::Ctrl('a'), Key::Ctrl('v')]);
        assert_eq!(ed.text(), "a");
        ed.key(Key::Ctrl('z'), &mut clip);
        assert_eq!(ed.text(), "lpha betaa");
        // Nothing selected: Ctrl+C and Ctrl+X leave the clipboard alone.
        keys(&mut ed, &mut clip, [Key::Ctrl('c'), Key::Ctrl('x')]);
        assert_eq!(clip.get(), Some("a"));
        assert_eq!(ed.text(), "lpha betaa");
    }

    /// The mouse (decision 0073): a press moves the cursor and drops the
    /// selection, a drag after it selects from the press, a release ends
    /// that; a drag without a press on the text, or after a press outside
    /// it, does nothing; the wheel moves three lines. Each ends a typing
    /// run for undo and disarms Ctrl+Q.
    #[test]
    fn mouse_presses_drags_and_wheels() {
        let (mut ed, mut clip) = editor("one\ntwo\nthree\nfour\nfive");
        ed.mouse(MouseEvent::Drag, Some(6), Instant::now());
        assert_eq!((ed.cursor(), ed.selection()), (0, None), "no press yet");
        ed.mouse(MouseEvent::Press, Some(5), Instant::now());
        ed.mouse(MouseEvent::Drag, Some(9), Instant::now());
        ed.mouse(MouseEvent::Drag, Some(10), Instant::now());
        assert_eq!(selected(&ed), "wo\nth");
        ed.mouse(MouseEvent::Release, Some(10), Instant::now());
        ed.mouse(MouseEvent::Drag, Some(2), Instant::now());
        assert_eq!(selected(&ed), "wo\nth", "released");
        ed.mouse(MouseEvent::Press, None, Instant::now());
        ed.mouse(MouseEvent::Drag, Some(2), Instant::now());
        assert_eq!(selected(&ed), "wo\nth", "pressed outside the text");
        ed.mouse(MouseEvent::Press, Some(1), Instant::now());
        assert_eq!((ed.cursor(), ed.selection()), (1, None));
        ed.mouse(MouseEvent::WheelDown, None, Instant::now());
        assert_eq!(ed.cursor(), 15, "three lines down, same column");
        ed.mouse(MouseEvent::WheelUp, None, Instant::now());
        assert_eq!(ed.cursor(), 1);
        // Past the end, and inside a grapheme: snapped.
        let (mut accent, _) = editor("e\u{301}x");
        accent.mouse(MouseEvent::Press, Some(99), Instant::now());
        assert_eq!(accent.cursor(), 4);
        accent.mouse(MouseEvent::Press, Some(1), Instant::now());
        assert_eq!(accent.cursor(), 3);
        // A typing run ends at a click; Ctrl+Q is disarmed.
        typed(&mut ed, &mut clip, "ab");
        ed.mouse(MouseEvent::Press, Some(0), Instant::now());
        typed(&mut ed, &mut clip, "c");
        keys(&mut ed, &mut clip, [Key::Ctrl('z')]);
        assert!(ed.text().starts_with("oab"), "only the c undone");
        keys(&mut ed, &mut clip, [Key::Ctrl('q')]);
        assert!(ed.notice().is_some());
        ed.mouse(MouseEvent::WheelDown, None, Instant::now());
        assert_eq!(ed.key(Key::Ctrl('q'), &mut clip), EditorAction::Stay);
    }

    /// Double and triple clicks (decision 0073): a second press at the
    /// same place within 400 ms selects the word, a third the line with
    /// its break, a fourth is a single click again. A later press, one
    /// elsewhere, or a key in between starts over; a drag after a double
    /// click changes nothing.
    #[test]
    fn double_clicks_select_words_and_triple_clicks_lines() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let (mut ed, mut clip) = editor("one two\nthree, four\n");
        let press = |ed: &mut Editor, at, t| ed.mouse(MouseEvent::Press, Some(at), t);
        press(&mut ed, 5, ms(0));
        assert_eq!((ed.cursor(), ed.selection()), (5, None));
        press(&mut ed, 5, ms(100));
        assert_eq!(selected(&ed), "two");
        ed.mouse(MouseEvent::Drag, Some(0), ms(150));
        assert_eq!(selected(&ed), "two", "no drag after a double click");
        press(&mut ed, 5, ms(200));
        assert_eq!(selected(&ed), "one two\n");
        press(&mut ed, 5, ms(300));
        assert_eq!((ed.cursor(), ed.selection()), (5, None), "fourth: single");
        // Too late, elsewhere, or a key between: single clicks.
        press(&mut ed, 5, ms(1000));
        press(&mut ed, 5, ms(1400));
        assert_eq!(ed.selection(), None, "400 ms apart");
        press(&mut ed, 5, ms(2000));
        press(&mut ed, 6, ms(2100));
        assert_eq!((ed.cursor(), ed.selection()), (6, None), "elsewhere");
        press(&mut ed, 6, ms(3000));
        keys(&mut ed, &mut clip, [Key::Esc]);
        press(&mut ed, 6, ms(3100));
        assert_eq!(ed.selection(), None, "a key between");
        // A space run and a punctuation mark are words; past the line's
        // end there is none.
        let double = |ed: &mut Editor, at, t: u64| {
            press(ed, at, ms(t));
            press(ed, at, ms(t + 1));
        };
        double(&mut ed, 3, 5000);
        assert_eq!(selected(&ed), " ");
        double(&mut ed, 13, 6000);
        assert_eq!(selected(&ed), ",");
        double(&mut ed, 7, 7000);
        assert_eq!((ed.cursor(), ed.selection()), (7, None));
        // Typing replaces the word.
        double(&mut ed, 8, 8000);
        typed(&mut ed, &mut clip, "3");
        assert_eq!(ed.text(), "one two\n3, four\n");
        // The last line has no break; `\r\n` is taken whole; accents stay
        // with their letter.
        let (mut ed, _) = editor("ab\r\ncd\ne\u{301}x");
        for t in 0..3 {
            press(&mut ed, 0, ms(t));
        }
        assert_eq!(selected(&ed), "ab\r\n");
        // Byte 8 starts the accent, inside the grapheme `e\u{301}`.
        for t in 10..13 {
            press(&mut ed, 8, ms(t));
        }
        assert_eq!(selected(&ed), "e\u{301}x");
        double(&mut ed, 8, 100);
        assert_eq!(selected(&ed), "e\u{301}x");
        // Not a char boundary (inside the accent's two bytes): ignored.
        press(&mut ed, 4, ms(200));
        press(&mut ed, 9, ms(1000));
        assert_eq!((ed.cursor(), ed.selection()), (4, None));
    }

    #[test]
    fn moves_by_lines_and_graphemes() {
        let (mut ed, mut clip) = editor("abcd\nx\nwxyz");
        keys(&mut ed, &mut clip, [Key::End, Key::Down]);
        assert_eq!(ed.cursor(), 6, "the end of the shorter line");
        ed.key(Key::Down, &mut clip);
        assert_eq!(ed.cursor(), 11, "the goal column comes back");
        keys(&mut ed, &mut clip, [Key::Home, Key::Up, Key::Up]);
        assert_eq!(ed.cursor(), 0);
        ed.key(Key::Up, &mut clip);
        assert_eq!(ed.cursor(), 0, "the first line stays");

        let (mut ed, mut clip) = editor("e\u{301}x");
        ed.key(Key::Right, &mut clip);
        assert_eq!(ed.cursor(), 3, "e and its accent are one step");
        ed.key(Key::Backspace, &mut clip);
        assert_eq!(ed.text(), "x");
    }

    #[test]
    fn typing_before_a_combining_accent_keeps_the_cursor_on_a_grapheme() {
        let (mut ed, mut clip) = editor("\u{301}");
        typed(&mut ed, &mut clip, "e");
        assert_eq!(ed.text(), "e\u{301}");
        assert_eq!(ed.cursor(), 3, "after the joined grapheme, not inside it");
    }

    /// Found by the property test: a redo that replaced `a` by `é` put
    /// the cursor at a position from before the change, inside the `é`.
    #[test]
    fn undo_and_redo_leave_the_cursor_on_a_boundary() {
        let (mut ed, mut clip) = editor("");
        typed(&mut ed, &mut clip, "\na");
        ed.key(Key::Ctrl('c'), &mut clip);
        typed(&mut ed, &mut clip, "\u{301}");
        ed.key(Key::Backspace, &mut clip);
        typed(&mut ed, &mut clip, "é");
        ed.key(Key::Ctrl('z'), &mut clip);
        assert_eq!((ed.text(), ed.cursor()), ("\na", 2));
        ed.key(Key::Ctrl('y'), &mut clip);
        assert_eq!((ed.text(), ed.cursor()), ("\né", 3));
    }

    #[test]
    fn lines_end_before_a_windows_line_break() {
        let (mut ed, mut clip) = editor("ab\r\ncd");
        keys(&mut ed, &mut clip, [Key::Down, Key::End, Key::Up]);
        assert_eq!(ed.cursor(), 2, "before the \\r\\n, not between them");
    }

    #[test]
    fn pages_move_by_the_view_height() {
        let (mut ed, mut clip) = editor("0\n1\n2\n3\n4\n5");
        ed.set_page(2);
        ed.key(Key::PageDown, &mut clip);
        assert_eq!(ed.cursor(), 4);
        keys(&mut ed, &mut clip, [Key::PageDown, Key::PageDown]);
        assert_eq!(ed.cursor(), 10, "stops on the last line");
        ed.key(Key::PageUp, &mut clip);
        assert_eq!(ed.cursor(), 6);
    }

    #[test]
    fn save_view_and_close_guard_unsaved_changes() {
        let (mut ed, mut clip) = editor("x");
        assert_eq!(ed.key(Key::Ctrl('e'), &mut clip), EditorAction::View);
        assert_eq!(ed.key(Key::Ctrl('q'), &mut clip), EditorAction::Close);
        typed(&mut ed, &mut clip, "y");
        assert_eq!(ed.key(Key::Ctrl('q'), &mut clip), EditorAction::Stay);
        assert_eq!(ed.notice(), Some(UNSAVED));
        // Any other key disarms it.
        ed.key(Key::Left, &mut clip);
        assert_eq!(ed.key(Key::Ctrl('q'), &mut clip), EditorAction::Stay);
        assert_eq!(ed.key(Key::Ctrl('q'), &mut clip), EditorAction::Close);
        assert_eq!(ed.key(Key::Ctrl('s'), &mut clip), EditorAction::Save);
        ed.saved();
        assert_eq!(ed.key(Key::Ctrl('q'), &mut clip), EditorAction::Close);
    }

    #[test]
    fn refusals_and_too_large_pastes_say_so() {
        let (mut ed, mut clip) = editor("");
        let paste = |text: &str, cut| Key::Paste {
            text: Zeroizing::new(text.into()),
            cut,
            cleaned: false,
        };
        // A paste the decoder had to cut is refused whole.
        ed.key(paste("start of a huge paste", true), &mut clip);
        assert_eq!(ed.text(), "");
        assert_eq!(ed.notice(), Some(PASTE_TOO_LARGE));
        assert!(!ed.is_dirty());
        ed.key(paste("a\n\tb", false), &mut clip);
        assert_eq!(ed.text(), "a\n\tb");
        // Found by review: an empty paste deleted the selection.
        ed.key(Key::Ctrl('a'), &mut clip);
        let empty = Key::Paste {
            text: Zeroizing::new(String::new()),
            cut: false,
            cleaned: false,
        };
        ed.key(empty, &mut clip);
        assert_eq!(ed.text(), "a\n\tb");

        let full = "a".repeat(crate::vault::codec::MAX_BODY_BYTES);
        let (mut ed, mut clip) = editor(&full);
        typed(&mut ed, &mut clip, "b");
        assert_eq!(ed.text().len(), full.len());
        assert_eq!(ed.notice(), Some(FULL));
        assert!(!ed.is_dirty());
    }

    /// A cleaned paste goes in and says so until the next key; a clean
    /// one says nothing; a cleaned paste into a full note says only that
    /// the note is full; Ctrl+Z takes the paste back whole.
    #[test]
    fn cleaned_pastes_say_so() {
        let paste = |text: &str, cleaned| Key::Paste {
            text: Zeroizing::new(text.into()),
            cut: false,
            cleaned,
        };
        let (mut ed, mut clip) = editor("x");
        ed.key(paste("plain", false), &mut clip);
        assert_eq!(ed.notice(), None);
        ed.key(paste("ab", true), &mut clip);
        assert_eq!(ed.text(), "plainabx");
        assert_eq!(ed.notice(), Some(PASTE_CLEANED));
        ed.key(Key::Ctrl('z'), &mut clip);
        assert_eq!(ed.text(), "plainx");
        assert_eq!(ed.notice(), None);

        let full = "a".repeat(crate::vault::codec::MAX_BODY_BYTES);
        let (mut ed, mut clip) = editor(&full);
        ed.key(paste("b", true), &mut clip);
        assert_eq!(ed.notice(), Some(FULL));
    }

    fn any_key() -> impl Strategy<Value = Key> {
        prop_oneof![
            prop::sample::select(&['a', 'é', ' ', '😀', '\u{301}', '(', ')', '[', ']'][..])
                .prop_map(Key::Char),
            prop::sample::select(&['a', 'c', 'x', 'v', 'z', 'y', 'q', 's', 'e'][..])
                .prop_map(Key::Ctrl),
            prop::sample::select(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15][..])
                .prop_map(|i| match i {
                    0 => Key::Left,
                    1 => Key::Right,
                    2 => Key::Up,
                    3 => Key::Down,
                    4 => Key::Home,
                    5 => Key::End,
                    6 => Key::PageUp,
                    7 => Key::PageDown,
                    8 => Key::ShiftLeft,
                    9 => Key::ShiftRight,
                    10 => Key::ShiftUp,
                    11 => Key::ShiftDown,
                    12 => Key::ShiftHome,
                    13 => Key::Enter,
                    14 => Key::Backspace,
                    _ => Key::Delete,
                }),
        ]
    }

    proptest! {
        /// Any keys on any text: no panic, and the cursor and selection
        /// ends stay inside the text on grapheme boundaries (brackets
        /// included: they pair, step over, delete in pairs and wrap).
        #[test]
        fn any_keys_keep_cursor_and_selection_valid(
            text in "[ab \r\n.é😀\u{301}()\\[\\]]{0,30}",
            keys in prop::collection::vec(any_key(), 0..60),
        ) {
            let (mut ed, mut clip) = editor(&text);
            for key in keys {
                ed.key(key, &mut clip);
                let len = ed.text().len();
                prop_assert!(ed.cursor() <= len && ed.text().is_char_boundary(ed.cursor()));
                let on_grapheme = |at: usize| {
                    at == len || ed.text().grapheme_indices(true).any(|(i, _)| i == at)
                };
                prop_assert!(on_grapheme(ed.cursor()), "cursor inside a grapheme");
                if let Some(range) = ed.selection() {
                    prop_assert!(range.end <= len);
                    prop_assert!(on_grapheme(range.start), "selection starts inside a grapheme");
                    prop_assert!(on_grapheme(range.end), "selection ends inside a grapheme");
                }
            }
        }
    }
}
