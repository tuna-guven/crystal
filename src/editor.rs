//! The note editor (docs/decisions/0045-simple-editor.md): modeless, with
//! the keys most editors share.
//!
//! The cursor is a caret between graphemes (it may sit at the end of the
//! text). A selection runs from an anchor to the cursor; Shift with a
//! movement key starts or extends it, a movement key alone drops it, and
//! typing replaces it. Only Ctrl+C and Ctrl+X copy, into Crystal's own
//! clipboard; selecting copies nothing.
//!
//! A run of typing (text, Enter, Tab, Backspace, Delete) is one undo step;
//! any other key ends it. A paste is a step of its own.
//!
//! Like the pager, it is terminal-free: keys in, an action out. The view
//! draws it; the session saves, closes, or shows the viewer.

pub mod buffer;
pub mod view;

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use crate::shell::keys::Key;
use crate::ui::clipboard::Clipboard;
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

/// Rows a page moves until the view sets the real height.
const DEFAULT_PAGE: usize = 20;

const FULL: &str = "The note is at its size limit (8 MiB).";
const PASTE_TOO_LARGE: &str = "Not pasted: it is larger than a note can hold (8 MiB).";
const UNSAVED: &str = "Unsaved changes: Ctrl+S saves, Ctrl+Q again discards them.";

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

    /// Shows `notice` until the next key (the session's save results).
    pub fn tell(&mut self, notice: &'static str) {
        self.notice = Some(notice);
    }

    /// Rows the view shows, for PgUp and PgDn.
    pub fn set_page(&mut self, rows: usize) {
        self.page = rows.max(1);
    }

    pub fn key(&mut self, key: Key, clipboard: &mut Clipboard) -> EditorAction {
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
            Key::Char(c) => {
                let mut bytes = [0; 4];
                self.replace_selection(c.encode_utf8(&mut bytes));
            }
            Key::Enter => self.replace_selection("\n"),
            Key::Tab => self.replace_selection("\t"),
            Key::Backspace => self.delete(false),
            Key::Delete => self.delete(true),
            Key::Paste { text, cut } => {
                // Longer than a note can hold: the decoder kept only its
                // start, which would look like the whole. Nothing is pasted.
                if cut {
                    self.notice = Some(PASTE_TOO_LARGE);
                // An empty paste (or one of dropped control characters
                // only) must not delete the selection.
                } else if !text.is_empty() {
                    self.replace_selection(&text);
                    self.buffer.commit();
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
    /// cursor after the new text. A refusal leaves everything as it was.
    fn replace_selection(&mut self, text: &str) {
        let range = self.selection().unwrap_or(self.cursor..self.cursor);
        if range.is_empty() && text.is_empty() {
            return;
        }
        match self.buffer.replace(range.clone(), text) {
            Ok(()) => {
                self.dirty = true;
                self.collapse(range.start + text.len());
            }
            Err(EditError::Full) => self.notice = Some(FULL),
            Err(EditError::NotABoundary) => {}
        }
    }

    /// Backspace and Delete: the selection, or one grapheme before or
    /// after the cursor.
    fn delete(&mut self, forward: bool) {
        if self.selection().is_none() {
            let other = if forward {
                self.buffer.next_grapheme(self.cursor)
            } else {
                self.buffer.prev_grapheme(self.cursor)
            };
            self.anchor = Some(other);
        }
        self.replace_selection("");
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
        self.cursor = if at == 0 {
            0
        } else {
            let start = self.buffer.prev_grapheme(at);
            self.buffer.next_grapheme(start).max(at)
        };
        self.anchor = None;
        self.goal = None;
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

    fn any_key() -> impl Strategy<Value = Key> {
        prop_oneof![
            prop::sample::select(&['a', 'é', ' ', '😀', '\u{301}'][..]).prop_map(Key::Char),
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
        /// stay inside the text on character boundaries.
        #[test]
        fn any_keys_keep_cursor_and_selection_valid(
            text in "[ab \r\n.é😀\u{301}]{0,30}",
            keys in prop::collection::vec(any_key(), 0..60),
        ) {
            let (mut ed, mut clip) = editor(&text);
            for key in keys {
                ed.key(key, &mut clip);
                let len = ed.text().len();
                prop_assert!(ed.cursor() <= len && ed.text().is_char_boundary(ed.cursor()));
                let on_grapheme = ed.cursor() == len
                    || ed.text().grapheme_indices(true).any(|(i, _)| i == ed.cursor());
                prop_assert!(on_grapheme, "cursor inside a grapheme");
                if let Some(range) = ed.selection() {
                    prop_assert!(range.end <= len);
                    prop_assert!(ed.text().is_char_boundary(range.start));
                    prop_assert!(ed.text().is_char_boundary(range.end));
                }
            }
        }
    }
}
