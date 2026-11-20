//! New block ids (decision 0075; its own file since step RF7): a block
//! chosen in the list that has no `^id` gets one, written at the block's
//! end, and the link is written with it.
//!
//! An id is `ID_LEN` characters drawn from the system's randomness by
//! rejection sampling (`new_id`), so every character is equally likely,
//! and never one the note already has. In the edited text the id and the
//! link go in as one undo step (`link_new_id`); in another note the
//! editor writes nothing there itself: it asks the session
//! (`BlockRequest`), which saves that note, then writes the link
//! (`Editor::link_block`). Every refusal changes nothing and says so: no
//! id could be drawn, `add_id` refused (the note would parse otherwise),
//! or the note is full. Requests hold positions and the id, never note
//! text.

use std::ops::Range;

use rustix::io::Errno;

use super::query::{Part, target};
use crate::editor::Editor;
use crate::editor::buffer::EditError;
use crate::md::blocks::{Block, add_id};
use crate::md::render::block_id;
use crate::notes::Notes;
use crate::vault::codec::MAX_BODY_BYTES;

/// No id could be drawn: the random source failed, or 8 were taken.
const NO_NEW_ID: &str = "No new ^id could be made; nothing was changed.";

/// `add_id` refused: the id would change how the note parses.
pub const ID_REFUSED: &str = "An ^id there would change the note's Markdown; nothing was changed.";

/// The id and the link do not fit in a note.
pub const ID_FULL: &str = "The note is at its size limit (8 MiB); nothing was changed.";

/// A new id for a block of another note, asked of the session
/// (`EditorAction::LinkBlock`, decision 0075): it saves the id into that
/// note, then calls `Editor::link_block`. Positions and the random id
/// only, no note text.
pub struct BlockRequest {
    pub note: usize,
    /// The block's text, a range of that note's saved body.
    pub block: Range<usize>,
    id: [u8; ID_LEN],
    /// The text after `#^` in the edited text's link.
    typed: Range<usize>,
}

impl BlockRequest {
    /// The new id: ASCII from `ID_ALPHABET`, so never empty in practice;
    /// `add_id` refuses an empty one.
    pub fn id(&self) -> &str {
        std::str::from_utf8(&self.id).unwrap_or("")
    }
}

/// The characters of a new block id (decision 0075).
const ID_ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// Its length, and how many taken ids are drawn before giving up.
pub const ID_LEN: usize = 6;

const ID_TRIES: usize = 8;

/// Random bytes at and above this are dropped, so each of the 36
/// characters is equally likely (252 = 7 × 36).
const ID_BYTE_LIMIT: u8 = 252;

/// Refills of the random buffer for one id before giving up: a source
/// that only gave dropped bytes would loop forever.
const ID_REFILLS: usize = 64;

/// Where new ids' randomness comes from: `crypto::fill_random`
/// (`getrandom`), replaced in tests.
pub type Random = fn(&mut [u8]) -> Result<(), Errno>;

/// A new block id: `ID_LEN` characters from `ID_ALPHABET`, drawn from
/// `random` by rejection sampling, and not `taken`. None after `ID_TRIES`
/// taken ids, when `random` fails, or after `ID_REFILLS` refills.
pub fn new_id(random: Random, taken: impl Fn(&str) -> bool) -> Option<[u8; ID_LEN]> {
    for _ in 0..ID_TRIES {
        let mut id = [0u8; ID_LEN];
        let mut filled = 0;
        for _ in 0..ID_REFILLS {
            if filled == ID_LEN {
                break;
            }
            let mut bytes = [0u8; 16];
            random(&mut bytes).ok()?;
            for byte in bytes.into_iter().filter(|&b| b < ID_BYTE_LIMIT) {
                if filled < ID_LEN {
                    id[filled] = ID_ALPHABET[usize::from(byte % 36)];
                    filled += 1;
                }
            }
        }
        if filled < ID_LEN {
            return None;
        }
        if !taken(std::str::from_utf8(&id).ok()?) {
            return Some(id);
        }
    }
    None
}

