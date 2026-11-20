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
//! The brackets `()`, `[]` and `{}` close as you type (`brackets`); the
//! cursor's moves are in `motion`.
//!
//! Inside `[[` it suggests notes to link (`suggest`): the session's keys
//! and mouse go through `key_in` and `mouse_in`, which wrap `key` and
//! `mouse`.
//!
//! A run of typing (text, Enter, Tab, Backspace, Delete) is one undo step;
//! any other key ends it. A paste is a step of its own.
//!
//! Like the pager, it is terminal-free: keys in, an action out. The view
//! draws it; the session saves, closes, or shows the viewer.
//!
//! Its parts, one job each (steps RF1 to RF8): this file holds the
//! editor's state and the key and mouse dispatch; `buffer` the text and
//! its undo log, wiped; `motion` where the cursor and the selection go;
//! `brackets` the closing pairs; `cells` a table's cells in live mode; `suggest` the link list (`query` what a
//! typed link matches, `ids` new block ids); `live` live mode's parse and
//! line layout; `view` the screen.

mod brackets;
pub mod buffer;
mod cells;
pub mod live;
mod motion;
pub mod suggest;
pub mod view;

use std::ops::Range;
use std::time::{Duration, Instant};

use crate::shell::keys::{Key, MouseEvent};
use crate::ui::clipboard::Clipboard;
use brackets::pair_of;
use buffer::{Buffer, EditError};

