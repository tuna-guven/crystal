//! The blocks of a Markdown note that a link can point into with `#^id`
//! (decision 0075): its paragraphs and the text of its list items, found
//! with the parser and options of the note index (`notes::scan`). Code,
//! tables, HTML, headings and rules are not blocks, and a list item's
//! block is its own text, not the lists nested in it.
//!
//! A block's id is read from its last line by `block_id`, as the index
//! reads ids, and a new one goes at the block's end, which is the end of
//! that line before any trailing space: there the index finds it. Lines
//! are the index's: they end only at `\n`, while the parser also ends one
//! at a lone `\r`. So a block whose index line goes on after it with
//! anything but whitespace has no id and takes none (found by the
//! property test: `a\r\r*`).
//!
//! Text added at a line's end can change how the note parses: after
//! `a\n1.` (an empty item cannot interrupt a paragraph, so `1.` is the
//! paragraph's) the line `1. ^id` starts a list. `add_id` therefore
//! parses the result again and gives it only when the same blocks come
//! back with the id on the chosen one (found by the property test too).
//! Everything is a range into the note; nothing is copied.

#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "used by the link suggestions from roadmap step J4"
    )
)]

use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use zeroize::Zeroizing;

use super::render::block_id;

/// The note index's parser options (`notes::scan`).
const OPTIONS: Options = Options::ENABLE_WIKILINKS
    .union(Options::ENABLE_TABLES)
    .union(Options::ENABLE_TASKLISTS);

/// A block of a note. No Debug: it points into note text.
pub struct Block {
    /// From its first character to its end: the end of its last line,
    /// trailing whitespace left out. Never empty.
    pub text: Range<usize>,
    /// Its `^id` (without ` ^`), at the end of `text`, if it has one.
    pub id: Option<Range<usize>>,
    /// Only whitespace follows `text` on its line, as the index reads
    /// lines: an id there is the block's.
    ends_line: bool,
}

impl Block {
    /// Where a new ` ^id` goes, the end of the block's text; None when
    /// the block has an id already or cannot take one.
    pub fn end(&self) -> Option<usize> {
        (self.ends_line && self.id.is_none()).then_some(self.text.end)
    }
}

/// `body` with ` ^id` at the end of `block`, one of `blocks(body)`; None
/// when the block takes no id (`Block::end`), `id` is not a valid one, or
/// the new text would parse otherwise: other blocks, other starts, or the
/// id not the block's. The text is built in zeroizing memory at its full
/// size, so it never reallocates.
pub fn add_id(body: &str, block: &Block, id: &str) -> Option<Zeroizing<String>> {
    let end = block.end()?;
    let valid = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !valid || body.get(..end).is_none() {
        return None;
    }
    let mut text = Zeroizing::new(String::with_capacity(body.len() + id.len() + 2));
    for part in [&body[..end], " ^", id, &body[end..]] {
        text.push_str(part);
    }
    let shift = id.len() + 2;
    let before = blocks(body);
    let after = blocks(&text);
    let same = before.len() == after.len()
        && before.iter().zip(&after).all(|(old, new)| {
            let moved = |at: usize| if at > end { at + shift } else { at };
            let new_id = new.id.clone().map(|r| &text[r]);
            new.text.start == moved(old.text.start)
                && if old.text == block.text {
                    new_id == Some(id)
                } else {
                    new_id == old.id.clone().map(|r| &body[r])
                }
        });
    same.then_some(text)
}

/// A container block the parser has open: a list item keeps the span of
/// its own inline text seen so far.
enum Open {
    Item(Option<Range<usize>>),
    Other,
}

/// The blocks of `body`, a Markdown note, in the order they start.
pub fn blocks(body: &str) -> Vec<Block> {
    let mut found = Vec::new();
    let mut open: Vec<Open> = Vec::new();
    for (event, range) in Parser::new_ext(body, OPTIONS).into_offset_iter() {
        match event {
            Event::Start(tag) if !is_inline(&tag) => {
                if matches!(tag, Tag::Paragraph) {
                    found.extend(block(body, range));
                }
                open.push(if matches!(tag, Tag::Item) {
                    Open::Item(None)
                } else {
                    Open::Other
                });
            }
            Event::End(tag) if !is_inline_end(tag) => {
                if let Some(Open::Item(Some(own))) = open.pop() {
                    found.extend(block(body, own));
                }
            }
            // Leaf blocks, never inline text.
            Event::Rule | Event::Html(_) => {}
            // Inline: the item's own text when the item is the innermost
            // open block (in a paragraph, heading or code it is not).
            _ => {
                if let Some(Open::Item(own)) = open.last_mut() {
                    *own = Some(match own.take() {
                        Some(r) => r.start.min(range.start)..r.end.max(range.end),
                        None => range,
                    });
                }
            }
        }
    }
    // A list item ends after the blocks nested in it.
    found.sort_by_key(|b: &Block| b.text.start);
    found
}

