//! Vault contents to bytes and back (docs/design/storage.md, "Vault
//! contents"), with postcard.
//!
//! Layout, format 1 (postcard: integers are varints, strings and lists
//! carry a varint length):
//!
//! ```text
//! format: u16 | vault_id: [u8; 32] | generation: u64 | notes: [Note]
//! Note = id: [u8; 16] | title: str | tags: [str] | created: u64
//!        | modified: u64 | body: str
//! ```
//!
//! The format number is decoded on its own first, so an unknown version is
//! refused before anything else is parsed. Field order is the on-disk
//! order; changing it, or a limit below, needs a new format number.
//!
//! `NoteText` has no `Serialize`, so the notes go through `WireNote`, a
//! private struct that only borrows `&str` from them. Encoding writes into
//! one zeroizing buffer of the exact final size, so it never grows and
//! leaves no partial copies behind. Decoding borrows `&str` straight from
//! the decrypted buffer, checks every limit, and only then copies each
//! string once, into its own `Zeroizing<String>`.

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

use crate::model::{Note, NoteId, NoteText, VaultId, assert_not_impl};

/// The vault format this build reads and writes. Also the content of the
/// `VERSION` file.
pub const FORMAT: u16 = 1;

/// Limits, checked when decoding and when encoding, so Crystal never
/// writes a vault it would refuse to read. The whole encoded vault must
/// also fit `crypto::MAX_PLAINTEXT` once padded.
pub const MAX_NOTES: usize = 100_000;
pub const MAX_TITLE_BYTES: usize = 1024;
pub const MAX_TAGS_PER_NOTE: usize = 64;
pub const MAX_TAG_BYTES: usize = 256;
pub const MAX_BODY_BYTES: usize = 8 << 20;

/// Everything in vault.age. No Debug, Clone or Serialize: it holds notes.
pub struct VaultContents {
    pub vault_id: VaultId,
    /// +1 on every save; compared with the state directory's counter to
    /// detect rollback (#11).
    pub generation: u64,
    pub notes: Vec<Note>,
}

assert_not_impl!(VaultContents: fmt::Debug, fmt::Display, serde::Serialize, Clone);

/// One note as postcard sees it: borrowed text, nothing owned.
#[derive(Serialize, Deserialize)]
struct WireNote<'a> {
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

// It points at note text, so it follows the same rule as NoteText.
assert_not_impl!(WireNote<'static>: fmt::Debug, fmt::Display, Clone);

/// Everything after the format number.
#[derive(Serialize, Deserialize)]
struct WireVault<'a> {
    vault_id: [u8; 32],
    generation: u64,
    #[serde(borrow)]
    notes: Vec<WireNote<'a>>,
}

assert_not_impl!(WireVault<'static>: fmt::Debug, fmt::Display, Clone);

impl<'a> WireNote<'a> {
    fn borrow(note: &'a Note) -> Self {
        Self {
            id: *note.id.as_bytes(),
            title: note.title.as_str(),
            tags: note.tags.iter().map(NoteText::as_str).collect(),
            created: note.created,
            modified: note.modified,
            body: note.body.as_str(),
        }
    }

    fn to_note(&self) -> Note {
        Note {
            id: NoteId::from_bytes(self.id),
            title: text(self.title),
            tags: self.tags.iter().map(|t| text(t)).collect(),
            created: self.created,
            modified: self.modified,
            body: text(self.body),
        }
    }
}

impl WireVault<'_> {
    /// Every limit, and unique note ids.
    fn check(&self) -> Result<(), CodecError> {
        if self.notes.len() > MAX_NOTES {
            return Err(CodecError::TooManyNotes);
        }
        let mut ids = HashSet::with_capacity(self.notes.len());
        for note in &self.notes {
            if note.title.len() > MAX_TITLE_BYTES
                || note.body.len() > MAX_BODY_BYTES
                || note.tags.iter().any(|t| t.len() > MAX_TAG_BYTES)
            {
                return Err(CodecError::FieldTooLong);
            }
            if note.tags.len() > MAX_TAGS_PER_NOTE {
                return Err(CodecError::TooManyTags);
            }
            if !ids.insert(note.id) {
                return Err(CodecError::DuplicateId);
            }
        }
        Ok(())
    }
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
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Encode => "cannot encode the vault",
            Self::Malformed => "vault contents are malformed",
            Self::TrailingBytes => "vault contents have unexpected trailing bytes",
            Self::UnknownFormat => "the vault uses a format this version of Crystal cannot read",
            Self::TooManyNotes => "the vault has more notes than Crystal allows",
            Self::TooManyTags => "a note has more tags than Crystal allows",
            Self::FieldTooLong => "a note's title, tag or body is longer than Crystal allows",
            Self::DuplicateId => "two notes in the vault have the same id",
        })
    }
}

