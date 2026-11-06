//! Note model: notes, titles, links and tags in zeroizing memory, plus the
//! wikilink/backlink index and search.
//!
//! Type-level rule (DESIGN.md, "Plaintext leakage prevention"): note
//! content, including titles and tags, only ever exists as `NoteText`,
//! which implements neither Debug, Display, Serialize nor Clone. The only
//! ways out are the renderer, the editor and the vault codec, which
//! borrows `as_str()` straight into the buffer that gets encrypted.

#![expect(dead_code, reason = "used by the vault codec and unlock (#10, #15)")]

use std::fmt;

use rustix::io::Errno;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::crypto::fill_random;

/// Fails the build if `$ty` implements any of the listed traits; each
/// trait gets its own check. How: `AmbiguousIfImpl` has a blanket impl
/// for every type, and a second one for types that have the trait. With
/// one impl, `<$ty as AmbiguousIfImpl<_>>` resolves; with two, the
/// compiler can't choose and stops with "type annotations needed". Safe
/// code, no crate (the static_assertions technique).
macro_rules! assert_not_impl {
    ($ty:ty: $($tr:path),+ $(,)?) => {
        $(
            const _: fn() = || {
                trait AmbiguousIfImpl<A> {
                    fn some_item() {}
                }
                impl<T: ?Sized> AmbiguousIfImpl<()> for T {}
                struct Invalid;
                impl<T: ?Sized + $tr> AmbiguousIfImpl<Invalid> for T {}
                let _ = <$ty as AmbiguousIfImpl<_>>::some_item;
            };
        )+
    };
}
// Also used for the vault codec's wire types.
pub(crate) use assert_not_impl;

// The type-level rule, checked by the compiler (CLAUDE.md, DESIGN.md).
// Removing any of these lines needs a reason in review.
assert_not_impl!(NoteText: fmt::Debug, fmt::Display, serde::Serialize, Clone);
assert_not_impl!(Note: fmt::Debug, fmt::Display, serde::Serialize, Clone);

// Dropping a Note runs its zeroize (the derive above implements Drop).
const _: fn() = || {
    fn wiped_on_drop<T: ZeroizeOnDrop>() {}
    wiped_on_drop::<Note>();
};

/// Note content: a body, a title or a tag. The buffer is wiped when the
/// value is dropped. Deliberately no Debug, Display, Serialize or Clone:
/// it can't reach a log, an error, a panic message or a `format!` string,
/// and can't be copied into memory that is not wiped.
pub struct NoteText(Zeroizing<String>);

impl NoteText {
    /// Takes over a buffer that is already zeroizing, so the text never
    /// passes through a plain `String`.
    pub fn from_zeroizing(text: Zeroizing<String>) -> Self {
        Self(text)
    }

    /// For the renderer, the editor and the vault codec only.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Wipes the text in place: the whole allocation, not only the bytes in use.
impl Zeroize for NoteText {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

/// A note's id: 16 random bytes, fixed for the note's lifetime. Not secret
/// on its own, but it is never shown either.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Zeroize)]
pub struct NoteId([u8; 16]);

impl NoteId {
    /// A new id for a new note, from the kernel's secure generator.
    pub fn random() -> Result<Self, Errno> {
        let mut bytes = [0; 16];
        fill_random(&mut bytes)?;
        Ok(Self(bytes))
    }

    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// One note. Every field, including the title and the tags, is note
/// content and is wiped on drop. No Debug, Serialize or Clone: the vault
/// codec writes it through a borrowed wire struct.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct Note {
    pub id: NoteId,
    pub title: NoteText,
    pub tags: Vec<NoteText>,
    /// Seconds since the Unix epoch.
    pub created: u64,
    /// Seconds since the Unix epoch.
    pub modified: u64,
    pub body: NoteText,
}

/// The vault's id: 32 random bytes, embedded in vault.age and pinned in
/// config.age, so a vault swapped in from elsewhere is refused.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VaultId([u8; 32]);

impl VaultId {
    /// A new id for a new vault (--init), from the kernel's secure
    /// generator.
    pub fn random() -> Result<Self, Errno> {
        let mut bytes = [0; 32];
        fill_random(&mut bytes)?;
        Ok(Self(bytes))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Shows only the first 4 bytes in hex, enough to tell two vaults apart in
/// a test failure without printing the whole id.
impl fmt::Debug for VaultId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, ..] = self.0;
        write!(f, "VaultId({a:02x}{b:02x}{c:02x}{d:02x}..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> NoteText {
        NoteText::from_zeroizing(Zeroizing::new(s.to_owned()))
    }

