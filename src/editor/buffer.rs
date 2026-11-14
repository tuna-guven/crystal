//! The editor's text and its undo log (docs/decisions/0045-simple-editor.md).
//!
//! Note text must never be left in memory that is not wiped. The buffer
//! therefore never lets its `String` reallocate on its own: before an
//! insert that would not fit, the text is copied into a new buffer of
//! twice the size, and the old one is dropped, which wipes it
//! (`Zeroizing`). Removing text shifts the rest left inside the same
//! allocation and then overwrites the bytes freed at the end. The text is
//! capped at the vault's body limit. An edit moves the text after it, so
//! its cost grows with the note (one `memmove`, about a millisecond at the
//! 8 MiB limit).
//!
//! Positions are byte offsets. An edit is refused unless both ends are on
//! character boundaries, so the text is always valid UTF-8; the editor
//! keeps its cursor on grapheme boundaries with `next_grapheme` and
//! `prev_grapheme`.
//!
//! Undo is a log of edits, not copies of the note: each edit keeps its
//! position and the removed and inserted text, in zeroizing memory. The
//! edits made between two `commit`s (a run of typing, one paste) are one
//! step. At most `MAX_UNDO` steps and `MAX_UNDO_BYTES` of text are kept,
//! the oldest steps dropped first: all memory is locked (`mlockall`), and
//! a log that grew past the memlock limit would abort Crystal.

use std::collections::VecDeque;
use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;
use zeroize::Zeroizing;

use crate::vault::codec::MAX_BODY_BYTES;

/// Undo steps kept.
pub const MAX_UNDO: usize = 200;

/// Text kept in the undo log (open step, undo and redo together): eight
/// full-size notes. One step holds at most two (removed and inserted), so
/// the newest step always fits.
pub const MAX_UNDO_BYTES: usize = 8 * MAX_BODY_BYTES;

/// The smallest allocation, so typing into a short note rarely grows it.
const MIN_CAPACITY: usize = 4096;

/// Why an edit was refused. Nothing changed in either case.
#[derive(Debug, PartialEq, Eq)]
pub enum EditError {
    /// The text would pass the body limit.
    Full,
    /// A position past the end or inside a character.
    NotABoundary,
}

/// One change: at `at`, `removed` was replaced by `inserted`.
struct Edit {
    at: usize,
    removed: Zeroizing<String>,
    inserted: Zeroizing<String>,
}

/// The text bytes a step keeps.
fn step_bytes(step: &[Edit]) -> usize {
    step.iter()
        .map(|edit| edit.removed.len() + edit.inserted.len())
        .sum()
}

/// One note's text while it is edited. No Debug or Clone: it holds note
/// text.
pub struct Buffer {
    text: Zeroizing<String>,
    /// The step being made, until `commit`.
    open: Vec<Edit>,
    undo: VecDeque<Vec<Edit>>,
    redo: Vec<Vec<Edit>>,
    /// Text bytes in `open`, `undo` and `redo`.
    logged: usize,
}