impl Editor {
    /// A block of the edited text without an id was chosen: a new id goes
    /// at the block's end and into the link typed at `typed`, in one undo
    /// step, the later position first. Nothing changes, with a notice,
    /// when no id can be made, when `add_id` refuses it (the note would
    /// parse otherwise), or when it does not fit.
    pub(super) fn link_new_id(&mut self, block: &Block, typed: Range<usize>) {
        let text = self.text();
        // Taken: any id the note index would find in the text.
        let taken = |id: &str| {
            text.lines()
                .any(|line| block_id(line).is_some_and(|(_, found)| found == id))
        };
        let Some(raw) = new_id(self.random, taken) else {
            self.notice = Some(NO_NEW_ID);
            return;
        };
        // ` ^` and the id: ASCII, so a char boundary everywhere.
        let mut suffix = [0u8; ID_LEN + 2];
        suffix[..2].copy_from_slice(b" ^");
        suffix[2..].copy_from_slice(&raw);
        let (Ok(id), Ok(suffix)) = (std::str::from_utf8(&raw), std::str::from_utf8(&suffix)) else {
            return;
        };
        // Room first: it is cheaper than the parses `add_id` makes.
        let close = !text
            .get(typed.end..)
            .is_some_and(|rest| rest.starts_with("]]"));
        let added = suffix.len() + id.len() + if close { 2 } else { 0 };
        if text.len() + added > MAX_BODY_BYTES + typed.len() {
            self.notice = Some(ID_FULL);
            return;
        }
        let (Some(end), Some(_)) = (block.end(), add_id(text, block, id)) else {
            self.notice = Some(ID_REFUSED);
            return;
        };
        self.buffer.commit();
        // The block ends after the link (the paragraph typed in) or
        // before it; the later edit goes first, so the earlier position
        // still holds.
        let after_link = typed.start + id.len() + 2;
        let edited = if end >= typed.end {
            self.buffer
                .replace(end..end, suffix)
                .and_then(|()| self.link_text_at(typed, id, close))
                .map(|()| after_link)
        } else {
            self.link_text_at(typed, id, close)
                .and_then(|()| self.buffer.replace(end..end, suffix))
                .map(|()| after_link + suffix.len())
        };
        self.buffer.commit();
        match edited {
            Ok(cursor) => {
                self.dirty = true;
                self.collapse(cursor);
            }
            // Positions come from the parse and the query, so this does
            // not happen; if it did, the half-made step is undone.
            Err(_) => {
                self.buffer.undo();
                self.notice = Some(ID_REFUSED);
            }
        }
    }

    /// A block of another note without an id was chosen: a new id, not
    /// taken in that note, is kept for the session as a `BlockRequest`.
    /// Nothing is kept, with a notice, when the block takes no id, none
    /// can be made, or the note or the link would not fit.
    pub(super) fn request_new_id(
        &mut self,
        note: usize,
        block: &Block,
        typed: Range<usize>,
        notes: &Notes,
    ) {
        if note >= notes.len() {
            return;
        }
        if block.end().is_none() {
            self.notice = Some(ID_REFUSED);
            return;
        }
        let body = notes.note(note).body.as_str();
        let taken = |id: &str| {
            body.lines()
                .any(|line| block_id(line).is_some_and(|(_, found)| found == id))
        };
        let Some(id) = new_id(self.random, taken) else {
            self.notice = Some(NO_NEW_ID);
            return;
        };
        let text = self.text();
        let close = !text
            .get(typed.end..)
            .is_some_and(|rest| rest.starts_with("]]"));
        let link_added = ID_LEN + if close { 2 } else { 0 };
        if body.len() + ID_LEN + 2 > MAX_BODY_BYTES
            || text.len() + link_added > MAX_BODY_BYTES + typed.len()
        {
            self.notice = Some(ID_FULL);
            return;
        }
        self.block_request = Some(BlockRequest {
            note,
            block: block.text.clone(),
            id,
            typed,
        });
    }

    /// After the session saved `request`'s id into its note: the link is
    /// written with it, in one undo step, the cursor after its `]]`.
    /// False, nothing written, when the cursor is no longer in that link
    /// as it was.
    pub fn link_block(&mut self, request: &BlockRequest) -> bool {
        let typed = request.typed.clone();
        let same = target(self.text(), self.cursor)
            .and_then(|q| q.fragment)
            .is_some_and(|fragment| fragment == (Part::Block, typed.clone()));
        let id = request.id();
        if !same || id.is_empty() {
            return false;
        }
        let close = !self
            .text()
            .get(typed.end..)
            .is_some_and(|rest| rest.starts_with("]]"));
        self.buffer.commit();
        let written = self.link_text_at(typed.clone(), id, close);
        self.buffer.commit();
        match written {
            Ok(()) => {
                self.dirty = true;
                self.collapse(typed.start + id.len() + 2);
                true
            }
            // The buffer refused (the size limit): a half-written link
            // is undone.
            Err(_) => {
                self.buffer.undo();
                false
            }
        }
    }