/// What the session does after a key.
#[derive(Debug, PartialEq, Eq)]
pub enum EditorAction {
    Stay,
    /// Ctrl+S.
    Save,
    /// Ctrl+Q: close, discarding unsaved changes (none, or their save
    /// failed and Ctrl+Q came again).
    Close,
    /// Ctrl+Q with unsaved changes: save them, then close; if the save
    /// fails, stay (`UNSAVED`), and Ctrl+Q right after closes.
    SaveAndClose,
    /// Ctrl+E: show the current text in the viewer.
    View,
    /// A block of another note without an id was chosen: the session
    /// saves a new id into it, then writes the link
    /// (`Editor::take_block_request`, decision 0075).
    LinkBlock,
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
/// Ctrl+Q's save failed: the editor stays, armed (the full error is in
/// the shell).
pub const UNSAVED: &str = "Not saved (why: in the shell after Ctrl+Q). Ctrl+Q again closes \
                           without saving.";

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
    /// The open list of link suggestions (`suggest`), if any.
    suggestions: Option<suggest::List>,
    /// The headings of the text, scanned for a list of them (`[[#`);
    /// empty otherwise.
    own_headings: Vec<zeroize::Zeroizing<String>>,
    /// Where the link starts whose list `own_headings` were scanned for:
    /// typing on in that link reuses them (`suggest`).
    own_headings_at: Option<usize>,
    /// The blocks a list of them points into, as ranges, and what they
    /// were parsed from (`suggest::BlocksFor`).
    blocks: Vec<crate::md::blocks::Block>,
    blocks_for: Option<suggest::BlocksFor>,
    /// Where new block ids' randomness comes from (`suggest::new_id`).
    random: suggest::Random,
    /// What `EditorAction::LinkBlock` asks of the session.
    block_request: Option<suggest::BlockRequest>,
    /// The note shown, if saved, and whether it is Markdown (`set_note`).
    own: Option<usize>,
    markdown: bool,
    /// Shown in live mode (`set_live`): in a table, keys move through
    /// its cells (`cells`).
    live: bool,
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
            suggestions: None,
            own_headings: Vec::new(),
            own_headings_at: None,
            blocks: Vec::new(),
            blocks_for: None,
            random: crate::crypto::fill_random,
            block_request: None,
            own: None,
            markdown: true,
            live: false,
        })
    }

    /// Whether the view shows this editor in live mode, where tables are
    /// boxed and keys move through their cells (steps TB2 to TB5). The
    /// session sets it on opening.
    pub fn set_live(&mut self, live: bool) {
        self.live = live;
    }

    /// The text's changes since the last call (offsets only), for the
    /// view's folds; the view takes them every frame.
    pub fn take_changes(&mut self) -> Vec<buffer::Change> {
        self.buffer.take_changes()
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
    /// disarms Ctrl+Q; it closes the list of link suggestions.
    pub fn mouse(&mut self, event: MouseEvent, at: Option<usize>, now: Instant) {
        // Only the viewer's mode reports it (decision 0076).
        if event == MouseEvent::Move {
            return;
        }
        self.close_list();
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
            (MouseEvent::Drag | MouseEvent::Move, _) => {}
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
        // Tab and Shift+Tab in a live table go from cell to cell (step
        // TB4): a move, which ends a run of typing; elsewhere Tab types.
        let cells = match key {
            Key::Tab | Key::BackTab => self.cells_at(self.cursor),
            _ => None,
        };
        let typing = cells.is_none()
            && matches!(
                key,
                Key::Char(_) | Key::Enter | Key::Tab | Key::Backspace | Key::Delete
            );
        if !typing {
            self.buffer.commit();
        }
        if let Some(cells) = cells {
            self.tab_cell(&cells, key == Key::Tab);
            return EditorAction::Stay;
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
            Key::Left | Key::ShiftLeft => self.horizontal(false, key == Key::ShiftLeft),
            Key::Right | Key::ShiftRight => self.horizontal(true, key == Key::ShiftRight),
            Key::Up | Key::ShiftUp => self.vertical(false, 1, key == Key::ShiftUp),
            Key::Down | Key::ShiftDown => self.vertical(true, 1, key == Key::ShiftDown),
            Key::PageUp => self.vertical(false, self.page, false),
            Key::PageDown => self.vertical(true, self.page, false),
            Key::Home | Key::ShiftHome => self.home_end(false, key == Key::ShiftHome),
            Key::End | Key::ShiftEnd => self.home_end(true, key == Key::ShiftEnd),
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
                return EditorAction::SaveAndClose;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use unicode_segmentation::UnicodeSegmentation;
    use zeroize::Zeroizing;

    pub(super) fn editor(text: &str) -> (Editor, Clipboard) {
        (Editor::new(text).unwrap(), Clipboard::default())
    }

    pub(super) fn typed(ed: &mut Editor, clip: &mut Clipboard, text: &str) {
        for c in text.chars() {
            ed.key(Key::Char(c), clip);
        }
    }

    pub(super) fn keys(ed: &mut Editor, clip: &mut Clipboard, keys: impl IntoIterator<Item = Key>) {
        for key in keys {
            ed.key(key, clip);
        }
    }

    pub(super) fn selected(ed: &Editor) -> &str {
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
        ed.mouse(MouseEvent::WheelDown, None, Instant::now());
        assert_eq!(
            ed.key(Key::Ctrl('q'), &mut clip),
            EditorAction::SaveAndClose
        );
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
    fn save_view_and_close_guard_unsaved_changes() {
        let (mut ed, mut clip) = editor("x");
        assert_eq!(ed.key(Key::Ctrl('e'), &mut clip), EditorAction::View);
        assert_eq!(ed.key(Key::Ctrl('q'), &mut clip), EditorAction::Close);
        // Unsaved: Ctrl+Q saves first; if that fails, Ctrl+Q right after
        // closes.
        typed(&mut ed, &mut clip, "y");
        assert_eq!(
            ed.key(Key::Ctrl('q'), &mut clip),
            EditorAction::SaveAndClose
        );
        // Any other key disarms it.
        ed.key(Key::Left, &mut clip);
        assert_eq!(
            ed.key(Key::Ctrl('q'), &mut clip),
            EditorAction::SaveAndClose
        );
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
            prop::sample::select(
                &[
                    'a', 'p', 'é', ' ', '😀', '\u{301}', '(', ')', '[', ']', '#', '^'
                ][..]
            )
            .prop_map(Key::Char),
            prop::sample::select(&['a', 'c', 'x', 'v', 'z', 'y', 'q', 's', 'e'][..])
                .prop_map(Key::Ctrl),
            prop::sample::select(
                &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17][..]
            )
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
                15 => Key::Tab,
                16 => Key::Esc,
                _ => Key::Delete,
            }),
        ]
    }

    /// A key, or a mouse event on a text position (picked among the
    /// text's graphemes) and maybe on a row of the list.
    #[derive(Debug)]
    enum Input {
        Key(Key),
        Mouse(MouseEvent, usize, Option<usize>),
    }

    fn any_input() -> impl Strategy<Value = Input> {
        let event = prop::sample::select(
            &[
                MouseEvent::Press,
                MouseEvent::Drag,
                MouseEvent::Release,
                MouseEvent::WheelUp,
                MouseEvent::WheelDown,
                MouseEvent::Move,
            ][..],
        );
        prop_oneof![
            4 => any_key().prop_map(Input::Key),
            1 => (event, any::<usize>(), prop::option::of(0..10usize))
                .prop_map(|(event, at, row)| Input::Mouse(event, at, row)),
        ]
    }

    proptest! {
        /// Any keys and mouse events on any text, through `key_in` and
        /// `mouse_in` with link suggestions (on every kind of list):
        /// no panic, the cursor and selection ends stay inside the text on
        /// grapheme boundaries, and an open list is valid (brackets
        /// included: they pair, step over, delete in pairs and wrap).
        #[test]
        fn any_keys_keep_cursor_and_selection_valid(
            text in "[ab \r\n.é😀\u{301}()\\[\\]]{0,30}",
            inputs in prop::collection::vec(any_input(), 0..60),
            prefix in prop::sample::select(vec!["", "[[", "[[#", "[[#^", "[[pa#", "[[pa#^"]),
        ) {
            // A heading and blocks of its own, for `[[#` and `[[#^`.
            let text = format!("{text}\n\n# Own head\n\nown para ^o1\n\nown plain\n");
            let (mut ed, mut clip) = editor(&text);
            // More notes than the list shows, some starting with `p`.
            let names = ["pa", "pb", "a/pc", "b/pa", "pe", "pf", "pg", "ph", "pi", "x"];
            let notes = crate::notes::tests::notes(
                names
                    .iter()
                    .zip(1u8..)
                    .map(|(name, id)| {
                        let (folder, title) = name.rsplit_once('/').unwrap_or(("", name));
                        // Headings to list after `[[pa#`, one that cannot
                        // be linked (`]`).
                        crate::notes::tests::note(
                            id,
                            folder,
                            title,
                            "# pa\n## a p\n# x]y\n\npara ^p1\n\nplain a\n",
                        )
                    })
                    .collect(),
            );
            // Often into a heading or block list first: random keys alone
            // seldom type `[[#^`.
            let inputs = prefix.chars().map(|c| Input::Key(Key::Char(c))).chain(inputs);
            for input in inputs {
                // Up, Down and Esc, and the wheel over an open list,
                // never change the text.
                let open = ed.suggestions().map_or(0, |list| list.entries.len());
                let moves_list = open > 0
                    && match &input {
                        Input::Key(key) => matches!(key, Key::Up | Key::Down | Key::Esc),
                        Input::Mouse(event, _, row) => {
                            row.is_some_and(|row| row < open)
                                && matches!(event, MouseEvent::WheelUp | MouseEvent::WheelDown)
                        }
                    };
                let before = ed.text().to_owned();
                match input {
                    Input::Key(key) => {
                        ed.key_in(key, &mut clip, &notes);
                    }
                    Input::Mouse(event, at, row) => {
                        let starts: Vec<usize> = ed
                            .text()
                            .grapheme_indices(true)
                            .map(|(i, _)| i)
                            .chain([ed.text().len()])
                            .collect();
                        let at = starts[at % starts.len()];
                        ed.mouse_in(event, Some(at), row, Instant::now(), &notes);
                    }
                }
                if moves_list {
                    prop_assert_eq!(ed.text(), before.as_str());
                }
                if let Some(list) = ed.suggestions() {
                    prop_assert!(!list.entries.is_empty());
                    prop_assert!(list.highlight < list.entries.len());
                    prop_assert!(list.top <= list.highlight);
                    prop_assert!(list.highlight < list.top + suggest::ROWS);
                    // The edited text's blocks, kept and moved while typing
                    // in the link, are those a fresh parse finds.
                    if matches!(list.source, suggest::Source::Blocks(None)) {
                        let kept: Vec<_> =
                            ed.blocks.iter().map(|b| (b.text.clone(), b.id.clone())).collect();
                        let fresh: Vec<_> = crate::md::blocks::blocks(ed.text())
                            .into_iter()
                            .map(|b| (b.text, b.id))
                            .collect();
                        prop_assert_eq!(kept, fresh);
                    }
                    // Every entry has its text: a note, heading or block.
                    prop_assert!((0..list.entries.len()).all(|i| ed.suggestion_label(i, &notes).is_some()));
                    prop_assert!(suggest::target(ed.text(), ed.cursor()).is_some());
                }
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
