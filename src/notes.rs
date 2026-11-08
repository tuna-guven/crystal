//! An unlocked vault's notes and the indexes built from them
//! (docs/decisions/0034-vault-format-2.md): built at unlock, never stored,
//! all in zeroizing memory.
//!
//! - names: each note's full name (`folder/title`), and a case-insensitive
//!   lookup; a bare title resolves when exactly one note has it;
//! - tags: Obsidian `#tag` in Markdown text outside code, case-insensitive;
//! - links: each Markdown note's `[[link]]` targets (followed in the pager).
//!
//! Plain text notes (`*.txt`, decision 0038) contribute names only.

use std::collections::HashMap;

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};
use zeroize::Zeroizing;

use crate::md::render::{Kind, kind_of};
use crate::model::Note;
use crate::model::name::{NameKey, unique_key};
use crate::vault::codec::VaultContents;

/// One tag: as first written, and the notes that have it.
pub struct TagEntry {
    pub shown: Zeroizing<String>,
    pub notes: Vec<usize>,
}

pub struct Notes {
    pub contents: VaultContents,
    /// `folder/title` per note, in note order.
    names: Vec<Zeroizing<String>>,
    by_name: HashMap<NameKey, usize>,
    /// Lowercased title to the notes that have it.
    by_title: HashMap<NameKey, Vec<usize>>,
    /// By lowercased tag; sorted by `tags()`.
    tags: HashMap<NameKey, TagEntry>,
    /// `[[link]]` targets per note.
    links: Vec<Vec<Zeroizing<String>>>,
}

/// How a name given by the user resolved.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "view and links in the pager (L6b)")
)]
pub enum Found {
    One(usize),
    None,
    /// A bare title that more than one note has.
    Ambiguous(Vec<usize>),
}

impl Notes {
    pub fn new(contents: VaultContents) -> Self {
        let count = contents.notes.len();
        let mut notes = Self {
            names: Vec::with_capacity(count),
            by_name: HashMap::with_capacity(count),
            by_title: HashMap::with_capacity(count),
            tags: HashMap::new(),
            links: Vec::with_capacity(count),
            contents,
        };
        for index in 0..count {
            let note = &notes.contents.notes[index];
            notes.names.push(full_name(note));
            notes
                .by_name
                .insert(unique_key(note.folder.as_str(), note.title.as_str()), index);
            notes
                .by_title
                .entry(unique_key("", note.title.as_str()))
                .or_default()
                .push(index);
            let (tags, links) = match kind_of(note.title.as_str()) {
                Kind::Markdown => scan(note.body.as_str()),
                Kind::Plain => (Vec::new(), Vec::new()),
            };
            for tag in tags {
                let entry = notes
                    .tags
                    .entry(unique_key("", &tag))
                    .or_insert_with(|| TagEntry {
                        shown: tag,
                        notes: Vec::new(),
                    });
                if entry.notes.last() != Some(&index) {
                    entry.notes.push(index);
                }
            }
            notes.links.push(links);
        }
        notes
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// `folder/title`, or `title` at the top level.
    pub fn name(&self, index: usize) -> &str {
        &self.names[index]
    }

    pub fn note(&self, index: usize) -> &Note {
        &self.contents.notes[index]
    }

    /// A full name (any case), else a bare title that one note has.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "view and links in the pager (L6b)")
    )]
    pub fn find(&self, name: &str) -> Found {
        let (folder, title) = name.rsplit_once('/').unwrap_or(("", name));
        if let Some(&index) = self.by_name.get(&unique_key(folder, title)) {
            return Found::One(index);
        }
        if name.contains('/') {
            return Found::None;
        }
        match self.by_title.get(&unique_key("", name)) {
            None => Found::None,
            Some(list) if list.len() == 1 => Found::One(list[0]),
            Some(list) => Found::Ambiguous(list.clone()),
        }
    }

    /// Every tag, sorted case-insensitively.
    pub fn tags(&self) -> Vec<&TagEntry> {
        let mut tags: Vec<&TagEntry> = self.tags.values().collect();
        tags.sort_by(|a, b| cmp_ci(&a.shown, &b.shown));
        tags
    }

    /// The notes with `tag` (with or without `#`, any case).
    pub fn tagged(&self, tag: &str) -> &[usize] {
        let tag = tag.strip_prefix('#').unwrap_or(tag);
        self.tags
            .get(&unique_key("", tag))
            .map_or(&[], |entry| entry.notes.as_slice())
    }

    /// `[[link]]` targets of a note, as written.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "links and backlinks in the pager (L6b)")
    )]
    pub fn links(&self, index: usize) -> &[Zeroizing<String>] {
        &self.links[index]
    }
}

/// `folder/title`, built once at its final size.
fn full_name(note: &Note) -> Zeroizing<String> {
    let (folder, title) = (note.folder.as_str(), note.title.as_str());
    let mut name = Zeroizing::new(String::with_capacity(folder.len() + 1 + title.len()));
    if !folder.is_empty() {
        name.push_str(folder);
        name.push('/');
    }
    name.push_str(title);
    name
}

