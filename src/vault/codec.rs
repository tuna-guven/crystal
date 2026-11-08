//! Vault contents to bytes and back (docs/design/storage.md, "Vault
//! contents"; docs/decisions/0034-vault-format-2.md), with postcard.
//!
//! Format 2, written (postcard: integers are varints, strings and lists
//! carry a varint length):
//!
//! ```text
//! format: u16 = 2 | vault_id: [u8; 32] | generation: u64 | notes: [Note]
//! Note = id: [u8; 16] | folder: str | title: str | created: u64
//!        | modified: u64 | body: str
//! ```
//!
//! Format 1, still read: no `folder`, and `tags: [str]` after `title`.
//! Its notes get an empty folder, and their tags are appended to the body
//! as Obsidian `#tag` text; the next save writes format 2.
//!
//! The format number is decoded on its own first, so an unknown version is
//! refused before anything else is parsed. Field order is the on-disk
//! order; changing it, or a limit below, needs a new format number.
//!
//! `NoteText` has no `Serialize`, so the notes go through wire structs
//! that only borrow `&str`. Encoding writes into one zeroizing buffer of
//! the exact final size, so it never grows and leaves no partial copies
//! behind. Decoding borrows `&str` straight from the decrypted buffer,
//! checks every limit and name rule, and only then copies each string
//! once, into its own `Zeroizing<String>`.

#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "sealed and opened by `crystal init` and unlock (#14, #15)"
    )
)]

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::model::name::{self, NameError};
use crate::model::{Note, NoteId, NoteText, VaultId, assert_not_impl};

/// The vault format this build writes, and the newest it reads.
pub const FORMAT: u16 = 2;
/// The oldest format this build reads.
pub const OLDEST_FORMAT: u16 = 1;

/// Limits, checked when decoding and when encoding, so Crystal never
/// writes a vault it would refuse to read. The whole encoded vault must
/// also fit `crypto::MAX_PLAINTEXT` once padded. Titles and folders are
/// limited by the name rules (`model::name`).
pub const MAX_NOTES: usize = 100_000;
pub const MAX_BODY_BYTES: usize = 8 << 20;

/// Format 1's own limits, for reading it.
const V1_MAX_TITLE_BYTES: usize = 1024;
const V1_MAX_TAGS_PER_NOTE: usize = 64;
const V1_MAX_TAG_BYTES: usize = 256;

/// Everything in vault.age. No Debug, Clone or Serialize: it holds notes.
pub struct VaultContents {
    pub vault_id: VaultId,
    /// +1 on every save; compared with the state directory's counter to
    /// detect rollback.
    pub generation: u64,
    pub notes: Vec<Note>,
}

assert_not_impl!(VaultContents: fmt::Debug, fmt::Display, serde::Serialize, Clone);

/// One format-2 note as postcard sees it: borrowed text, nothing owned.
#[derive(Serialize, Deserialize)]
struct WireNote<'a> {
    id: [u8; 16],
    #[serde(borrow)]
    folder: &'a str,
    #[serde(borrow)]
    title: &'a str,
    created: u64,
    modified: u64,
    #[serde(borrow)]
    body: &'a str,
}

/// One format-1 note, read only.
#[derive(Deserialize)]
struct WireNoteV1<'a> {
    id: [u8; 16],
    #[serde(borrow)]
    title: &'a str,
    #[serde(borrow)]
    tags: Vec<&'a str>,
    created: u64,
    modified: u64,
    #[serde(borrow)]
    body: &'a str,
}

/// Everything after the format number.
#[derive(Serialize, Deserialize)]
struct WireVault<N> {
    vault_id: [u8; 32],
    generation: u64,
    notes: Vec<N>,
}