impl Buffer {
    /// A buffer holding a copy of `text` (a note's body).
    pub fn new(text: &str) -> Result<Self, EditError> {
        if text.len() > MAX_BODY_BYTES {
            return Err(EditError::Full);
        }
        let mut copy = Zeroizing::new(String::with_capacity(capacity_for(text.len(), 0)));
        copy.push_str(text);
        Ok(Self {
            text: copy,
            open: Vec::new(),
            undo: VecDeque::new(),
            redo: Vec::new(),
            logged: 0,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Replaces `range` with `text`, as part of the open undo step. A new
    /// edit makes the undone steps unreachable, so redo is cleared.
    pub fn replace(&mut self, range: Range<usize>, text: &str) -> Result<(), EditError> {
        if range.start > range.end
            || !self.text.is_char_boundary(range.start)
            || !self.text.is_char_boundary(range.end)
        {
            return Err(EditError::NotABoundary);
        }
        if self.text.len() - range.len() + text.len() > MAX_BODY_BYTES {
            return Err(EditError::Full);
        }
        if range.is_empty() && text.is_empty() {
            return Ok(());
        }
        let removed = Zeroizing::new(self.text[range.clone()].to_owned());
        self.apply(range.start, range.len(), text);
        self.logged += removed.len() + text.len();
        self.open.push(Edit {
            at: range.start,
            removed,
            inserted: Zeroizing::new(text.to_owned()),
        });
        // Dropping the undone steps wipes their text.
        for step in self.redo.drain(..) {
            self.logged -= step_bytes(&step);
        }
        Ok(())
    }

    /// Ends the open step: the edits since the last commit undo together.
    pub fn commit(&mut self) {
        if !self.open.is_empty() {
            let step = std::mem::take(&mut self.open);
            self.push_undo(step);
        }
    }

    /// Undoes the last step and returns where it ends in the restored text
    /// (after what the step had removed), or `None` when there is nothing
    /// to undo. An open step is committed first. The position is measured
    /// in the text as it is now, so it is always a character boundary.
    pub fn undo(&mut self) -> Option<usize> {
        self.commit();
        let step = self.undo.pop_back()?;
        for edit in step.iter().rev() {
            self.apply(edit.at, edit.inserted.len(), &edit.removed);
        }
        // Applied last, so its position holds in the text now.
        let at = step.first().map_or(0, |edit| edit.at + edit.removed.len());
        self.redo.push(step);
        Some(at)
    }

    /// Redoes the last undone step and returns where it ends (after the
    /// text its last edit inserted).
    pub fn redo(&mut self) -> Option<usize> {
        self.commit();
        let step = self.redo.pop()?;
        for edit in &step {
            self.apply(edit.at, edit.removed.len(), &edit.inserted);
        }
        let at = step.last().map_or(0, |edit| edit.at + edit.inserted.len());
        self.push_undo(step);
        Some(at)
    }

    /// The grapheme boundary after `at`, or the end. `at` must be a
    /// character boundary; otherwise `at` comes back unchanged.
    pub fn next_grapheme(&self, at: usize) -> usize {
        self.text
            .get(at..)
            .and_then(|rest| rest.graphemes(true).next())
            .map_or(at.min(self.text.len()), |g| at + g.len())
    }

    /// The grapheme boundary before `at`, or 0.
    pub fn prev_grapheme(&self, at: usize) -> usize {
        self.text
            .get(..at)
            .and_then(|head| head.grapheme_indices(true).next_back())
            .map_or(0, |(start, _)| start)
    }

    /// Adds `step` (already counted in `logged`), then drops the oldest
    /// steps until the log is within `MAX_UNDO` and `MAX_UNDO_BYTES`.
    fn push_undo(&mut self, step: Vec<Edit>) {
        self.undo.push_back(step);
        while self.undo.len() > MAX_UNDO || (self.logged > MAX_UNDO_BYTES && self.undo.len() > 1) {
            if let Some(oldest) = self.undo.pop_front() {
                self.logged -= step_bytes(&oldest);
            }
        }
    }

    /// Removes `remove` bytes at `at` and inserts `insert` there. The
    /// caller has checked boundaries and the limit (undo and redo return
    /// to texts that were within it).
    fn apply(&mut self, at: usize, remove: usize, insert: &str) {
        self.remove(at, at + remove);
        self.reserve(insert.len());
        // Fits the capacity, so `insert_str` moves bytes but never
        // reallocates.
        self.text.insert_str(at, insert);
    }

    /// Removes `start..end` in place and wipes the bytes freed at the end.
    fn remove(&mut self, start: usize, end: usize) {
        let freed = end - start;
        if freed == 0 {
            return;
        }
        let len = self.text.len();
        // Shifts the tail left; the last `freed` bytes of the allocation
        // still hold old text.
        drop(self.text.drain(start..end));
        // Writing NULs over them stays within the capacity (the text was
        // `len` bytes long), so this never reallocates; then cut them off.
        self.text.extend(std::iter::repeat_n('\0', freed));
        self.text.truncate(len - freed);
    }

    /// Makes room for `extra` more bytes: copies the text into a larger
    /// buffer and wipes the old one, instead of letting `String` grow.
    fn reserve(&mut self, extra: usize) {
        let need = self.text.len() + extra;
        if need <= self.text.capacity() {
            return;
        }
        let mut grown = Zeroizing::new(String::with_capacity(capacity_for(
            need,
            self.text.capacity(),
        )));
        grown.push_str(&self.text);
        // Dropping the old buffer wipes all of its capacity.
        self.text = grown;
    }
}

/// Twice the current capacity, at least `need` and `MIN_CAPACITY`, at most
/// the body limit unless `need` itself is larger.
fn capacity_for(need: usize, current: usize) -> usize {
    current
        .saturating_mul(2)
        .clamp(MIN_CAPACITY, MAX_BODY_BYTES)
        .max(need)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn typed(b: &mut Buffer, at: usize, text: &str) {
        b.replace(at..at, text).unwrap();
    }

    #[test]
    fn inserts_removes_and_replaces() {
        let mut b = Buffer::new("hello world").unwrap();
        b.replace(0..5, "goodbye").unwrap();
        assert_eq!(b.as_str(), "goodbye world");
        b.replace(7..13, "").unwrap();
        assert_eq!(b.as_str(), "goodbye");
        typed(&mut b, 7, "!");
        assert_eq!(b.as_str(), "goodbye!");
    }

    #[test]
    fn refuses_positions_inside_a_character_or_past_the_end() {
        let mut b = Buffer::new("é").unwrap();
        assert_eq!(b.replace(1..1, "x"), Err(EditError::NotABoundary));
        assert_eq!(b.replace(0..1, ""), Err(EditError::NotABoundary));
        assert_eq!(b.replace(3..3, "x"), Err(EditError::NotABoundary));
        #[expect(clippy::reversed_empty_ranges, reason = "the case under test")]
        let backwards = 2..0;
        assert_eq!(b.replace(backwards, ""), Err(EditError::NotABoundary));
        assert_eq!(b.as_str(), "é");
    }

    #[test]
    fn refuses_text_past_the_body_limit_and_changes_nothing() {
        assert!(matches!(
            Buffer::new(&"a".repeat(MAX_BODY_BYTES + 1)),
            Err(EditError::Full)
        ));
        let mut b = Buffer::new(&"a".repeat(MAX_BODY_BYTES - 1)).unwrap();
        assert_eq!(b.replace(0..0, "bc"), Err(EditError::Full));
        assert_eq!(b.as_str().len(), MAX_BODY_BYTES - 1);
        b.replace(0..1, "bc").unwrap();
        assert_eq!(b.as_str().len(), MAX_BODY_BYTES);
    }

    #[test]
    fn edits_between_commits_undo_and_redo_as_one_step() {
        let mut b = Buffer::new("x").unwrap();
        typed(&mut b, 1, "a");
        typed(&mut b, 2, "b");
        b.commit();
        b.replace(0..1, "").unwrap();
        assert_eq!(b.as_str(), "ab");
        assert_eq!(b.undo(), Some(1));
        assert_eq!(b.as_str(), "xab");
        assert_eq!(b.undo(), Some(1));
        assert_eq!(b.as_str(), "x");
        assert_eq!(b.undo(), None);
        assert_eq!(b.redo(), Some(3));
        assert_eq!(b.as_str(), "xab");
        assert_eq!(b.redo(), Some(0));
        assert_eq!(b.as_str(), "ab");
        assert_eq!(b.redo(), None);
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let mut b = Buffer::new("").unwrap();
        typed(&mut b, 0, "a");
        b.undo();
        typed(&mut b, 0, "b");
        assert_eq!(b.redo(), None);
        assert_eq!(b.as_str(), "b");
    }

    #[test]
    fn keeps_at_most_max_undo_steps() {
        let mut b = Buffer::new("").unwrap();
        for i in 0..=MAX_UNDO {
            typed(&mut b, i, "a");
            b.commit();
        }
        for _ in 0..MAX_UNDO {
            assert!(b.undo().is_some());
        }
        assert_eq!(b.undo(), None);
        // The oldest step fell off the log, so its edit stays.
        assert_eq!(b.as_str(), "a");
    }

    /// What `logged` must be: the text in the open step, undo and redo.
    fn recount(b: &Buffer) -> usize {
        step_bytes(&b.open)
            + b.undo.iter().map(|s| step_bytes(s)).sum::<usize>()
            + b.redo.iter().map(|s| step_bytes(s)).sum::<usize>()
    }

    /// Found by review: select all and paste on a full-size note, again
    /// and again, grew the log until locked memory ran out and Crystal
    /// aborted. The log now stays within its byte limit by dropping the
    /// oldest steps; the newest still undo.
    #[test]
    fn keeps_at_most_max_undo_bytes() {
        let a = "a".repeat(MAX_BODY_BYTES);
        let b_text = "b".repeat(MAX_BODY_BYTES);
        let mut b = Buffer::new(&a).unwrap();
        for i in 0..10 {
            let next = if i % 2 == 0 { &b_text } else { &a };
            b.replace(0..MAX_BODY_BYTES, next).unwrap();
            b.commit();
            assert!(b.logged <= MAX_UNDO_BYTES, "step {i}: {}", b.logged);
            assert_eq!(b.logged, recount(&b));
        }
        // Each step holds two full notes: four steps fit, the rest fell off.
        assert_eq!(b.undo.len(), MAX_UNDO_BYTES / (2 * MAX_BODY_BYTES));
        while b.undo().is_some() {}
        // Steps 6 to 9 were kept: undone, the text is step 5's, all `a`.
        assert_eq!(b.as_str(), a, "back to before the oldest step kept");
        assert_eq!(b.logged, recount(&b), "undo moves steps, never loses count");
        b.replace(0..1, "c").unwrap();
        assert_eq!(b.redo.len(), 0);
        assert_eq!(b.logged, recount(&b), "a new edit drops redo and its bytes");
    }

    #[test]
    fn grows_by_doubling_and_keeps_its_allocation_while_text_fits() {
        let mut b = Buffer::new("").unwrap();
        assert_eq!(b.text.capacity(), MIN_CAPACITY);
        let start = b.text.as_ptr();
        typed(&mut b, 0, &"a".repeat(MIN_CAPACITY));
        b.replace(0..MIN_CAPACITY, "").unwrap();
        assert_eq!(
            b.text.as_ptr(),
            start,
            "an edit that fits never moves the text"
        );
        typed(&mut b, 0, &"a".repeat(MIN_CAPACITY + 1));
        assert_eq!(b.text.capacity(), 2 * MIN_CAPACITY);
        typed(&mut b, 0, &"a".repeat(5 * MIN_CAPACITY));
        assert_eq!(b.text.capacity(), 6 * MIN_CAPACITY + 1);
        assert_eq!(capacity_for(MAX_BODY_BYTES, MAX_BODY_BYTES), MAX_BODY_BYTES);
    }

    /// The bytes a removal frees at the end of the allocation are wiped,
    /// not left behind past the text's length.
    #[test]
    #[allow(unsafe_code)]
    fn removal_wipes_the_freed_bytes() {
        let mut b = Buffer::new("public SECRET").unwrap();
        b.replace(6..13, "").unwrap();
        assert_eq!(b.as_str(), "public");
        let spare = b.text.capacity() - b.text.len();
        assert!(spare >= 7);
        // SAFETY: the 7 bytes after the text are inside the String's own
        // allocation (capacity checked above) and were written by
        // `remove`, so they are initialized; they are only read.
        let freed = unsafe { std::slice::from_raw_parts(b.text.as_ptr().add(6), 7) };
        assert_eq!(freed, [0; 7]);
    }

    #[test]
    fn steps_over_whole_graphemes() {
        // e + combining acute, a family emoji (joined by ZWJ), x.
        let text = "e\u{301}👨\u{200d}👩\u{200d}👧x";
        let b = Buffer::new(text).unwrap();
        let family = 3 + "👨\u{200d}👩\u{200d}👧".len();
        assert_eq!(b.next_grapheme(0), 3);
        assert_eq!(b.next_grapheme(3), family);
        assert_eq!(b.next_grapheme(family), text.len());
        assert_eq!(b.next_grapheme(text.len()), text.len());
        assert_eq!(b.prev_grapheme(text.len()), family);
        assert_eq!(b.prev_grapheme(family), 3);
        assert_eq!(b.prev_grapheme(3), 0);
        assert_eq!(b.prev_grapheme(0), 0);
        // Byte 2 is inside the combining accent (bytes 1 to 2): no panic,
        // and nothing to step over.
        assert_eq!(b.next_grapheme(2), 2);
        assert_eq!(b.prev_grapheme(2), 0);
    }

    /// The nearest character boundary at or before `at`.
    fn floor_boundary(text: &str, at: usize) -> usize {
        let mut at = at.min(text.len());
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    proptest! {
        /// Any sequence of edits gives the same text as a plain `String`;
        /// undoing every step returns the original exactly, and redoing
        /// every step returns the last text.
        #[test]
        fn undo_and_redo_restore_texts_exactly(
            start in "\\PC{0,40}",
            ops in prop::collection::vec(
                (any::<usize>(), any::<usize>(), "\\PC{0,8}", any::<bool>()),
                0..40,
            ),
        ) {
            let mut b = Buffer::new(&start).unwrap();
            let mut model = start.clone();
            let mut steps = 0;
            for (a, len, text, commit) in ops {
                let s = floor_boundary(&model, a % (model.len() + 1));
                let e = floor_boundary(&model, s + len % 8);
                b.replace(s..e, &text).unwrap();
                model.replace_range(s..e, &text);
                prop_assert_eq!(b.as_str(), model.as_str());
                if commit {
                    b.commit();
                }
                prop_assert_eq!(b.logged, recount(&b));
            }
            b.commit();
            while let Some(at) = b.undo() {
                prop_assert!(b.as_str().is_char_boundary(at));
                steps += 1;
            }
            prop_assert_eq!(b.as_str(), start.as_str());
            for _ in 0..steps {
                let at = b.redo();
                prop_assert!(at.is_some_and(|at| b.as_str().is_char_boundary(at)));
            }
            prop_assert_eq!(b.as_str(), model.as_str());
        }

        /// Grapheme steps from any boundary stay on boundaries and move.
        #[test]
        fn grapheme_steps_land_on_boundaries(text in "\\PC{0,30}", at in any::<usize>()) {
            let b = Buffer::new(&text).unwrap();
            let at = floor_boundary(&text, at % (text.len() + 1));
            let next = b.next_grapheme(at);
            let prev = b.prev_grapheme(at);
            prop_assert!(text.is_char_boundary(next) && text.is_char_boundary(prev));
            prop_assert!(next > at || at == text.len());
            prop_assert!(prev < at || at == 0);
        }
    }
}