impl std::error::Error for CodecError {}

/// The vault as postcard bytes, in a zeroizing buffer of the exact size.
/// Refuses contents that break a limit or repeat a note id.
pub fn encode(contents: &VaultContents) -> Result<Zeroizing<Vec<u8>>, CodecError> {
    let wire = WireVault {
        vault_id: *contents.vault_id.as_bytes(),
        generation: contents.generation,
        notes: contents.notes.iter().map(WireNote::borrow).collect(),
    };
    wire.check()?;
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

/// The vault back from postcard bytes. Fails closed: an unknown format,
/// malformed input (including invalid UTF-8 and a cut-off encoding), a
/// broken limit, a repeated note id, and any byte left over are refused.
/// Limits are checked before any note text is copied.
pub fn decode(bytes: &[u8]) -> Result<VaultContents, CodecError> {
    let (format, rest): (u16, _) =
        postcard::take_from_bytes(bytes).map_err(|_| CodecError::Malformed)?;
    if format != FORMAT {
        return Err(CodecError::UnknownFormat);
    }
    let (wire, rest): (WireVault<'_>, _) =
        postcard::take_from_bytes(rest).map_err(|_| CodecError::Malformed)?;
    if !rest.is_empty() {
        return Err(CodecError::TrailingBytes);
    }
    wire.check()?;
    Ok(VaultContents {
        vault_id: VaultId::from_bytes(wire.vault_id),
        generation: wire.generation,
        notes: wire.notes.iter().map(WireNote::to_note).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A note's plain parts, for building notes and comparing results.
    type Raw = ([u8; 16], String, Vec<String>, u64, u64, String);

    fn note(raw: &Raw) -> Note {
        let (id, title, tags, created, modified, body) = raw;
        Note {
            id: NoteId::from_bytes(*id),
            title: text(title),
            tags: tags.iter().map(|t| text(t)).collect(),
            created: *created,
            modified: *modified,
            body: text(body),
        }
    }

    fn same(note: &Note, raw: &Raw) -> bool {
        let (id, title, tags, created, modified, body) = raw;
        note.id.as_bytes() == id
            && note.title.as_str() == title
            && note
                .tags
                .iter()
                .map(NoteText::as_str)
                .eq(tags.iter().map(String::as_str))
            && note.created == *created
            && note.modified == *modified
            && note.body.as_str() == body
    }

    fn sample() -> Vec<Raw> {
        vec![
            (
                [1; 16],
                "Title".into(),
                vec!["a".into(), "b".into()],
                10,
                20,
                "Body with [[link]] and ünïcode".into(),
            ),
            ([2; 16], String::new(), vec![], 0, u64::MAX, String::new()),
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

    /// Bytes for `raw` without encode's limit checks, to test that decode
    /// enforces them on its own (a planted vault skips our encoder).
    fn unchecked(raw: &[Raw]) -> Vec<u8> {
        let notes: Vec<Note> = raw.iter().map(note).collect();
        let wire = WireVault {
            vault_id: [7; 32],
            generation: 3,
            notes: notes.iter().map(WireNote::borrow).collect(),
        };
        postcard::to_allocvec(&(FORMAT, &wire)).unwrap()
    }

    fn one(title: &str, tags: &[&str], body: &str) -> Raw {
        let tags = tags.iter().map(|t| t.to_string()).collect();
        ([1; 16], title.into(), tags, 0, 0, body.into())
    }

    proptest! {
        // docs/development/testing.md "Round-trip properties": any vault,
        // including empty and non-ASCII text, comes back exactly as it
        // went in.
        #[test]
        fn round_trip(
            vault_id in any::<[u8; 32]>(),
            generation in any::<u64>(),
            raw in prop::collection::vec(
                (
                    any::<[u8; 16]>(),
                    ".{0,40}",
                    prop::collection::vec(".{0,10}", 0..4),
                    any::<u64>(),
                    any::<u64>(),
                    any::<String>(),
                ),
                0..8,
            ),
        ) {
            // Note ids must be unique; drop repeats.
            let mut raw = raw;
            let mut ids = HashSet::new();
            raw.retain(|r| ids.insert(r.0));
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
        let bytes = encoded(&[([1; 16], "T".into(), vec!["a".into()], 1, 2, "B".into())]);
        let mut expected = vec![0x01]; // format 1
        expected.extend([7; 32]); // vault_id
        expected.push(0x03); // generation
        expected.push(0x01); // one note
        expected.extend([1; 16]); // id
        expected.extend([0x01, b'T']); // title
        expected.extend([0x01, 0x01, b'a']); // one tag
        expected.extend([0x01, 0x02]); // created, modified
        expected.extend([0x01, b'B']); // body
        assert_eq!(bytes[..], expected[..]);
    }

    #[test]
    fn unknown_format_is_refused_first() {
        for format in [0u16, 2, 300, u16::MAX] {
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
        let mut bytes = encoded(&[one("ab", &[], "")]).to_vec();
        let at = bytes.windows(2).position(|w| w == b"ab").unwrap();
        bytes[at] = 0xff;
        assert_eq!(decode(&bytes).err(), Some(CodecError::Malformed));
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let twice = [one("a", &[], ""), one("b", &[], "")];
        assert_eq!(
            decode(&unchecked(&twice)).err(),
            Some(CodecError::DuplicateId)
        );
        assert_eq!(
            encode(&contents(&twice)).err(),
            Some(CodecError::DuplicateId)
        );
    }

    // Each limit: the largest allowed value passes, one more is refused,
    // by decode (planted input) and by encode (never written).
    #[test]
    fn field_limits() {
        let title = "t".repeat(MAX_TITLE_BYTES);
        let tag = "g".repeat(MAX_TAG_BYTES);
        let body = "b".repeat(MAX_BODY_BYTES);
        let many: Vec<&str> = vec!["x"; MAX_TAGS_PER_NOTE];
        let at_limit = [one(&title, &many, ""), {
            let mut r = one("", &[&tag], &body);
            r.0 = [2; 16];
            r
        }];
        assert_eq!(decode(&encoded(&at_limit)).unwrap().notes.len(), 2);

        let over: [(Raw, CodecError); 4] = [
            (one(&format!("{title}t"), &[], ""), CodecError::FieldTooLong),
            (one("", &[&format!("{tag}g")], ""), CodecError::FieldTooLong),
            (one("", &[], &format!("{body}b")), CodecError::FieldTooLong),
            (
                one("", &[many.as_slice(), &["x"]].concat(), ""),
                CodecError::TooManyTags,
            ),
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
                (id, String::new(), vec![], 0, 0, String::new())
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
        for shown in [err.to_string(), format!("{err:?}")] {
            assert!(
                !shown.contains("Title") && !shown.contains("link"),
                "{shown}"
            );
        }
    }
}