// They point at note text, so they follow the same rule as NoteText.
assert_not_impl!(WireNote<'static>: fmt::Debug, fmt::Display, Clone);
assert_not_impl!(WireNoteV1<'static>: fmt::Debug, fmt::Display, Clone);
assert_not_impl!(WireVault<WireNote<'static>>: fmt::Debug, fmt::Display, Clone);

impl<'a> WireNote<'a> {
    fn borrow(note: &'a Note) -> Self {
        Self {
            id: *note.id.as_bytes(),
            folder: note.folder.as_str(),
            title: note.title.as_str(),
            created: note.created,
            modified: note.modified,
            body: note.body.as_str(),
        }
    }

    fn to_note(&self) -> Note {
        Note {
            id: NoteId::from_bytes(self.id),
            folder: text(self.folder),
            title: text(self.title),
            created: self.created,
            modified: self.modified,
            body: text(self.body),
        }
    }
}

impl WireNoteV1<'_> {
    /// Format 1's limits, before anything is copied.
    fn check(&self) -> Result<(), CodecError> {
        if self.title.len() > V1_MAX_TITLE_BYTES
            || self.body.len() > MAX_BODY_BYTES
            || self.tags.iter().any(|t| t.len() > V1_MAX_TAG_BYTES)
        {
            return Err(CodecError::FieldTooLong);
        }
        if self.tags.len() > V1_MAX_TAGS_PER_NOTE {
            return Err(CodecError::TooManyTags);
        }
        Ok(())
    }

    /// The note in format 2: top level, tags appended to the body as
    /// `#tag`, separated from it by an empty line.
    fn to_note(&self) -> Note {
        let tags_len: usize = self.tags.iter().map(|t| t.len() + 2).sum();
        let mut body = Zeroizing::new(String::with_capacity(self.body.len() + 2 + tags_len));
        body.push_str(self.body);
        if !self.tags.is_empty() {
            if !self.body.is_empty() {
                body.push_str("\n\n");
            }
            for (i, tag) in self.tags.iter().enumerate() {
                if i > 0 {
                    body.push(' ');
                }
                body.push('#');
                body.push_str(tag);
            }
        }
        Note {
            id: NoteId::from_bytes(self.id),
            folder: text(""),
            title: text(self.title),
            created: self.created,
            modified: self.modified,
            body: NoteText::from_zeroizing(body),
        }
    }
}

/// The format-2 rules over borrowed fields: note count, body size, name
/// rules, unique ids and unique names.
fn check<'a>(
    notes: impl ExactSizeIterator<Item = ([u8; 16], &'a str, &'a str, &'a str)>,
) -> Result<(), CodecError> {
    if notes.len() > MAX_NOTES {
        return Err(CodecError::TooManyNotes);
    }
    let mut ids = HashSet::with_capacity(notes.len());
    let mut names = HashSet::with_capacity(notes.len());
    for (id, folder, title, body) in notes {
        if body.len() > MAX_BODY_BYTES {
            return Err(CodecError::FieldTooLong);
        }
        name::check_folder(folder).map_err(CodecError::Name)?;
        name::check_segment(title).map_err(CodecError::Name)?;
        if !ids.insert(id) {
            return Err(CodecError::DuplicateId);
        }
        if !names.insert(name::unique_key(folder, title)) {
            return Err(CodecError::DuplicateName);
        }
    }
    Ok(())
}

fn check_notes(notes: &[Note]) -> Result<(), CodecError> {
    check(notes.iter().map(|n| {
        (
            *n.id.as_bytes(),
            n.folder.as_str(),
            n.title.as_str(),
            n.body.as_str(),
        )
    }))
}

/// One copy, into an allocation of exactly the right size.
fn text(s: &str) -> NoteText {
    NoteText::from_zeroizing(Zeroizing::new(s.to_owned()))
}

/// Fixed messages only: never a byte or a note from the input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecError {
    Encode,
    Malformed,
    TrailingBytes,
    UnknownFormat,
    TooManyNotes,
    TooManyTags,
    FieldTooLong,
    DuplicateId,
    Name(NameError),
    DuplicateName,
    /// The blob's format is newer than the vault's `VERSION` allows.
    NewerThanVersion,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode => f.write_str("cannot encode the vault"),
            Self::Malformed => f.write_str("vault contents are malformed"),
            Self::TrailingBytes => f.write_str("vault contents have unexpected trailing bytes"),
            Self::UnknownFormat => {
                f.write_str("the vault uses a format this version of Crystal cannot read")
            }
            Self::TooManyNotes => f.write_str("the vault has more notes than Crystal allows"),
            Self::TooManyTags => f.write_str("a note has more tags than Crystal allows"),
            Self::FieldTooLong => {
                f.write_str("a note's title, tag or body is longer than Crystal allows")
            }
            Self::DuplicateId => f.write_str("two notes in the vault have the same id"),
            Self::Name(e) => e.fmt(f),
            Self::DuplicateName => f.write_str(
                "two notes in the vault have the same name (folder and title, ignoring case)",
            ),
            Self::NewerThanVersion => f.write_str(
                "the vault's contents are in a newer format than its VERSION file allows",
            ),
        }
    }
}

