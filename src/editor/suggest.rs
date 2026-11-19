//! Link suggestions in the editor (roadmap W1 to W6): the name typed
//! after an open `[[`, the notes it matches, and the text that links to
//! a chosen note. Pure functions over the text and the session's
//! `Notes`; the editor keeps the list's state. They return ranges and
//! note indexes, never copies of names.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the editor from roadmap step W2")
)]

use std::ops::Range;

use crate::notes::{Found, Notes, cmp_ci};

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
