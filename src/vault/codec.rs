//! Notes to bytes and back (docs/design/storage.md, "Vault contents"), with postcard.
//!
//! `NoteText` has no `Serialize`, so the notes go through `WireNote`, a
//! private struct that only borrows `&str` from them. Encoding writes into
//! one zeroizing buffer of the exact final size, so it never grows and
//! leaves no partial copies behind. Decoding borrows `&str` straight from
//! the decrypted buffer and copies each one once, into its own
//! `Zeroizing<String>`.
//!
//! This is only the list of notes; the vault framing around it (format
//! version, vault_id, generation, Padmé padding) is #10.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "called by the vault format (#10)")
)]

use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::model::{Note, NoteId, NoteText, assert_not_impl};

/// One note as postcard sees it: borrowed text, nothing owned. Field order
/// is the on-disk order; changing it needs a new format version (#10).
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

/// One copy, into an allocation of exactly the right size.
fn text(s: &str) -> NoteText {
    NoteText::from_zeroizing(Zeroizing::new(s.to_owned()))
}

/// Fixed messages only: never a byte or a note from the input.
#[derive(Debug, PartialEq, Eq)]
pub enum CodecError {
    Encode,
    Malformed,
    TrailingBytes,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Encode => "cannot encode the notes",
            Self::Malformed => "vault contents are malformed",
            Self::TrailingBytes => "vault contents have unexpected trailing bytes",
        })
    }
}

impl std::error::Error for CodecError {}

/// The notes as postcard bytes, in a zeroizing buffer of the exact size.
pub fn encode_notes(notes: &[Note]) -> Result<Zeroizing<Vec<u8>>, CodecError> {
    let wire: Vec<WireNote<'_>> = notes.iter().map(WireNote::borrow).collect();
    let size = postcard::serialize_with_flavor(&wire, postcard::ser_flavors::Size::default())
        .map_err(|_| CodecError::Encode)?;
    let mut out = Zeroizing::new(vec![0; size]);
    let used = postcard::to_slice(&wire, &mut out)
        .map_err(|_| CodecError::Encode)?
        .len();
    if used != size {
        return Err(CodecError::Encode);
    }
    Ok(out)
}

/// The notes back from postcard bytes. Fails closed: malformed input
/// (including invalid UTF-8 and a cut-off encoding) and any byte left over
/// after the last note are refused.
pub fn decode_notes(bytes: &[u8]) -> Result<Vec<Note>, CodecError> {
    let (wire, rest): (Vec<WireNote<'_>>, _) =
        postcard::take_from_bytes(bytes).map_err(|_| CodecError::Malformed)?;
    if !rest.is_empty() {
        return Err(CodecError::TrailingBytes);
    }
    Ok(wire.iter().map(WireNote::to_note).collect())
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

    fn encode(raw: &[Raw]) -> Zeroizing<Vec<u8>> {
        encode_notes(&raw.iter().map(note).collect::<Vec<_>>()).unwrap()
    }

    proptest! {
        // docs/development/testing.md "Round-trip properties": any notes, including empty and
        // non-ASCII text, come back exactly as they went in.
        #[test]
        fn round_trip(raw in prop::collection::vec(
            (
                any::<[u8; 16]>(),
                any::<String>(),
                prop::collection::vec(any::<String>(), 0..4),
                any::<u64>(),
                any::<u64>(),
                any::<String>(),
            ),
            0..8,
        )) {
            let back = decode_notes(&encode(&raw)).unwrap();
            prop_assert_eq!(back.len(), raw.len());
            for (n, r) in back.iter().zip(&raw) {
                prop_assert!(same(n, r));
            }
        }
    }

    #[test]
    fn empty_vault_round_trips() {
        let bytes = encode(&[]);
        assert!(decode_notes(&bytes).unwrap().is_empty());
    }

    // Every cut-off version of an encoding is refused, never read as a
    // shorter but valid list.
    #[test]
    fn truncated_input_is_refused() {
        let bytes = encode(&sample());
        for len in 0..bytes.len() {
            let err = decode_notes(&bytes[..len]).err();
            assert_eq!(err, Some(CodecError::Malformed), "length {len}");
        }
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let mut bytes = encode(&sample()).to_vec();
        bytes.push(0);
        assert_eq!(decode_notes(&bytes).err(), Some(CodecError::TrailingBytes));
    }

    #[test]
    fn invalid_utf8_is_refused() {
        let raw = vec![([0; 16], "ab".into(), vec![], 0, 0, String::new())];
        let mut bytes = encode(&raw).to_vec();
        let at = bytes.windows(2).position(|w| w == b"ab").unwrap();
        bytes[at] = 0xff;
        assert_eq!(decode_notes(&bytes).err(), Some(CodecError::Malformed));
    }

    // Errors carry no input: neither note text nor bytes.
    #[test]
    fn errors_never_show_content() {
        let mut bytes = encode(&sample()).to_vec();
        bytes.push(b'x');
        let err = decode_notes(&bytes).err().unwrap();
        for shown in [err.to_string(), format!("{err:?}")] {
            assert!(
                !shown.contains("Title") && !shown.contains("link"),
                "{shown}"
            );
        }
    }
}