impl std::error::Error for CodecError {}

/// The vault as format-2 postcard bytes, in a zeroizing buffer of the
/// exact size. Refuses contents that break a limit or a name rule, or
/// repeat a note id or name.
pub fn encode(contents: &VaultContents) -> Result<Zeroizing<Vec<u8>>, CodecError> {
    check_notes(&contents.notes)?;
    let wire = WireVault {
        vault_id: *contents.vault_id.as_bytes(),
        generation: contents.generation,
        notes: contents
            .notes
            .iter()
            .map(WireNote::borrow)
            .collect::<Vec<_>>(),
    };
    let framed = (FORMAT, &wire);
    let size = postcard::serialize_with_flavor(&framed, postcard::ser_flavors::Size::default())
        .map_err(|_| CodecError::Encode)?;
    let mut out = Zeroizing::new(vec![0; size]);
    let used = postcard::to_slice(&framed, &mut out)
        .map_err(|_| CodecError::Encode)?
        .len();
    if used != size {
        return Err(CodecError::Encode);
    }
    Ok(out)
}

/// The vault back from postcard bytes, format 1 or 2. Fails closed: an
/// unknown format, malformed input (including invalid UTF-8 and a cut-off
/// encoding), a broken limit or name rule, a repeated note id or name, and
/// any byte left over are refused. Limits are checked before any note text
/// is copied.
pub fn decode(bytes: &[u8]) -> Result<VaultContents, CodecError> {
    decode_at_most(bytes, FORMAT)
}

/// As `decode`, refusing a format newer than `max`: the vault's `VERSION`
/// names the newest format its directory may hold.
pub fn decode_at_most(bytes: &[u8], max: u16) -> Result<VaultContents, CodecError> {
    let (format, rest): (u16, _) =
        postcard::take_from_bytes(bytes).map_err(|_| CodecError::Malformed)?;
    if format > max && format <= FORMAT {
        return Err(CodecError::NewerThanVersion);
    }
    match format {
        FORMAT => {
            let wire = take_all::<WireNote<'_>>(rest)?;
            check(wire.notes.iter().map(|n| (n.id, n.folder, n.title, n.body)))?;
            Ok(VaultContents {
                vault_id: VaultId::from_bytes(wire.vault_id),
                generation: wire.generation,
                notes: wire.notes.iter().map(WireNote::to_note).collect(),
            })
        }
        OLDEST_FORMAT => {
            let wire = take_all::<WireNoteV1<'_>>(rest)?;
            if wire.notes.len() > MAX_NOTES {
                return Err(CodecError::TooManyNotes);
            }
            wire.notes.iter().try_for_each(WireNoteV1::check)?;
            let notes: Vec<Note> = wire.notes.iter().map(WireNoteV1::to_note).collect();
            // The converted notes must meet format 2's rules too.
            check_notes(&notes)?;
            Ok(VaultContents {
                vault_id: VaultId::from_bytes(wire.vault_id),
                generation: wire.generation,
                notes,
            })
        }
        _ => Err(CodecError::UnknownFormat),
    }
}

