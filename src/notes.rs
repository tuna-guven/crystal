//! An unlocked vault's notes and the indexes built from them
//! (docs/decisions/0034-vault-format-2.md): built at unlock, never stored,
//! all in zeroizing memory.
//!
//! - names: each note's full name (`folder/title`), and a case-insensitive
//!   lookup; a bare title resolves when exactly one note has it;
//! - tags: Obsidian `#tag` in Markdown text outside code, case-insensitive;
//! - links: each Markdown note's `[[link]]` targets (followed in the pager),
//!   and its headings and block ids (`^id`), so links to them can be
//!   checked without rendering.
//!
//! Plain text notes (`*.txt`, decision 0038) contribute names only.

use std::collections::HashMap;

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};
use zeroize::Zeroizing;

use crate::md::render::{Kind, block_id, kind_of};
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
    /// Heading texts per note.
    headings: Vec<Vec<Zeroizing<String>>>,
    /// Block ids per note.
    blocks: Vec<Vec<Zeroizing<String>>>,
}

/// Where a link points inside its note.
#[derive(Debug, PartialEq, Eq)]
pub enum Fragment<'a> {
    Heading(&'a str),
    Block(&'a str),
}

/// A link target split: the note's name (empty for the same note) and
/// what inside it.
pub fn split_target(target: &str) -> (&str, Option<Fragment<'_>>) {
    match target.split_once('#') {
        None => (target, None),
        Some((name, fragment)) => (
            name,
            Some(match fragment.strip_prefix('^') {
                Some(id) => Fragment::Block(id),
                None => Fragment::Heading(fragment),
            }),
        ),
    }
}

/// Why a link does not lead anywhere.
#[derive(Debug, PartialEq, Eq)]
pub enum Broken {
    NoNote,
    /// A bare title more than one note has.
    Ambiguous,
    NoHeading,
    NoBlock,
}

impl Broken {
    pub fn describe(&self) -> &'static str {
        match self {
            Self::NoNote => "no note with that name",
            Self::Ambiguous => "more than one note has that title; write folder/title",
            Self::NoHeading => "that note has no such heading",
            Self::NoBlock => "that note has no such block (^id)",
        }
    }
}

/// How a name given by the user resolved.
#[derive(Debug, PartialEq, Eq)]
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
            headings: Vec::with_capacity(count),
            blocks: Vec::with_capacity(count),
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
            let Scan {
                tags,
                links,
                headings,
                blocks,
            } = match kind_of(note.title.as_str()) {
                Kind::Markdown => scan(note.body.as_str()),
                Kind::Plain => Scan::default(),
            };
            notes.headings.push(headings);
            notes.blocks.push(blocks);
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

    /// A full name (any case), else a bare title that one note has. As in
    /// Obsidian, `name.md` also finds the note `name` (a note really
    /// titled `name.md` comes first).
    pub fn find(&self, name: &str) -> Found {
        match self.find_exact(name) {
            Found::None => name
                .len()
                .checked_sub(3)
                .filter(|&at| name.is_char_boundary(at) && name[at..].eq_ignore_ascii_case(".md"))
                .map_or(Found::None, |at| self.find_exact(&name[..at])),
            found => found,
        }
    }

    fn find_exact(&self, name: &str) -> Found {
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

    /// Where `target`, a link in note `from`, leads: a note and, if
    /// given, a heading or block that exists in it.
    pub fn resolve<'t>(
        &self,
        from: usize,
        target: &'t str,
    ) -> Result<(usize, Option<Fragment<'t>>), Broken> {
        let (name, fragment) = split_target(target);
        let note = if name.is_empty() {
            from
        } else {
            match self.find(name) {
                Found::One(index) => index,
                Found::None => return Err(Broken::NoNote),
                Found::Ambiguous(_) => return Err(Broken::Ambiguous),
            }
        };
        match &fragment {
            Some(Fragment::Heading(h)) if !self.headings[note].iter().any(|x| eq_ci(x, h)) => {
                Err(Broken::NoHeading)
            }
            Some(Fragment::Block(id)) if !self.blocks[note].iter().any(|x| x.as_str() == *id) => {
                Err(Broken::NoBlock)
            }
            _ => Ok((note, fragment)),
        }
    }

    /// The notes with a link that leads to `note`, in note order.
    pub fn backlinks(&self, note: usize) -> Vec<usize> {
        (0..self.len())
            .filter(|&from| {
                from != note
                    && self.links[from]
                        .iter()
                        .any(|t| matches!(self.resolve(from, t), Ok((to, _)) if to == note))
            })
            .collect()
    }

    /// Every link that leads nowhere: (note, target, why).
    pub fn broken_links(&self) -> Vec<(usize, &str, Broken)> {
        let mut broken = Vec::new();
        for from in 0..self.len() {
            for target in &self.links[from] {
                if let Err(why) = self.resolve(from, target) {
                    broken.push((from, target.as_str(), why));
                }
            }
        }
        broken
    }

    /// `[[link]]` targets of a note, as written (the pager and `links`
    /// go through `resolve`, `backlinks` and `broken_links`).
    #[cfg(test)]
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

/// `a` and `b` equal ignoring case, without lowercase copies.
fn eq_ci(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
}

/// What a Markdown note says about itself.
#[derive(Default)]
struct Scan {
    tags: Vec<Zeroizing<String>>,
    links: Vec<Zeroizing<String>>,
    headings: Vec<Zeroizing<String>>,
    blocks: Vec<Zeroizing<String>>,
}