/// Case-insensitive ordering without making lowercase copies; text equal
/// ignoring case is then ordered by its original form, so the order is
/// stable.
pub fn cmp_ci(a: &str, b: &str) -> std::cmp::Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
        .then_with(|| a.cmp(b))
}

/// Obsidian tags and wikilink targets in Markdown, outside code.
fn scan(body: &str) -> (Vec<Zeroizing<String>>, Vec<Zeroizing<String>>) {
    let mut tags = Vec::new();
    let mut links = Vec::new();
    let mut in_code = false;
    let options = Options::ENABLE_WIKILINKS | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(body, options) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => in_code = true,
            Event::End(TagEnd::CodeBlock) => in_code = false,
            Event::Text(text) if !in_code => tags_in(&text, &mut tags),
            Event::Start(Tag::Link {
                link_type: LinkType::WikiLink { .. },
                dest_url,
                ..
            }) => links.push(Zeroizing::new(dest_url.to_string())),
            _ => {}
        }
    }
    (tags, links)
}

/// `#tag` at the start or after whitespace: letters, digits, `_`, `-`,
/// `/`, and not only digits (`#1` is not a tag).
fn tags_in(text: &str, out: &mut Vec<Zeroizing<String>>) {
    let mut previous = ' ';
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c == '#' && previous.is_whitespace() {
            let start = at + 1;
            let mut end = start;
            while let Some(&(i, n)) = chars.peek() {
                if n.is_alphanumeric() || matches!(n, '_' | '-' | '/') {
                    end = i + n.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            let tag = text[start..end].trim_end_matches('/');
            if !tag.is_empty() && !tag.chars().all(|c| c.is_ascii_digit()) {
                out.push(Zeroizing::new(tag.to_owned()));
            }
            previous = ' ';
            continue;
        }
        previous = c;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::{NoteId, NoteText, VaultId};

    pub fn note(id: u8, folder: &str, title: &str, body: &str) -> Note {
        let text = |s: &str| NoteText::from_zeroizing(Zeroizing::new(s.to_owned()));
        Note {
            id: NoteId::from_bytes([id; 16]),
            folder: text(folder),
            title: text(title),
            created: 0,
            modified: 0,
            body: text(body),
        }
    }

    pub fn notes(list: Vec<Note>) -> Notes {
        Notes::new(VaultContents {
            vault_id: VaultId::from_bytes([1; 32]),
            generation: 1,
            notes: list,
        })
    }

    pub fn sample() -> Notes {
        notes(vec![
            note(
                1,
                "work/projects",
                "Roadmap",
                "Plan for #Work and #crystal.\n\nSee [[ideas]].",
            ),
            note(
                2,
                "work",
                "meeting 2026-11-07",
                "Notes about #work, #2026 is not a tag.",
            ),
            note(
                3,
                "",
                "ideas",
                "`#notatag` in code\n\n```\n#nor here\n```\n#idea/sub",
            ),
            note(4, "home", "ideas", "Other ideas, #Home."),
            note(5, "", "todo.txt", "#plain [[nolink]]"),
        ])
    }

    #[test]
    fn names_and_lookup() {
        let n = sample();
        assert_eq!(n.name(0), "work/projects/Roadmap");
        assert_eq!(n.name(2), "ideas");
        assert_eq!(n.find("work/projects/roadmap"), Found::One(0));
        assert_eq!(n.find("Roadmap"), Found::One(0), "a unique bare title");
        assert_eq!(
            n.find("ideas"),
            Found::One(2),
            "a full name wins over titles"
        );
        assert_eq!(n.find("home/IDEAS"), Found::One(3));
        assert_eq!(n.find("nope"), Found::None);
        assert_eq!(n.find("work/nope"), Found::None);
        let more = notes(vec![note(1, "a", "x", ""), note(2, "b", "x", "")]);
        assert_eq!(more.find("x"), Found::Ambiguous(vec![0, 1]));
    }

    #[test]
    fn tags_from_markdown_text_only() {
        let n = sample();
        let tags: Vec<&str> = n.tags().iter().map(|t| t.shown.as_str()).collect();
        assert_eq!(tags, ["crystal", "Home", "idea/sub", "Work"]);
        assert_eq!(n.tagged("#work"), [0, 1]);
        assert_eq!(n.tagged("WORK"), [0, 1]);
        assert!(n.tagged("plain").is_empty(), "plain text has no tags");
        assert!(n.tagged("notatag").is_empty() && n.tagged("nor").is_empty());
    }

    #[test]
    fn tag_rules() {
        let mut out = Vec::new();
        tags_in("#a b#c #d-e_f/g #12 #x1 # #é (#paren", &mut out);
        let tags: Vec<&str> = out.iter().map(|t| t.as_str()).collect();
        assert_eq!(tags, ["a", "d-e_f/g", "x1", "é"]);
    }

    #[test]
    fn links_from_markdown_only() {
        let n = sample();
        let links: Vec<&str> = n.links(0).iter().map(|l| l.as_str()).collect();
        assert_eq!(links, ["ideas"]);
        assert!(n.links(4).is_empty());
    }
}
