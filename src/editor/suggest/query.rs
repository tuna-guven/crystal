//! What a link being typed asks for, and what matches it (its own file
//! since step RF7): the link the cursor is typing after `[[` (`target`),
//! the notes, headings and blocks it matches, and the text a link to a
//! note is written with (`link_text`).
//!
//! Everything here is pure: text and an index in, ranges and indexes
//! out, never copies of note text, so it is tested on its own and the
//! list (`suggest.rs`) only decides when to ask. Matching ignores case
//! without lowercase copies (`starts_ci`, `contains_ci`), and keeps at
//! most `MAX_MATCHES` entries, so a key costs the same in any vault.

use std::ops::Range;

use zeroize::Zeroizing;

use crate::md::blocks::Block;
use crate::notes::{Found, Notes, cmp_ci};

/// The most notes the list holds: the best matches; typing more finds
/// the others.
pub const MAX_MATCHES: usize = 100;

/// Characters a heading must not hold to be linked as written: they end
/// or split a link (decision 0075).
const LINK_CHARS: &[char] = &['[', ']', '|', '#', '^'];

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
/// `cursor` on its line (ended by `\n` or `\r`) (`![[` counts too) up to `cursor`. A name is
/// followed by at most one `#`: `name#text` is a heading, `name#^text` a
/// block, with an empty name for the note being edited. None when the
/// cursor is not inside such a link, or not on a char boundary: `[`, `]`
/// or `|` anywhere, a second `#`, or a `^` anywhere else.
pub fn target(text: &str, cursor: usize) -> Option<Query> {
    let before = text.get(..cursor)?;
    // Lines end where the parser ends them, a lone `\r` too: no link
    // spans one, and the kept blocks (`load_blocks`) take the link and
    // the cursor to be on one parser line (found by the long tests).
    let line = before.rfind(['\n', '\r']).map_or(0, |at| at + 1);
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
    use super::super::tests::sample;
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
        // A lone `\r` ends a line too, as the parser reads it.
        assert_eq!(q("[[a\rb", 5), None);
        assert_eq!(t("[[#^a\rb"), None);
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
