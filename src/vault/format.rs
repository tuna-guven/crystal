//! The vault file format (docs/design/storage.md): `vault.age` is
//! encode, then Padmé-pad, then encrypt to the vault recipient; `VERSION`
//! holds the format number in plain text so an unknown format is refused
//! before anything is decrypted.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "`crystal init` (#14) and unlock (#15)")
)]

use std::fmt;
use std::os::fd::OwnedFd;

use rustix::io::Errno;

use super::atomic::{FileName, Payload, PublicBytes, WriteError, atomic_write};
use super::codec::{self, CodecError, FORMAT, OLDEST_FORMAT, VaultContents};
use super::padme::{self, PadError};
use super::public_file::{ReadError, parse_decimal_line, read_bounded};
use crate::age_v1::x25519;
use crate::crypto::{self, Ciphertext, CryptoError, VaultIdentity};

/// Why sealing or opening `vault.age` failed. Each part holds only a fixed
/// variant, never content.
#[derive(Debug, PartialEq, Eq)]
pub enum VaultError {
    Codec(CodecError),
    Padding(PadError),
    Crypto(CryptoError),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(e) => e.fmt(f),
            Self::Padding(e) => e.fmt(f),
            Self::Crypto(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for VaultError {}

/// The bytes of `vault.age`: encode, pad, encrypt to `recipient`. Every
/// plaintext stage lives in a zeroizing buffer of exact size.
pub fn seal(
    contents: &VaultContents,
    recipient: &x25519::Recipient,
) -> Result<Ciphertext, VaultError> {
    let encoded = codec::encode(contents).map_err(VaultError::Codec)?;
    let padded = padme::pad(&encoded).map_err(VaultError::Padding)?;
    crypto::encrypt_to_recipients(&padded, std::slice::from_ref(recipient))
        .map_err(VaultError::Crypto)
}

/// The vault back from `vault.age`: decrypt, unpad, decode. age
/// authenticates the whole file, so a changed byte anywhere fails at
/// decryption, before padding or contents are looked at. `version` is the
/// vault's `VERSION`: a blob in a newer format is refused
/// (docs/decisions/0034-vault-format-2.md).
pub fn open(
    ciphertext: &[u8],
    identity: &VaultIdentity,
    version: u16,
) -> Result<VaultContents, VaultError> {
    let padded = crypto::decrypt_with_identity(ciphertext, identity).map_err(VaultError::Crypto)?;
    let encoded = padme::unpad(&padded).map_err(VaultError::Padding)?;
    codec::decode_at_most(encoded, version).map_err(VaultError::Codec)
}

// The vault directory's files (docs/design/storage.md, "Key design"),
// named once for every module that reads or writes them.
pub const VERSION_FILE: &str = "VERSION";
pub const IDENTITY_FILE: &str = "identity.age";
pub const IDENTITY_BACKUP_FILE: &str = "identity.backup.age";
pub const VAULT_FILE: &str = "vault.age";
/// `identity.age` holds one age secret key (74 bytes) under one scrypt
/// stanza: far below this.
pub const MAX_IDENTITY_FILE: usize = 64 << 10;

/// The longest valid `VERSION`: `65535\n`.
const MAX_VERSION_LEN: usize = 6;

/// Why `VERSION` was refused. Fixed text and errnos only.
#[derive(Debug, PartialEq, Eq)]
pub enum VersionError {
    /// No `VERSION` in the directory: not a vault (#12 also uses this to
    /// refuse a config pointing at, say, `~/.ssh`).
    Missing,
    /// A symlink, directory, FIFO, device or socket.
    NotARegularFile,
    Open(Errno),
    Stat(Errno),
    Read(Errno),
    /// Anything but ASCII digits (no sign, no leading zero) and one `\n`.
    Malformed,
    /// A well-formed number this build does not read.
    Unknown,
}

impl fmt::Display for VersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(f, "the vault has no VERSION file"),
            Self::NotARegularFile => write!(f, "the vault's VERSION is not a regular file"),
            Self::Open(errno) => write!(f, "cannot open the vault's VERSION ({errno})"),
            Self::Stat(errno) => write!(f, "cannot check the vault's VERSION ({errno})"),
            Self::Read(errno) => write!(f, "cannot read the vault's VERSION ({errno})"),
            Self::Malformed => write!(f, "the vault's VERSION is malformed"),
            Self::Unknown => write!(
                f,
                "the vault uses a format this version of Crystal cannot read"
            ),
        }
    }
}

impl std::error::Error for VersionError {}

/// Parses `VERSION` strictly (`public_file::parse_decimal_line`: one
/// spelling per number). It names the newest blob format the directory
/// may hold (docs/decisions/0034-vault-format-2.md), so every format from
/// the oldest this build reads up to `FORMAT` is accepted. Returns it.
pub fn check_version(bytes: &[u8]) -> Result<u16, VersionError> {
    let number = parse_decimal_line(bytes)
        .and_then(|n| u16::try_from(n).ok())
        .ok_or(VersionError::Malformed)?;
    if (OLDEST_FORMAT..=FORMAT).contains(&number) {
        Ok(number)
    } else {
        Err(VersionError::Unknown)
    }
}