    /// `typed` becomes `id`, with `]]` after it when `close`.
    fn link_text_at(
        &mut self,
        typed: Range<usize>,
        id: &str,
        close: bool,
    ) -> Result<(), EditError> {
        let at = typed.start + id.len();
        self.buffer.replace(typed, id)?;
        if close {
            self.buffer.replace(at..at, "]]")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{at_end, chars, rows_of, send, with_blocks};
    use super::*;
    use std::time::Instant;

    use crate::editor::EditorAction;
    use crate::md::blocks::blocks;
    use crate::notes::tests::{note, notes};
    use crate::shell::keys::{Key, MouseEvent};
    use crate::ui::clipboard::Clipboard;
    use proptest::prelude::*;

    #[test]
    fn a_block_without_id_in_another_note_is_asked_of_the_session() {
        let n = with_blocks();
        let (mut ed, mut clip) = with_ids("");
        send(&mut ed, &mut clip, &n, chars("[[plan#^no"));
        assert_eq!(rows_of(&ed, &n), ["No id here"]);
        // Enter asks; nothing is written before the session saved.
        assert_eq!(
            ed.key_in(Key::Enter, &mut clip, &n),
            EditorAction::LinkBlock
        );
        assert_eq!(ed.text(), "[[plan#^no]]");
        assert!(ed.suggestions().is_none());
        assert_eq!(ed.notice(), None);
        let request = ed.take_block_request().unwrap();
        assert!(ed.take_block_request().is_none(), "taken once");
        let body = n.note(0).body.as_str();
        assert_eq!(
            (request.note, &body[request.block.clone()]),
            (0, "No id here")
        );
        assert_eq!(request.id(), "abcdef");
        // Saved there: the link, one undo step, the cursor after `]]`.
        assert!(ed.link_block(&request));
        assert_eq!(ed.text(), "[[plan#^abcdef]]");
        assert_eq!(ed.cursor, ed.text().len());
        assert!(ed.is_dirty());
        ed.key_in(Key::Ctrl('z'), &mut clip, &n);
        assert_eq!(ed.text(), "[[plan#^no]]");
        // Not written when the cursor left the link, or the link changed.
        send(&mut ed, &mut clip, &n, [Key::Left]);
        assert!(!ed.link_block(&request));
        send(
            &mut ed,
            &mut clip,
            &n,
            [Key::End, Key::Left, Key::Left, Key::Backspace],
        );
        assert!(!ed.link_block(&request));
        assert_eq!(ed.text(), "[[plan#^n]]");
        // Any later key drops a request not taken; a click asks too.
        let (mut ed, mut clip) = with_ids("");
        send(&mut ed, &mut clip, &n, chars("[[plan#^no"));
        send(&mut ed, &mut clip, &n, [Key::Enter, Key::Left]);
        assert!(ed.take_block_request().is_none());
        let (mut ed, mut clip) = with_ids("");
        send(&mut ed, &mut clip, &n, chars("[[plan#^no"));
        let action = ed.mouse_in(MouseEvent::Press, None, Some(0), Instant::now(), &n);
        assert_eq!(action, EditorAction::LinkBlock);
        assert!(ed.take_block_request().is_some());
        // An id taken in that note is drawn again; none, a notice.
        let taken = notes(vec![note(1, "", "plan", "x ^abcdef\n\nNo id here\n")]);
        let (mut ed, mut clip) = with_ids("");
        ed.random = counting;
        NEXT.set(0);
        send(
            &mut ed,
            &mut clip,
            &taken,
            chars("[[plan#^no").into_iter().chain([Key::Enter]),
        );
        assert_ne!(ed.take_block_request().unwrap().id(), "abcdef");
        let (mut ed, mut clip) = with_ids("");
        ed.random = failing;
        send(
            &mut ed,
            &mut clip,
            &n,
            chars("[[plan#^no").into_iter().chain([Key::Enter]),
        );
        assert!(ed.take_block_request().is_none());
        assert_eq!(ed.notice(), Some(NO_NEW_ID));
        // That note full: a notice, no request.
        let full = format!("No id here\n{}", "x".repeat(MAX_BODY_BYTES - 11 - 7));
        let full = notes(vec![note(1, "", "plan", &full)]);
        let (mut ed, mut clip) = with_ids("");
        send(
            &mut ed,
            &mut clip,
            &full,
            chars("[[plan#^no").into_iter().chain([Key::Enter]),
        );
        assert!(ed.take_block_request().is_none());
        assert_eq!(ed.notice(), Some(ID_FULL));
        // No blocks in a text note, or for a missing note.
        for typed in ["[[todo.txt#^", "[[nope#^"] {
            let (mut ed, mut clip) = at_end("");
            send(&mut ed, &mut clip, &n, chars(typed));
            assert!(ed.suggestions().is_none(), "{typed}");
        }
    }

    /// Random sources for new ids: bytes 0, 1, 2, … (`abcdef`).
    fn fixed(buf: &mut [u8]) -> Result<(), Errno> {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = u8::try_from(i).unwrap();
        }
        Ok(())
    }

    thread_local! {
        static NEXT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    }

    /// Counts on across calls in this thread: a new id each time.
    fn counting(buf: &mut [u8]) -> Result<(), Errno> {
        for b in buf {
            *b = NEXT.get();
            NEXT.set(NEXT.get().wrapping_add(1));
        }
        Ok(())
    }

    /// Only bytes that rejection sampling drops.
    fn high(buf: &mut [u8]) -> Result<(), Errno> {
        buf.fill(255);
        Ok(())
    }

    fn failing(_: &mut [u8]) -> Result<(), Errno> {
        Err(Errno::NOSYS)
    }

    #[test]
    fn new_ids_are_six_unbiased_characters_not_taken() {
        let text = |id: Option<[u8; ID_LEN]>| id.map(|id| String::from_utf8(id.to_vec()).unwrap());
        assert_eq!(text(new_id(fixed, |_| false)), Some("abcdef".into()));
        // 252 and up are dropped; 36 is `a` again, 71 and 251 are `9`.
        fn skips(buf: &mut [u8]) -> Result<(), Errno> {
            buf[..8].copy_from_slice(&[252, 255, 36, 37, 71, 251, 0, 9]);
            Ok(())
        }
        assert_eq!(text(new_id(skips, |_| false)), Some("ab99aj".into()));
        // Taken ids are drawn again, at most 8 times.
        NEXT.set(0);
        assert_eq!(
            text(new_id(counting, |id| id == "abcdef")),
            Some("qrstuv".into())
        );
        assert_eq!(new_id(fixed, |_| true), None);
        // A failing source, or one that never gives a usable byte.
        assert_eq!(new_id(failing, |_| false), None);
        assert_eq!(new_id(high, |_| false), None);
    }

    /// An editor on `text`, the cursor at its end, ids from `fixed`.
    fn with_ids(text: &str) -> (Editor, Clipboard) {
        let (mut ed, clip) = at_end(text);
        ed.random = fixed;
        (ed, clip)
    }

    #[test]
    fn a_new_id_goes_into_the_edited_text_with_the_link() {
        let n = with_blocks();
        // A block before the link.
        let (mut ed, mut clip) = with_ids("Para one\n\nPara two ^p\n\nsee ");
        send(&mut ed, &mut clip, &n, chars("[[#^one"));
        // The paragraph typed in holds `one` too.
        assert_eq!(rows_of(&ed, &n), ["Para one", "see [[#^one]]"]);
        send(&mut ed, &mut clip, &n, [Key::Enter]);
        assert_eq!(
            ed.text(),
            "Para one ^abcdef\n\nPara two ^p\n\nsee [[#^abcdef]]"
        );
        assert_eq!(ed.cursor(), ed.text().len());
        assert!(
            notes(vec![note(1, "", "t", ed.text())])
                .resolve(0, "#^abcdef")
                .is_ok()
        );
        // One undo step, and redo.
        ed.key_in(Key::Ctrl('z'), &mut clip, &n);
        assert_eq!(ed.text(), "Para one\n\nPara two ^p\n\nsee [[#^one]]");
        ed.key_in(Key::Ctrl('y'), &mut clip, &n);
        assert!(ed.text().starts_with("Para one ^abcdef"));
        // A block after the link.
        let mut ed = Editor::new("\n\nLater para\n").unwrap();
        ed.random = fixed;
        send(&mut ed, &mut clip, &n, chars("see [[#^lat"));
        send(&mut ed, &mut clip, &n, [Key::Enter]);
        assert_eq!(ed.text(), "see [[#^abcdef]]\n\nLater para ^abcdef\n");
        assert_eq!(ed.cursor(), "see [[#^abcdef]]".len());
        // The paragraph the link is typed in.
        let (mut ed, mut clip) = with_ids("see ");
        send(&mut ed, &mut clip, &n, chars("[[#^see"));
        send(&mut ed, &mut clip, &n, [Key::Enter]);
        assert_eq!(ed.text(), "see [[#^abcdef]] ^abcdef");
        assert_eq!(ed.cursor(), "see [[#^abcdef]]".len());
    }

    #[test]
    fn no_new_id_when_refused_taken_or_full() {
        let n = with_blocks();
        // `1.` would start a list: `add_id` refuses.
        let (mut ed, mut clip) = with_ids("a\n1.\n\nsee ");
        send(&mut ed, &mut clip, &n, chars("[[#^1"));
        assert_eq!(rows_of(&ed, &n), ["a", "see [[#^1]]"]);
        send(&mut ed, &mut clip, &n, [Key::Enter]);
        assert_eq!(ed.text(), "a\n1.\n\nsee [[#^1]]");
        assert_eq!(ed.notice(), Some(ID_REFUSED));
        // Every id drawn is taken (any line's, even in code).
        let (mut ed, mut clip) = with_ids("Para\n\n```\nx ^abcdef\n```\n\n");
        send(&mut ed, &mut clip, &n, chars("[[#^para"));
        send(&mut ed, &mut clip, &n, [Key::Enter]);
        assert_eq!(ed.notice(), Some(NO_NEW_ID));
        assert!(ed.text().starts_with("Para\n"));
        // No randomness.
        let (mut ed, mut clip) = at_end("Para\n\n");
        ed.random = failing;
        send(&mut ed, &mut clip, &n, chars("[[#^"));
        send(&mut ed, &mut clip, &n, [Key::Enter]);
        assert_eq!(ed.notice(), Some(NO_NEW_ID));
        assert_eq!(ed.text(), "Para\n\n[[#^]]");
        // No room: 8 bytes left (`[[#^]]` took 6), 14 needed.
        let big = format!("{}\n\n", "b".repeat(MAX_BODY_BYTES - 16));
        let (mut ed, mut clip) = with_ids(&big);
        send(&mut ed, &mut clip, &n, chars("[[#^"));
        assert_eq!(MAX_BODY_BYTES - ed.text().len(), 8);
        send(&mut ed, &mut clip, &n, [Key::Enter]);
        assert_eq!(ed.notice(), Some(ID_FULL));
        assert!(ed.text().ends_with("\n\n[[#^]]"));
    }

    proptest! {
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
            NEXT.set(0);
            ed.random = counting;
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
            if !has_id && !own {
                // Another note: asked of the session, nothing written
                // yet; once it saved the id there, the link resolves.
                prop_assert_eq!(&text, &before);
                let Some(request) = ed.take_block_request() else {
                    prop_assert_eq!(ed.notice(), Some(ID_REFUSED));
                    return Ok(());
                };
                let block = blocks(&body).into_iter().find(|b| b.text == request.block).unwrap();
                let Some(saved) = add_id(&body, &block, request.id()) else {
                    // The session refuses it too: no link.
                    return Ok(());
                };
                prop_assert!(ed.link_block(&request));
                let text = ed.text();
                let link = &text[text.rfind("[[").unwrap() + 2..text.len() - 2];
                let n = notes(vec![note(1, "", "t", &saved)]);
                let resolved = n.resolve(0, link).map(|(to, f)| (to, f.is_some()));
                prop_assert_eq!(resolved, Ok((0, true)), "{:?}", link);
                return Ok(());
            }
            if !has_id && ed.notice() == Some(ID_REFUSED) {
                prop_assert_eq!(&text, &before);
                return Ok(());
            }
            if !has_id {
                // A new id: ` ^` and 6 of `a-z0-9` at the block's end, the
                // same 6 in the link (the `]]` was typed already); one
                // undo step takes both back.
                prop_assert_eq!(text.len(), before.len() + 14);
                let link_id = &text[text.rfind("[[#^").unwrap() + 4..text.rfind("]]").unwrap()];
                prop_assert_eq!(link_id.len(), ID_LEN);
                prop_assert!(link_id.bytes().all(|b| ID_ALPHABET.contains(&b)));
                let suffix = format!(" ^{link_id}");
                prop_assert_eq!(text.matches(&suffix).count(), 1);
                ed.key_in(Key::Ctrl('z'), &mut clip, &n);
                prop_assert_eq!(ed.text(), before.as_str());
            }
            // The link up to its own `]]`: a new id can follow it.
            let start = text.rfind("[[").unwrap() + 2;
            let link = &text[start..start + text[start..].find("]]").unwrap()];
            let resolved = if own {
                notes(vec![note(1, "", "t", &text)]).resolve(0, link).map(|(to, f)| (to, f.is_some()))
            } else {
                n.resolve(0, link).map(|(to, f)| (to, f.is_some()))
            };
            prop_assert_eq!(resolved, Ok((0, true)), "{:?}", link);
        }
    }
}