/// The rest of the input as one `WireVault`, with nothing left over.
fn take_all<'a, N: Deserialize<'a>>(bytes: &'a [u8]) -> Result<WireVault<N>, CodecError> {
    let (wire, rest): (WireVault<N>, _) =
        postcard::take_from_bytes(bytes).map_err(|_| CodecError::Malformed)?;
    if !rest.is_empty() {
        return Err(CodecError::TrailingBytes);
    }
    Ok(wire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A note's plain parts: id, folder, title, created, modified, body.
    type Raw = ([u8; 16], String, String, u64, u64, String);

    fn note(raw: &Raw) -> Note {
        let (id, folder, title, created, modified, body) = raw;
        Note {
            id: NoteId::from_bytes(*id),
            folder: text(folder),
            title: text(title),
            created: *created,
            modified: *modified,
            body: text(body),
        }
    }

    fn same(note: &Note, raw: &Raw) -> bool {
        let (id, folder, title, created, modified, body) = raw;
        note.id.as_bytes() == id
            && note.folder.as_str() == folder
            && note.title.as_str() == title
            && note.created == *created
            && note.modified == *modified
            && note.body.as_str() == body
    }

    fn sample() -> Vec<Raw> {
        vec![
            (
                [1; 16],
                "work/projects".into(),
                "Title".into(),
                10,
                20,
                "Body with [[link]], #tag and ünïcode".into(),
            ),
            (
                [2; 16],
                String::new(),
                "Other".into(),
                0,
                u64::MAX,
                String::new(),
            ),
        ]
    }

    fn contents(raw: &[Raw]) -> VaultContents {
        VaultContents {
            vault_id: VaultId::from_bytes([7; 32]),
            generation: 3,
            notes: raw.iter().map(note).collect(),
        }
    }

    fn encoded(raw: &[Raw]) -> Zeroizing<Vec<u8>> {
        encode(&contents(raw)).unwrap()
    }

    /// Format-2 bytes for `raw` without encode's checks, to test that
    /// decode enforces them on its own (a planted vault skips our encoder).
    fn unchecked(raw: &[Raw]) -> Vec<u8> {
        let notes: Vec<Note> = raw.iter().map(note).collect();
        let wire = WireVault {
            vault_id: [7; 32],
            generation: 3,
            notes: notes.iter().map(WireNote::borrow).collect::<Vec<_>>(),
        };
        postcard::to_allocvec(&(FORMAT, &wire)).unwrap()
    }

    fn one(folder: &str, title: &str, body: &str) -> Raw {
        ([1; 16], folder.into(), title.into(), 0, 0, body.into())
    }

    /// A format-1 vault, as Crystal wrote it before format 2.
    #[derive(Serialize)]
    struct V1Note<'a> {
        id: [u8; 16],
        title: &'a str,
        tags: Vec<&'a str>,
        created: u64,
        modified: u64,
        body: &'a str,
    }

    fn v1(notes: &[V1Note<'_>]) -> Vec<u8> {
        let wire = WireVault {
            vault_id: [7; 32],
            generation: 3,
            notes: notes.iter().collect::<Vec<_>>(),
        };
        postcard::to_allocvec(&(1u16, &wire)).unwrap()
    }

    fn v1_note<'a>(id: u8, title: &'a str, tags: Vec<&'a str>, body: &'a str) -> V1Note<'a> {
        V1Note {
            id: [id; 16],
            title,
            tags,
            created: 1,
            modified: 2,
            body,
        }
    }

    /// Titles that pass the name rules, for the property test.
    fn title() -> impl Strategy<Value = String> {
        "[A-Za-z0-9ü日][A-Za-z0-9ü日 .-]{0,30}[A-Za-z0-9ü日]"
    }

    proptest! {
        // docs/development/testing.md "Round-trip properties": any valid
        // vault, including non-ASCII text, comes back exactly as it went in.
        #[test]
        fn round_trip(
            vault_id in any::<[u8; 32]>(),
            generation in any::<u64>(),
            raw in prop::collection::vec(
                (
                    any::<[u8; 16]>(),
                    prop::sample::select(vec!["", "work", "work/projects", "日本"]),
                    title(),
                    any::<u64>(),
                    any::<u64>(),
                    any::<String>(),
                ),
                0..8,
            ),
        ) {
            // Ids and names must be unique; drop repeats.
            let mut ids = HashSet::new();
            let mut names = HashSet::new();
            let raw: Vec<Raw> = raw
                .into_iter()
                .filter(|r| ids.insert(r.0) && names.insert(name::unique_key(r.1, &r.2)))
                .map(|(id, f, t, c, m, b)| (id, f.to_owned(), t, c, m, b))
                .collect();
            let vault = VaultContents {
                vault_id: VaultId::from_bytes(vault_id),
                generation,
                notes: raw.iter().map(note).collect(),
            };
            let back = decode(&encode(&vault).unwrap()).unwrap();
            prop_assert_eq!(back.vault_id, vault.vault_id);
            prop_assert_eq!(back.generation, generation);
            prop_assert_eq!(back.notes.len(), raw.len());
            for (n, r) in back.notes.iter().zip(&raw) {
                prop_assert!(same(n, r));
            }
        }
    }

    #[test]
    fn empty_vault_round_trips() {
        let back = decode(&encoded(&[])).unwrap();
        assert!(back.notes.is_empty());
        assert_eq!(back.generation, 3);
    }

    // The exact bytes of a small vault. A change here is a format change:
    // it needs a new FORMAT number and a decision record.
    #[test]
    fn golden_bytes() {
        let bytes = encoded(&[([1; 16], "f".into(), "T".into(), 1, 2, "B".into())]);
        let mut expected = vec![0x02]; // format 2
        expected.extend([7; 32]); // vault_id
        expected.push(0x03); // generation
        expected.push(0x01); // one note
        expected.extend([1; 16]); // id
        expected.extend([0x01, b'f']); // folder
        expected.extend([0x01, b'T']); // title
        expected.extend([0x01, 0x02]); // created, modified
        expected.extend([0x01, b'B']); // body
        assert_eq!(bytes[..], expected[..]);
    }

    // Format 1's exact bytes (its golden test before format 2) still
    // decode: an empty folder, the tag moved into the body.
    #[test]
    fn format_1_golden_bytes_still_decode() {
        let mut v1 = vec![0x01]; // format 1
        v1.extend([7; 32]); // vault_id
        v1.push(0x03); // generation
        v1.push(0x01); // one note
        v1.extend([1; 16]); // id
        v1.extend([0x01, b'T']); // title
        v1.extend([0x01, 0x01, b'a']); // one tag
        v1.extend([0x01, 0x02]); // created, modified
        v1.extend([0x01, b'B']); // body
        let back = decode(&v1).unwrap();
        let n = &back.notes[0];
        assert_eq!(
            (n.folder.as_str(), n.title.as_str(), n.body.as_str()),
            ("", "T", "B\n\n#a")
        );
        assert_eq!((n.created, n.modified, back.generation), (1, 2, 3));
        // Written back, it is format 2.
        assert_eq!(encode(&back).unwrap()[0], 0x02);
    }

    #[test]
    fn format_1_tags_join_the_body() {
        let back = decode(&v1(&[
            v1_note(1, "None", vec![], "text"),
            v1_note(2, "Only tags", vec!["x", "y/z"], ""),
            v1_note(3, "Both", vec!["x"], "text"),
        ]))
        .unwrap();
        let bodies: Vec<&str> = back.notes.iter().map(|n| n.body.as_str()).collect();
        assert_eq!(bodies, ["text", "#x #y/z", "text\n\n#x"]);
    }

    // A format-1 note whose title breaks the name rules, or two whose
    // titles now collide, are refused: nothing is renamed silently.
    #[test]
    fn format_1_must_meet_the_name_rules() {
        assert_eq!(
            decode(&v1(&[v1_note(1, "a/b", vec![], "")])).err(),
            Some(CodecError::Name(NameError::BadCharacter))
        );
        assert_eq!(
            decode(&v1(&[v1_note(1, "", vec![], "")])).err(),
            Some(CodecError::Name(NameError::Empty))
        );
        assert_eq!(
            decode(&v1(&[
                v1_note(1, "Same", vec![], ""),
                v1_note(2, "same", vec![], "")
            ]))
            .err(),
            Some(CodecError::DuplicateName)
        );
        let many: Vec<&str> = vec!["x"; V1_MAX_TAGS_PER_NOTE + 1];
        assert_eq!(
            decode(&v1(&[v1_note(1, "T", many, "")])).err(),
            Some(CodecError::TooManyTags)
        );
    }

    // VERSION 1 with a format-2 blob is refused; format 1 under VERSION 2
    // (a vault not saved since) is fine.
    #[test]
    fn blob_format_must_not_exceed_version() {
        let v2 = encoded(&sample());
        assert_eq!(
            decode_at_most(&v2, 1).err(),
            Some(CodecError::NewerThanVersion)
        );
        assert!(decode_at_most(&v2, 2).is_ok());
        let v1 = v1(&[v1_note(1, "T", vec![], "")]);
        assert!(decode_at_most(&v1, 1).is_ok() && decode_at_most(&v1, 2).is_ok());
    }

    #[test]
    fn unknown_format_is_refused_first() {
        for format in [0u16, 3, 300, u16::MAX] {
            let mut bytes = postcard::to_allocvec(&format).unwrap();
            // Whatever follows, even garbage, is never parsed.
            bytes.extend([0xff; 8]);
            assert_eq!(
                decode(&bytes).err(),
                Some(CodecError::UnknownFormat),
                "{format}"
            );
        }
    }

    // Every cut-off version of an encoding is refused, never read as a
    // shorter but valid vault.
    #[test]
    fn truncated_input_is_refused() {
        let bytes = encoded(&sample());
        for len in 0..bytes.len() {
            let err = decode(&bytes[..len]).err();
            assert_eq!(err, Some(CodecError::Malformed), "length {len}");
        }
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let mut bytes = encoded(&sample()).to_vec();
        bytes.push(0);
        assert_eq!(decode(&bytes).err(), Some(CodecError::TrailingBytes));
    }

    #[test]
    fn invalid_utf8_is_refused() {
        let mut bytes = encoded(&[one("", "ab", "")]).to_vec();
        let at = bytes.windows(2).position(|w| w == b"ab").unwrap();
        bytes[at] = 0xff;
        assert_eq!(decode(&bytes).err(), Some(CodecError::Malformed));
    }

    #[test]
    fn duplicate_ids_and_names_are_refused() {
        let mut other = one("", "b", "");
        other.0 = [1; 16];
        let ids = [one("", "a", ""), other];
        assert_eq!(
            decode(&unchecked(&ids)).err(),
            Some(CodecError::DuplicateId)
        );
        assert_eq!(encode(&contents(&ids)).err(), Some(CodecError::DuplicateId));

        let mut second = one("Work", "ROADMAP", "");
        second.0 = [2; 16];
        let names = [one("work", "Roadmap", ""), second];
        assert_eq!(
            decode(&unchecked(&names)).err(),
            Some(CodecError::DuplicateName)
        );
        assert_eq!(
            encode(&contents(&names)).err(),
            Some(CodecError::DuplicateName)
        );

        // The same title in two folders is fine.
        let mut elsewhere = one("home", "Roadmap", "");
        elsewhere.0 = [2; 16];
        assert!(decode(&encoded(&[one("work", "Roadmap", ""), elsewhere])).is_ok());
    }

    // Name rules and the body limit: refused by decode (planted input) and
    // by encode (never written).
    #[test]
    fn names_and_limits() {
        let body = "b".repeat(MAX_BODY_BYTES);
        assert!(decode(&encoded(&[one("", "T", &body)])).is_ok());
        let over: [(Raw, CodecError); 5] = [
            (one("", "T", &format!("{body}b")), CodecError::FieldTooLong),
            (one("", "", ""), CodecError::Name(NameError::Empty)),
            (
                one("", "a#b", ""),
                CodecError::Name(NameError::BadCharacter),
            ),
            (one("/abs", "T", ""), CodecError::Name(NameError::Empty)),
            (one("a/../b", "T", ""), CodecError::Name(NameError::BadForm)),
        ];
        for (raw, err) in over {
            let raw = [raw];
            assert_eq!(decode(&unchecked(&raw)).err(), Some(err));
            assert_eq!(encode(&contents(&raw)).err(), Some(err));
        }
    }

    #[test]
    fn note_count_limit() {
        let raw: Vec<Raw> = (0..=MAX_NOTES as u32)
            .map(|i| {
                let mut id = [0; 16];
                id[..4].copy_from_slice(&i.to_le_bytes());
                (id, String::new(), format!("n{i}"), 0, 0, String::new())
            })
            .collect();
        let err = decode(&unchecked(&raw)).err();
        assert_eq!(err, Some(CodecError::TooManyNotes));
        assert_eq!(
            encode(&contents(&raw)).err(),
            Some(CodecError::TooManyNotes)
        );
        assert_eq!(
            decode(&encoded(&raw[..MAX_NOTES])).unwrap().notes.len(),
            MAX_NOTES
        );
    }

    // Errors carry no input: neither note text nor bytes.
    #[test]
    fn errors_never_show_content() {
        let mut bytes = encoded(&sample()).to_vec();
        bytes.push(b'x');
        let err = decode(&bytes).err().unwrap();
        let bad = decode(&unchecked(&[one("", "Title#", "")])).err().unwrap();
        for shown in [
            err.to_string(),
            format!("{err:?}"),
            bad.to_string(),
            format!("{bad:?}"),
        ] {
            assert!(
                !shown.contains("Title") && !shown.contains("link"),
                "{shown}"
            );
        }
    }
}
