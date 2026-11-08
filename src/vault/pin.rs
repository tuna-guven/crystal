//! Vault pins (docs/decisions/0032-fixed-locations.md): which vault a name
//! stands for on this device. `~/.local/state/crystal/NAME.pin` holds the
//! vault's `vault_id` in 64 lowercase hex digits and a newline, written
//! through the atomic writer (`PublicBytes::pin`).
//!
//! `crystal init` writes it; on a device without one (a vault synced or
//! restored here), the first unlock writes it. After that, a vault whose
//! `vault_id` differs is refused, so one of your vaults cannot be swapped
//! in for another under its name. Like the generation counters, it is
//! plaintext that a process running as you can change (A5). A malformed
//! pin fails closed.

use std::fmt;
use std::os::fd::OwnedFd;

use rustix::fs::{AtFlags, unlinkat};
use rustix::io::Errno;

use super::VaultName;
use super::atomic::{FileName, Payload, PublicBytes, Step, WriteError, atomic_write};
use super::public_file::{ReadError, read_bounded};
use crate::model::VaultId;

/// 64 hex digits and a newline.
const PIN_LEN: usize = 65;

/// Whether the vault was already pinned to this name on this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pinned {
    /// The pin matches.
    Known,
    /// There was no pin; this vault is now pinned to the name.
    New,
}

/// Fixed text and errnos; never a name or a vault id.
#[derive(Debug, PartialEq, Eq)]
pub enum PinError {
    /// The vault's `vault_id` is not the one pinned to its name.
    Mismatch,
    /// The pin file is not 64 lowercase hex digits and a newline.
    Corrupt,
    Read(ReadError),
    Write(WriteError),
    Remove(Errno),
}

impl fmt::Display for PinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mismatch => write!(
                f,
                "this is not the vault this name was pinned to on this device (its vault_id \
                 differs): refusing a swapped vault. If you replaced it on purpose, delete its \
                 .pin file in ~/.local/state/crystal"
            ),
            Self::Corrupt => write!(
                f,
                "the vault's .pin file in ~/.local/state/crystal is malformed; refusing to guess"
            ),
            Self::Read(e) => write!(f, "cannot read the vault's pin ({e})"),
            Self::Write(e) => write!(f, "cannot write the vault's pin: {e}"),
            Self::Remove(errno) => write!(f, "cannot remove the vault's pin ({errno})"),
        }
    }
}

impl std::error::Error for PinError {}

/// `NAME.pin`. A vault name is 1 to 64 characters of `A-Z a-z 0-9 - _`,
/// starting with a letter or digit, so this is always a valid FileName.
fn file_name(name: &VaultName) -> Result<FileName, PinError> {
    FileName::new(&format!("{}.pin", name.as_str())).ok_or(PinError::Write(WriteError {
        step: Step::Open,
        errno: Errno::INVAL,
    }))
}

/// The pinned vault id, or `None` if this device has no pin for `name`.
pub fn load(state: &OwnedFd, name: &VaultName) -> Result<Option<VaultId>, PinError> {
    let file = file_name(name)?;
    let Some(bytes) = read_bounded(state, file.as_str(), PIN_LEN).map_err(PinError::Read)? else {
        return Ok(None);
    };
    parse(&bytes).map(Some).ok_or(PinError::Corrupt)
}

/// Exactly 64 lowercase hex digits and a newline: one valid spelling.
fn parse(bytes: &[u8]) -> Option<VaultId> {
    let hex = bytes.strip_suffix(b"\n").filter(|h| h.len() == 64)?;
    let digit = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let mut id = [0u8; 32];
    let (pairs, _) = hex.as_chunks::<2>();
    for (byte, [high, low]) in id.iter_mut().zip(pairs) {
        *byte = digit(*high)? << 4 | digit(*low)?;
    }
    Some(VaultId::from_bytes(id))
}