/// Reads and checks `VERSION` in the vault directory `dir`: first, before
/// any decryption. Regular file only, at most a few bytes, whatever the
/// file's size (`public_file::read_bounded`). Returns the number.
pub fn read_version(dir: &OwnedFd) -> Result<u16, VersionError> {
    let bytes = read_bounded(dir, VERSION_FILE, MAX_VERSION_LEN)
        .map_err(|e| match e {
            ReadError::NotARegularFile => VersionError::NotARegularFile,
            ReadError::Open(errno) => VersionError::Open(errno),
            ReadError::Stat(errno) => VersionError::Stat(errno),
            ReadError::Read(errno) => VersionError::Read(errno),
        })?
        .ok_or(VersionError::Missing)?;
    check_version(&bytes)
}

/// Writes `VERSION` (for `crystal init`) through the atomic writer.
pub fn write_version(dir: &OwnedFd) -> Result<(), WriteError> {
    // A fixed, valid name: FileName::new cannot refuse it.
    let name = FileName::new(VERSION_FILE).ok_or(WriteError {
        step: super::atomic::Step::Open,
        errno: Errno::INVAL,
    })?;
    atomic_write(dir, &name, Payload::Public(&PublicBytes::version()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Note, NoteId, NoteText, VaultId};
    use proptest::prelude::*;
    use rustix::fs::{FileType, Mode, OFlags, openat};
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use zeroize::Zeroizing;

    fn text(s: &str) -> NoteText {
        NoteText::from_zeroizing(Zeroizing::new(s.to_owned()))
    }

    fn vault(notes: &[(u8, &str, &str)]) -> VaultContents {
        VaultContents {
            vault_id: VaultId::from_bytes([9; 32]),
            generation: 5,
            notes: notes
                .iter()
                .map(|(id, title, body)| Note {
                    id: NoteId::from_bytes([*id; 16]),
                    folder: text("work"),
                    title: text(title),
                    created: 1,
                    modified: 2,
                    body: text(body),
                })
                .collect(),
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        // Acceptance: the round trip holds through the full pipeline.
        #[test]
        fn seal_open_round_trip(
            generation in any::<u64>(),
            notes in prop::collection::vec(("[a-z][a-z0-9]{0,19}", any::<String>()), 0..6),
        ) {
            let identity = VaultIdentity::generate().unwrap();
            let contents = VaultContents {
                vault_id: VaultId::from_bytes([3; 32]),
                generation,
                notes: notes
                    .iter()
                    .enumerate()
                    .map(|(i, (title, body))| Note {
                        id: NoteId::from_bytes([i as u8; 16]),
                        // Unique names whatever titles were drawn.
                        folder: text(&format!("f{i}")),
                        title: text(title),
                        created: 0,
                        modified: 0,
                        body: text(body),
                    })
                    .collect(),
            };
            let sealed = seal(&contents, &identity.recipient()).unwrap();
            let back = open(sealed.as_bytes(), &identity, FORMAT).unwrap();
            prop_assert_eq!(back.vault_id, contents.vault_id);
            prop_assert_eq!(back.generation, generation);
            prop_assert_eq!(back.notes.len(), notes.len());
            for (n, (title, body)) in back.notes.iter().zip(&notes) {
                prop_assert_eq!(n.title.as_str(), title.as_str());
                prop_assert_eq!(n.body.as_str(), body.as_str());
            }
        }
    }

    #[test]
    fn open_refuses_other_keys_and_tampering() {
        let identity = VaultIdentity::generate().unwrap();
        let sealed = seal(&vault(&[(1, "T", "secret body")]), &identity.recipient()).unwrap();
        let other = VaultIdentity::generate().unwrap();
        let err = open(sealed.as_bytes(), &other, FORMAT).err();
        assert_eq!(err, Some(VaultError::Crypto(CryptoError::NotForThisKey)));

        let mut bad = sealed.as_bytes().to_vec();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        let err = open(&bad, &identity, FORMAT).err();
        assert_eq!(err, Some(VaultError::Crypto(CryptoError::Corrupt)));
        let err = open(&bad[..last], &identity, FORMAT).err();
        assert_eq!(err, Some(VaultError::Crypto(CryptoError::Corrupt)));
    }

    // Seal refuses what decode would refuse, so no unreadable vault is
    // ever written.
    #[test]
    fn seal_refuses_invalid_contents() {
        let identity = VaultIdentity::generate().unwrap();
        let err = seal(&vault(&[(1, "a", ""), (1, "b", "")]), &identity.recipient()).err();
        assert_eq!(err, Some(VaultError::Codec(CodecError::DuplicateId)));
    }

    /// The part of an age file after the header: nonce and encrypted
    /// chunks. Its size follows the plaintext size exactly.
    fn payload(file: &[u8]) -> &[u8] {
        let mac = file.windows(5).position(|w| w == b"\n--- ").unwrap() + 1;
        let end = mac + file[mac..].iter().position(|b| *b == b'\n').unwrap() + 1;
        &file[end..]
    }

    // The payload size depends only on the Padmé bucket: two vaults whose
    // encodings differ by a few bytes give the same size. The header has a
    // fixed size too (one X25519 stanza, no random "grease" stanza since
    // decision 0042), so the whole file is the same size.
    #[test]
    fn sealed_size_hides_small_differences() {
        let identity = VaultIdentity::generate().unwrap();
        let body = "x".repeat(5000);
        let a = seal(&vault(&[(1, "T", &body)]), &identity.recipient()).unwrap();
        let b = seal(&vault(&[(1, "Title", &body)]), &identity.recipient()).unwrap();
        assert_eq!(payload(a.as_bytes()).len(), payload(b.as_bytes()).len());
        // 5120-byte Padmé frame: 16-byte nonce, one chunk plus its 16-byte tag.
        assert_eq!(payload(a.as_bytes()).len(), 16 + 5120 + 16);
        assert_eq!(a.as_bytes().len(), b.as_bytes().len());
    }

    #[test]
    fn version_text_is_strict() {
        assert_eq!(PublicBytes::version().as_bytes(), b"2\n");
        // VERSION names the newest format allowed: 1 and 2 are read.
        assert_eq!(check_version(b"1\n"), Ok(1));
        assert_eq!(check_version(b"2\n"), Ok(2));
        for unknown in [&b"0\n"[..], b"3\n", b"65535\n"] {
            assert_eq!(
                check_version(unknown),
                Err(VersionError::Unknown),
                "{unknown:?}"
            );
        }
        let malformed = [
            &b""[..],
            b"1",
            b"\n",
            b"01\n",
            b"+1\n",
            b"-1\n",
            b" 1\n",
            b"1 \n",
            b"1\r\n",
            b"1\n\n",
            b"1\nx",
            b"65536\n",
            b"1.0\n",
        ];
        for bad in malformed {
            assert_eq!(check_version(bad), Err(VersionError::Malformed), "{bad:?}");
        }
    }

    /// A fresh, empty directory under target/ and an fd for it.
    fn fresh_dir(test: &str) -> (PathBuf, OwnedFd) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/unit-format")
            .join(test);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let fd = openat(rustix::fs::CWD, &path, flags, Mode::empty()).unwrap();
        (path, fd)
    }

    #[test]
    fn version_file_round_trip() {
        let (path, dir) = fresh_dir("version_ok");
        assert_eq!(read_version(&dir), Err(VersionError::Missing));
        write_version(&dir).unwrap();
        assert_eq!(fs::read(path.join("VERSION")).unwrap(), b"2\n");
        let mode = fs::metadata(path.join("VERSION"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o600);
        assert_eq!(read_version(&dir), Ok(FORMAT));
    }

    #[test]
    fn version_file_must_be_a_small_regular_file() {
        let (path, dir) = fresh_dir("version_bad");
        let version = path.join("VERSION");

        fs::write(path.join("real"), b"1\n").unwrap();
        symlink(path.join("real"), &version).unwrap();
        assert_eq!(read_version(&dir), Err(VersionError::NotARegularFile));
        fs::remove_file(&version).unwrap();

        fs::create_dir(&version).unwrap();
        assert_eq!(read_version(&dir), Err(VersionError::NotARegularFile));
        fs::remove_dir(&version).unwrap();

        // A FIFO would block a plain open forever; it is refused at once.
        rustix::fs::mknodat(&dir, "VERSION", FileType::Fifo, Mode::RUSR | Mode::WUSR, 0).unwrap();
        assert_eq!(read_version(&dir), Err(VersionError::NotARegularFile));
        fs::remove_file(&version).unwrap();

        fs::write(&version, [b"1\n".as_slice(), &[b'0'; 4096]].concat()).unwrap();
        assert_eq!(read_version(&dir), Err(VersionError::Malformed));

        fs::write(&version, b"3\n").unwrap();
        assert_eq!(read_version(&dir), Err(VersionError::Unknown));
    }

    #[test]
    fn errors_are_fixed_text() {
        let identity = VaultIdentity::generate().unwrap();
        let sealed = seal(
            &vault(&[(1, "Title", "secret body")]),
            &identity.recipient(),
        )
        .unwrap();
        let err = open(&sealed.as_bytes()[..40], &identity, FORMAT)
            .err()
            .unwrap();
        for shown in [err.to_string(), format!("{err:?}")] {
            assert!(
                !shown.contains("secret") && !shown.contains("Title"),
                "{shown}"
            );
        }
    }
}