/// Obsidian tags, wikilink targets, headings and block ids in Markdown;
/// tags only outside code.
fn scan(body: &str) -> Scan {
    let mut tags = Vec::new();
    let mut links = Vec::new();
    let mut headings = Vec::new();
    let mut heading: Option<Zeroizing<String>> = None;
    let mut in_code = false;
    let options = Options::ENABLE_WIKILINKS | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    for (event, range) in Parser::new_ext(body, options).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => in_code = true,
            Event::End(TagEnd::CodeBlock) => in_code = false,
            // The heading's source is at least as long as its text
            // (entities only shrink), so the buffer never reallocates.
            Event::Start(Tag::Heading { .. }) => {
                heading = Some(Zeroizing::new(String::with_capacity(range.len())));
            }
            Event::End(TagEnd::Heading(_)) => headings.extend(heading.take()),
            Event::Text(text) if heading.is_some() => {
                if let Some(h) = &mut heading {
                    push_wiped(h, &text);
                }
                tags_in(&text, &mut tags);
            }
            // Inline code is part of a heading's text but holds no tags.
            Event::Code(text) => {
                if let Some(h) = &mut heading {
                    push_wiped(h, &text);
                }
            }
            Event::Text(text) if !in_code => tags_in(&text, &mut tags),
            Event::Start(Tag::Link {
                link_type: LinkType::WikiLink { .. },
                dest_url,
                ..
            }) => links.push(Zeroizing::new(dest_url.to_string())),
            _ => {}
        }
    }
    let blocks = body
        .lines()
        .filter_map(|line| block_id(line).map(|(_, id)| Zeroizing::new(id.to_owned())))
        .collect();
    Scan {
        tags,
        links,
        headings,
        blocks,
    }
}

/// `#tag` at the start or after whitespace: letters, digits, `_`, `-`,
/// `/`, and not only digits (`#1` is not a tag).
/// Appends `text` to `buffer`; if it would not fit, moves to a larger
/// buffer first so the old one is wiped, never reallocated in place.
fn push_wiped(buffer: &mut Zeroizing<String>, text: &str) {
    let needed = buffer.len() + text.len();
    if needed > buffer.capacity() {
        let mut larger = Zeroizing::new(String::with_capacity(needed.max(buffer.capacity() * 2)));
        larger.push_str(buffer);
        *buffer = larger;
    }
    buffer.push_str(text);
}

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
    fn md_suffix_finds_the_note() {
        let n = sample();
        assert_eq!(n.find("work/projects/Roadmap.md"), Found::One(0));
        assert_eq!(n.find("roadmap.MD"), Found::One(0));
        assert_eq!(n.find("nope.md"), Found::None);
        assert_eq!(n.find(".md"), Found::None);
        let real = notes(vec![note(1, "", "x", ""), note(2, "", "x.md", "")]);
        assert_eq!(real.find("x.md"), Found::One(1), "a real title wins");
    }

    #[test]
    fn long_headings_and_code_in_headings() {
        let long = "word ".repeat(100);
        let body = format!(
            "# {long}\n\n## Plan `#notatag` #real\n\n[[#{}]]",
            long.trim()
        );
        let n = notes(vec![note(1, "", "a", &body)]);
        assert!(n.broken_links().is_empty(), "a 500-byte heading resolves");
        assert!(n.resolve(0, "#plan #notatag #real").is_ok());
        assert!(n.tagged("notatag").is_empty(), "no tags from inline code");
        assert_eq!(n.tagged("real"), [0]);
    }

    #[test]
    fn tag_rules() {
        let mut out = Vec::new();
        tags_in("#a b#c #d-e_f/g #12 #x1 # #é (#paren", &mut out);
        let tags: Vec<&str> = out.iter().map(|t| t.as_str()).collect();
        assert_eq!(tags, ["a", "d-e_f/g", "x1", "é"]);
    }

    #[test]
    fn links_resolve_with_headings_and_blocks() {
        let n = notes(vec![
            note(
                1,
                "",
                "a",
                "# Intro\n\nSee [[b#Part Two]], [[b#^k3]], [[#intro]].\n\n[[nope]] [[b#Gone]] [[x]] [[b#^no]]",
            ),
            note(2, "", "b", "## Part two\n\nA paragraph. ^k3"),
            note(3, "p", "x", ""),
            note(4, "q", "x", ""),
        ]);
        assert_eq!(
            n.resolve(0, "b#Part Two"),
            Ok((1, Some(Fragment::Heading("Part Two"))))
        );
        assert_eq!(n.resolve(0, "b#^k3"), Ok((1, Some(Fragment::Block("k3")))));
        assert_eq!(
            n.resolve(0, "#intro"),
            Ok((0, Some(Fragment::Heading("intro"))))
        );
        assert_eq!(n.resolve(0, "p/x"), Ok((2, None)));
        let broken: Vec<(&str, Broken)> = n
            .broken_links()
            .into_iter()
            .map(|(_, t, b)| (t, b))
            .collect();
        assert_eq!(
            broken,
            [
                ("nope", Broken::NoNote),
                ("b#Gone", Broken::NoHeading),
                ("x", Broken::Ambiguous),
                ("b#^no", Broken::NoBlock)
            ]
        );
        assert_eq!(n.backlinks(1), [0]);
        assert!(
            n.backlinks(0).is_empty(),
            "a link to itself is not a backlink"
        );
    }

    #[test]
    fn links_from_markdown_only() {
        let n = sample();
        let links: Vec<&str> = n.links(0).iter().map(|l| l.as_str()).collect();
        assert_eq!(links, ["ideas"]);
        assert!(n.links(4).is_empty());
    }
}