/// Pins `vault_id` to `name`, replacing any pin.
pub fn store(state: &OwnedFd, name: &VaultName, vault_id: &VaultId) -> Result<(), PinError> {
    let file = file_name(name)?;
    atomic_write(state, &file, Payload::Public(&PublicBytes::pin(vault_id)))
        .map_err(PinError::Write)
}

/// After a successful decrypt: the vault must be the one pinned to its
/// name; without a pin, it is pinned now.
pub fn check(state: &OwnedFd, name: &VaultName, vault_id: &VaultId) -> Result<Pinned, PinError> {
    match load(state, name)? {
        Some(pinned) if pinned == *vault_id => Ok(Pinned::Known),
        Some(_) => Err(PinError::Mismatch),
        None => {
            store(state, name, vault_id)?;
            Ok(Pinned::New)
        }
    }
}

/// For `crystal rm`: forgets the pin. A missing pin is fine.
pub fn remove(state: &OwnedFd, name: &VaultName) -> Result<(), PinError> {
    let file = file_name(name)?;
    match unlinkat(state, file.as_str(), AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(errno) => Err(PinError::Remove(errno)),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};

    use rustix::fs::{CWD, Mode, OFlags, openat};

    use super::*;

    fn state(test: &str) -> (PathBuf, OwnedFd) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/unit-homes")
            .join(format!("pin_{test}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let fd = openat(
            CWD,
            &dir,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        (dir, fd)
    }

    fn name(s: &str) -> VaultName {
        VaultName::new(OsString::from(s)).unwrap()
    }

    const A: VaultId = VaultId::from_bytes([0xab; 32]);
    const B: VaultId = VaultId::from_bytes([0x01; 32]);

    #[test]
    fn first_check_pins_then_matches() {
        let (dir, fd) = state("pins");
        assert_eq!(check(&fd, &name("diary"), &A), Ok(Pinned::New));
        assert_eq!(
            fs::read(dir.join("diary.pin")).unwrap(),
            format!("{}\n", "ab".repeat(32)).as_bytes()
        );
        assert_eq!(check(&fd, &name("diary"), &A), Ok(Pinned::Known));
        // Another name has its own pin.
        assert_eq!(check(&fd, &name("work"), &B), Ok(Pinned::New));
    }

    #[test]
    fn refuses_another_vault_under_a_pinned_name() {
        let (_dir, fd) = state("mismatch");
        store(&fd, &name("diary"), &A).unwrap();
        assert_eq!(check(&fd, &name("diary"), &B), Err(PinError::Mismatch));
        // The pin is not changed by a refused check.
        assert_eq!(load(&fd, &name("diary")), Ok(Some(A)));
    }

    #[test]
    fn malformed_pins_fail_closed() {
        let (dir, fd) = state("corrupt");
        let good = format!("{}\n", "ab".repeat(32));
        let bad = [
            String::new(),
            "ab".repeat(32),
            good.to_uppercase(),
            format!("{}\n", "ab".repeat(31)),
            format!("{}\n", "zz".repeat(32)),
            format!("{good}\n"),
            format!(" {good}"),
        ];
        for content in bad {
            fs::write(dir.join("diary.pin"), &content).unwrap();
            assert_eq!(
                check(&fd, &name("diary"), &A),
                Err(PinError::Corrupt),
                "{content:?}"
            );
        }
    }

    #[test]
    fn remove_forgets_the_pin() {
        let (dir, fd) = state("remove");
        store(&fd, &name("diary"), &A).unwrap();
        remove(&fd, &name("diary")).unwrap();
        assert!(!dir.join("diary.pin").exists());
        remove(&fd, &name("diary")).unwrap();
        assert_eq!(check(&fd, &name("diary"), &B), Ok(Pinned::New));
    }

    #[test]
    fn errors_never_show_the_id_or_name() {
        let errors = [
            PinError::Mismatch,
            PinError::Corrupt,
            PinError::Remove(Errno::ACCESS),
        ];
        for e in errors {
            let shown = e.to_string();
            assert!(
                !shown.contains("abab") && !shown.contains("diary"),
                "{shown}"
            );
        }
    }
}