/// The block over `range`: trimmed at its end, with the id its last line
/// carries. None if nothing but whitespace is left.
fn block(body: &str, range: Range<usize>) -> Option<Block> {
    let text = body.get(range.clone())?.trim_end();
    if text.is_empty() {
        return None;
    }
    let end = range.start + text.len();
    let line = body[..end].rfind('\n').map_or(0, |at| at + 1);
    let line_end = body[end..].find('\n').map_or(body.len(), |at| end + at);
    let ends_line = body[end..line_end].trim().is_empty();
    let id = block_id(&body[line..end])
        .filter(|_| ends_line)
        .map(|(_, id)| end - id.len()..end);
    Some(Block {
        text: range.start..end,
        id,
        ends_line,
    })
}

/// Tags that live inside a block's text.
fn is_inline(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Emphasis
            | Tag::Strong
            | Tag::Strikethrough
            | Tag::Superscript
            | Tag::Subscript
            | Tag::Link { .. }
            | Tag::Image { .. }
    )
}

fn is_inline_end(tag: TagEnd) -> bool {
    matches!(
        tag,
        TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::Link
            | TagEnd::Image
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::tests::{note, notes};
    use proptest::prelude::*;

    /// Each block as (its text, its id).
    fn shown(body: &str) -> Vec<(&str, Option<&str>)> {
        blocks(body)
            .into_iter()
            .map(|b| (&body[b.text], b.id.map(|id| &body[id])))
            .collect()
    }

    #[test]
    fn paragraphs_and_list_items_are_blocks() {
        let body = "# Title\n\nFirst line\nsecond line  \n\n- one\n- two ^b-2\n  - nested\n\n> quoted ^q\n";
        assert_eq!(
            shown(body),
            [
                ("First line\nsecond line", None),
                ("one", None),
                ("two ^b-2", Some("b-2")),
                ("nested", None),
                ("quoted ^q", Some("q")),
            ]
        );
    }

    #[test]
    fn loose_items_are_their_paragraphs() {
        let body = "1. a\n\n   more a\n2. b\n";
        assert_eq!(shown(body), [("a", None), ("more a", None), ("b", None)]);
    }

    #[test]
    fn code_tables_html_headings_and_rules_are_not_blocks() {
        let body = "```\ncode ^c\n```\n\n    indented\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n<div>\nhtml\n</div>\n\n## h ^h\n\n---\n\n- ```\n  in item\n  ```\n- # heading\n";
        assert!(shown(body).is_empty(), "{:?}", shown(body));
        // Text after a code fence is a block again.
        assert_eq!(shown("```\nx\n```\nafter\n"), [("after", None)]);
    }

    #[test]
    fn ids_and_ends() {
        // `\r\n`, trailing spaces, an id that is not one (`^` alone, no
        // space before it), a task item.
        let body = "a ^x1\r\n\r\nb   \r\n\r\nc^x\n\nd ^\n\n- [ ] task ^t\n";
        assert_eq!(
            shown(body),
            [
                ("a ^x1", Some("x1")),
                ("b", None),
                ("c^x", None),
                ("d ^", None),
                ("[ ] task ^t", Some("t")),
            ]
        );
        let b = &blocks("b   \n")[0];
        assert_eq!(b.end(), Some(1));
        // An id already there: no new one.
        assert_eq!(blocks("a ^x\n")[0].end(), None);
    }

    #[test]
    fn a_lone_carriage_return_does_not_end_the_index_line() {
        // The parser sees `a` and `*`; the index sees one line `a\r\r*`,
        // so `a` can take no id, and an id after `*` is not `a`'s.
        let body = "a\r\r* ^z\n";
        let all = blocks(body);
        assert_eq!(body[all[0].text.clone()].to_owned(), "a");
        assert_eq!(all[0].end(), None);
        assert_eq!(all[0].id, None);
        // A `\r\n` line end is fine.
        assert_eq!(blocks("a\r\n\r\nb")[0].end(), Some(1));
    }

    #[test]
    fn plain_and_empty_text() {
        assert!(blocks("").is_empty());
        assert!(blocks("\n\n  \n").is_empty());
        assert_eq!(shown("Ünï 日本 😀"), [("Ünï 日本 😀", None)]);
    }

    #[test]
    fn add_id_puts_the_id_at_the_end_or_refuses() {
        let body = "First\nlines  \n\n- item\n";
        let all = blocks(body);
        let text = add_id(body, &all[0], "ab-1").unwrap();
        assert_eq!(text.as_str(), "First\nlines ^ab-1  \n\n- item\n");
        let text = add_id(body, &all[1], "x").unwrap();
        assert_eq!(text.as_str(), "First\nlines  \n\n- item ^x\n");
        // Not an id, or an id already there.
        assert!(add_id(body, &all[1], "a b").is_none());
        assert!(add_id(body, &all[1], "").is_none());
        assert!(add_id("a ^x", &blocks("a ^x")[0], "y").is_none());
        // `1.` would start a list once it holds text.
        let body = "a\n1.";
        assert!(add_id(body, &blocks(body)[0], "x").is_none());
    }

    /// Markdown-ish notes: lines of a few block prefixes and inline text.
    fn markdown() -> impl Strategy<Value = String> {
        let prefix = prop::sample::select(vec![
            "",
            "",
            "",
            "- ",
            "  - ",
            "1. ",
            "> ",
            "# ",
            "```",
            "    ",
            "| a | b |",
            "|---|---|",
            "---",
            "<div>",
            "* [ ] ",
        ]);
        let text = "[a-c ^\\-*`|\\[\\]é日\t\r]{0,12}";
        prop::collection::vec((prefix, text), 0..14).prop_map(|lines| {
            lines
                .into_iter()
                .map(|(p, t)| format!("{p}{t}"))
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    /// An id the generator's text never holds.
    const NEW_ID: &str = "qq1z9x";

    proptest! {
        /// Every block is a non-empty char-boundary range without
        /// trailing whitespace, its id at its end.
        #[test]
        fn blocks_are_valid_ranges(body in markdown()) {
            let mut last = 0;
            for b in blocks(&body) {
                let text = body.get(b.text.clone());
                prop_assert!(text.is_some());
                let text = text.unwrap();
                prop_assert!(!text.is_empty());
                prop_assert_eq!(text, text.trim_end());
                prop_assert!(b.text.start >= last);
                last = b.text.start;
                if let Some(id) = b.id {
                    prop_assert_eq!(id.end, b.text.end);
                    prop_assert!(body[id].chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
                }
            }
        }

        /// `add_id` either refuses or gives a text where the note index
        /// finds the new id and every id it found before, and `blocks` give
        /// the chosen block the id and leave the others as they were.
        #[test]
        fn a_new_id_at_the_end_is_found(body in markdown(), pick in any::<prop::sample::Index>()) {
            let before = blocks(&body);
            let without: Vec<&Block> = before.iter().filter(|b| b.end().is_some()).collect();
            if without.is_empty() {
                return Ok(());
            }
            let chosen = without[pick.index(without.len())];
            let end = chosen.end().unwrap();
            let Some(changed) = add_id(&body, chosen, NEW_ID) else {
                return Ok(());
            };
            prop_assert_eq!(
                changed.as_str(),
                format!("{}{}{}", &body[..end], format_args!(" ^{NEW_ID}"), &body[end..])
            );
            let target = format!("#^{NEW_ID}");
            let old = notes(vec![note(1, "", "t", &body)]);
            let new = notes(vec![note(1, "", "t", &changed)]);
            prop_assert!(old.resolve(0, &target).is_err());
            prop_assert!(new.resolve(0, &target).is_ok(), "{:?}", changed.as_str());
            for b in &before {
                if let Some(id) = &b.id {
                    let link = format!("#^{}", &body[id.clone()]);
                    prop_assert!(new.resolve(0, &link).is_ok());
                }
            }
            let after = blocks(&changed);
            prop_assert_eq!(after.len(), before.len());
            for (old_b, new_b) in before.iter().zip(&after) {
                if std::ptr::eq(old_b, chosen) {
                    prop_assert_eq!(new_b.id.clone().map(|id| &changed[id]), Some(NEW_ID));
                }
            }
        }

        /// On plain paragraphs `add_id` never refuses: the check only
        /// catches text that parses otherwise.
        #[test]
        fn plain_paragraphs_always_take_an_id(
            paragraphs in prop::collection::vec("[a-z][a-z ]{0,20}", 1..6),
            pick in any::<prop::sample::Index>(),
        ) {
            let body = paragraphs.join("\n\n");
            let all = blocks(&body);
            prop_assert_eq!(all.len(), paragraphs.len());
            let chosen = &all[pick.index(all.len())];
            prop_assert!(add_id(&body, chosen, NEW_ID).is_some());
        }
    }
}