    #[test]
    fn note_text_accessors() {
        let t = text("hello");
        assert_eq!(t.as_str(), "hello");
        assert_eq!(t.len(), 5);
        assert!(!t.is_empty());
        assert!(text("").is_empty());
    }

    #[test]
    fn random_ids_differ() {
        assert!(NoteId::random().unwrap() != NoteId::random().unwrap());
        assert_ne!(VaultId::random().unwrap(), VaultId::random().unwrap());
    }

    #[test]
    fn vault_id_debug_shows_only_a_prefix() {
        let mut bytes = [0xab; 32];
        bytes[..4].copy_from_slice(&[0x01, 0x23, 0x45, 0x67]);
        assert_eq!(
            format!("{:?}", VaultId::from_bytes(bytes)),
            "VaultId(01234567..)"
        );
    }

    /// A text whose allocation is 64 bytes larger than the text, and those
    /// 64 bytes still hold old text (`#`), as after deleting text in the
    /// editor. So the checks below fail if the spare part is not wiped.
    fn roomy_text(s: &str) -> NoteText {
        let mut buf = Zeroizing::new(String::with_capacity(s.len() + 64));
        buf.push_str(s);
        buf.push_str(&"#".repeat(64));
        buf.truncate(s.len());
        NoteText::from_zeroizing(buf)
    }

    /// A copy of every byte of `t`'s allocation, also past its length.
    /// Call only after `t.zeroize()`: before that, the bytes past the
    /// length are uninitialized and reading them is undefined behaviour.
    #[allow(unsafe_code, reason = "test-only check of wiped memory")]
    fn wiped_allocation(t: &mut NoteText) -> Vec<u8> {
        // SAFETY:
        // - `as_mut_vec` is only read through; the String's bytes are not
        //   changed, so it stays valid UTF-8.
        // - The allocation is still owned by `t`: zeroize truncates the
        //   String to length 0 but keeps (does not free) its buffer.
        // - Every one of its `capacity` bytes is initialized: zeroize wrote
        //   0 to the text bytes and to the spare capacity.
        // - The pointer comes from the Vec, so its provenance covers the
        //   whole allocation, not only the (now empty) string slice.
        unsafe {
            let vec = t.0.as_mut_vec();
            std::slice::from_raw_parts(vec.as_ptr(), vec.capacity()).to_vec()
        }
    }

    // The same allocation that held the text is overwritten with zeros,
    // including the spare capacity, and is not freed or moved first.
    #[test]
    fn zeroize_wipes_the_whole_text_allocation() {
        let mut t = roomy_text("hunter2 is my secret");
        let before = (t.as_str().as_ptr(), t.0.capacity());
        t.zeroize();
        assert!(t.is_empty());
        assert_eq!(
            (t.as_str().as_ptr(), t.0.capacity()),
            before,
            "zeroize must wipe in place, not reallocate"
        );
        let bytes = wiped_allocation(&mut t);
        assert_eq!(bytes.len(), before.1);
        assert!(bytes.iter().all(|&b| b == 0), "{bytes:?}");
    }

    // `note.zeroize()` is exactly what Drop runs (ZeroizeOnDrop, checked
    // at compile time above), so this shows dropping a Note wipes every
    // field. Tags are wiped one by one like the title (the test above),
    // then freed by Vec::clear, so their memory can't be read afterwards.
    #[test]
    fn zeroize_wipes_every_field_of_a_note() {
        let mut note = Note {
            id: NoteId::from_bytes([0x5a; 16]),
            title: roomy_text("secret title"),
            tags: vec![roomy_text("tag-a"), roomy_text("tag-b")],
            created: 1_700_000_000,
            modified: 1_700_000_001,
            body: roomy_text(&"secret body ".repeat(50)),
        };
        note.zeroize();
        for t in [&mut note.title, &mut note.body] {
            assert!(t.is_empty());
            assert!(wiped_allocation(t).iter().all(|&b| b == 0));
        }
        assert!(note.tags.is_empty());
        assert_eq!(note.id.as_bytes(), &[0; 16]);
        assert_eq!((note.created, note.modified), (0, 0));
    }
}
