//! The brackets the editor closes as you type (decision 0045; their own
//! file since step RF6): `()`, `[]` and `{}`. An opening one brings its
//! closer, a typed closer steps over the same one, and Backspace in an
//! empty pair deletes both. An opening bracket typed over a selection
//! wraps it.
//!
//! Each pair is two ASCII bytes, so both brackets go in or neither (the
//! note's size limit is checked for both), and a bracket that carries a
//! combining accent is never taken for one: the checks compare whole
//! graphemes. The key dispatch (`Editor::key`) asks these functions; they
//! never handle a key themselves.

use std::ops::Range;

use super::{Editor, FULL};
use crate::vault::codec::MAX_BODY_BYTES;

/// The brackets the editor closes as you type (decision 0045). Each is
/// two ASCII bytes, so the cursor fits between them. Quotes are left out:
/// `don't` must not open a pair.
const PAIRS: [&str; 3] = ["()", "[]", "{}"];

/// The pair an opening bracket starts.
pub(super) fn pair_of(c: char) -> Option<&'static str> {
    PAIRS.iter().copied().find(|pair| pair.starts_with(c))
}

impl Editor {
    /// Puts the pair around the selection, which stays selected inside
    /// it. The closer goes in first, so `range.start` still holds for the
    /// opener; the room is checked first, so both go in or neither.
    pub(super) fn wrap(&mut self, pair: &str, range: Range<usize>) {
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
    pub(super) fn closes_here(&self) -> bool {
        self.text()[self.cursor..]
            .chars()
            .next()
            .is_none_or(|next| next.is_whitespace() || PAIRS.iter().any(|p| p.ends_with(next)))
    }

    /// Whether `c` is a closing bracket and the grapheme after the cursor
    /// is that bracket alone (not `)` with a combining accent on it).
    pub(super) fn closer_follows(&self, c: char) -> bool {
        let next = self.buffer.next_grapheme(self.cursor);
        PAIRS.iter().any(|p| p.ends_with(c)) && self.text()[self.cursor..next].chars().eq([c])
    }

    /// Whether the graphemes around the cursor are exactly one of the
    /// pairs: two bytes in all, so neither bracket carries an accent.
    pub(super) fn in_empty_pair(&self) -> bool {
        let start = self.buffer.prev_grapheme(self.cursor);
        let end = self.buffer.next_grapheme(self.cursor);
        PAIRS.contains(&&self.text()[start..end])
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{editor, selected, typed};
    use super::*;
    use crate::shell::keys::Key;

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
}
