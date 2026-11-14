//! Rollback detection (docs/design/storage.md, "Rollback"): the last
//! generation this device has seen, per vault, kept in the local state
//! directory as `<vault_id hex>.gen`.
//!
//! Each file is a decimal number and a newline, written through the atomic
//! writer (`PublicBytes::generation`). It is plaintext by design; a process
//! running as you can lower it, which the threat model accepts (A5,
//! docs/decisions/0019-generation-counters.md). A malformed file fails
//! closed: Crystal never resets a counter it cannot read.

use std::fmt;
use std::os::fd::OwnedFd;

use super::atomic::{FileName, Payload, PublicBytes, Step, WriteError, atomic_write};
use super::public_file::{ReadError, parse_decimal_line, read_bounded};
use crate::model::VaultId;

/// Which counter: one per vault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Counter {
    Vault(VaultId),
}

impl Counter {
    /// The vault id in lowercase hex plus `.gen`.
    fn file_name(&self) -> String {
        let Self::Vault(id) = self;
        format!("{}.gen", id.hex())
    }

    /// For messages: never the id.
    fn what(&self) -> &'static str {
        "vault"
    }
}

/// `u64::MAX` is 20 digits, plus the newline.
const MAX_GEN_LEN: usize = 21;

/// How an accepted open relates to what this device has seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    /// No counter yet: the first unlock of this vault on this device. Rollback detection starts from here; the caller tells the
    /// user.
    FirstUse,
    /// The same or a newer generation than last time.
    Known,
}

/// Fixed text plus generation numbers and errnos; never a vault id.
#[derive(Debug, PartialEq, Eq)]
pub enum GenError {
    /// The file is older than the last one seen here: a possible rollback.
    Rollback {
        what: &'static str,
        seen: u64,
        stored: u64,
    },
    /// The counter file is not a decimal number and a newline.
    Corrupt { what: &'static str },
    Read {
        what: &'static str,
        error: ReadError,
    },
    Write {
        what: &'static str,
        error: WriteError,
    },
}

impl fmt::Display for GenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rollback { what, seen, stored } => write!(
                f,
                "the {what} is older than the last one this device has seen \
                 (generation {seen}, last seen {stored}): refusing a possible rollback"
            ),
            Self::Corrupt { what } => write!(
                f,
                "the {what}'s generation file in ~/.local/state/crystal is malformed; \
                 refusing to guess (see docs/design/storage.md, Rollback)"
            ),
            Self::Read { what, error } => {
                write!(f, "cannot read the {what}'s generation file ({error})")
            }
            Self::Write { what, error } => {
                write!(f, "cannot record the {what}'s generation: {error}")
            }
        }
    }
}

impl std::error::Error for GenError {}

/// The rule: a generation lower than the stored one is refused; equal or
/// higher is accepted; no stored value is the first use on this device.
pub fn check(counter: Counter, seen: u64, stored: Option<u64>) -> Result<Seen, GenError> {
    match stored {
        None => Ok(Seen::FirstUse),
        Some(stored) if seen < stored => Err(GenError::Rollback {
            what: counter.what(),
            seen,
            stored,
        }),
        Some(_) => Ok(Seen::Known),
    }
}

/// The stored generation, or `None` if this device has none yet.
pub fn load(state: &OwnedFd, counter: Counter) -> Result<Option<u64>, GenError> {
    let what = counter.what();
    let Some(bytes) = read_bounded(state, &counter.file_name(), MAX_GEN_LEN)
        .map_err(|error| GenError::Read { what, error })?
    else {
        return Ok(None);
    };
    parse_decimal_line(&bytes)
        .map(Some)
        .ok_or(GenError::Corrupt { what })
}

/// Replaces the stored generation, atomically.
pub fn store(state: &OwnedFd, counter: Counter, generation: u64) -> Result<(), GenError> {
    let what = counter.what();
    // Always a valid FileName: 64 lowercase hex digits and ".gen".
    let name = FileName::new(&counter.file_name()).ok_or(GenError::Write {
        what,
        error: WriteError {
            step: Step::Open,
            errno: rustix::io::Errno::INVAL,
        },
    })?;
    let bytes = PublicBytes::generation(generation);
    atomic_write(state, &name, Payload::Public(&bytes))
        .map_err(|error| GenError::Write { what, error })
}

/// For `crystal rm`: forgets the counter. A missing counter is fine.
pub fn remove(state: &OwnedFd, counter: Counter) -> Result<(), rustix::io::Errno> {
    match rustix::fs::unlinkat(
        state,
        counter.file_name().as_str(),
        rustix::fs::AtFlags::empty(),
    ) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(errno) => Err(errno),
    }
}

/// An opened vault is older than the newest one this device has seen:
/// restored from a backup, or put back by someone else. Only the user can
/// tell (docs/decisions/0036-backups-external.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Older {
    pub seen: u64,
    pub stored: u64,
}

/// After a successful decrypt, for an open the user can still confirm: as
/// `accept_on_open`, but an older vault is reported as `Older` and the
/// counter is left as it is.
pub fn open_or_older(
    state: &OwnedFd,
    counter: Counter,
    seen: u64,
) -> Result<Result<Seen, Older>, GenError> {
    match accept_on_open(state, counter, seen) {
        Ok(result) => Ok(Ok(result)),
        Err(GenError::Rollback { seen, stored, .. }) => Ok(Err(Older { seen, stored })),
        Err(e) => Err(e),
    }
}

/// The user typed `restore` for an `Older` vault: the counter goes back to
/// its generation. The only way a counter is ever lowered.
pub fn restore(state: &OwnedFd, counter: Counter, older: Older) -> Result<(), GenError> {
    store(state, counter, older.seen)
}

/// After a successful decrypt: refuse a rollback, else remember the
/// highest generation seen. A corrupt counter is refused, never reset.
pub fn accept_on_open(state: &OwnedFd, counter: Counter, seen: u64) -> Result<Seen, GenError> {
    let stored = load(state, counter)?;
    let result = check(counter, seen, stored)?;
    // check() passed, so `seen` is the maximum of the two.
    if stored != Some(seen) {
        store(state, counter, seen)?;
    }
    Ok(result)
}

/// After a save, once the rename of the new file has committed: remember
/// its generation. A crash before this runs only leaves the counter one
/// behind the file, which the next open accepts (higher is fine) and
/// catches up. A saved generation below the stored one means another
/// writer got ahead; that is reported, and the counter is not lowered.
pub fn record_save(state: &OwnedFd, counter: Counter, saved: u64) -> Result<(), GenError> {
    match load(state, counter)? {
        Some(stored) if saved < stored => Err(GenError::Rollback {
            what: counter.what(),
            seen: saved,
            stored,
        }),
        Some(stored) if saved == stored => Ok(()),
        _ => store(state, counter, saved),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::fs::{Mode, OFlags, openat};
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    const VAULT: Counter = Counter::Vault(VaultId::from_bytes([0xab; 32]));

    fn fresh_dir(test: &str) -> (PathBuf, OwnedFd) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/unit-generation")
            .join(test);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let fd = openat(rustix::fs::CWD, &path, flags, Mode::empty()).unwrap();
        (path, fd)
    }

    fn vault_file(path: &Path) -> PathBuf {
        path.join(format!("{}.gen", "ab".repeat(32)))
    }

    #[test]
    fn check_rule() {
        assert_eq!(check(VAULT, 0, None), Ok(Seen::FirstUse));
        assert_eq!(check(VAULT, 5, Some(5)), Ok(Seen::Known));
        assert_eq!(check(VAULT, 6, Some(5)), Ok(Seen::Known));
        let err = check(VAULT, 4, Some(5)).unwrap_err();
        assert_eq!(
            err,
            GenError::Rollback {
                what: "vault",
                seen: 4,
                stored: 5
            }
        );
    }

    // Acceptance: the refusal names both generations, and nothing else
    // (no vault id).
    #[test]
    fn rollback_message_names_the_generations() {
        let shown = check(VAULT, 3, Some(7)).unwrap_err().to_string();
        assert_eq!(
            shown,
            "the vault is older than the last one this device has seen \
             (generation 3, last seen 7): refusing a possible rollback"
        );
        assert!(!shown.contains("ab"), "{shown}");
    }

    #[test]
    fn file_names_and_format() {
        let (path, state) = fresh_dir("names");
        store(&state, VAULT, 42).unwrap();
        assert_eq!(fs::read(vault_file(&path)).unwrap(), b"42\n");
        let mode = fs::metadata(vault_file(&path))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o600);
        assert_eq!(load(&state, VAULT), Ok(Some(42)));

        // Another vault has its own counter.
        let other = Counter::Vault(VaultId::from_bytes([1; 32]));
        assert_eq!(load(&state, other), Ok(None));
        store(&state, VAULT, u64::MAX).unwrap();
        assert_eq!(load(&state, VAULT), Ok(Some(u64::MAX)));
    }

    #[test]
    fn open_stores_the_highest_generation_seen() {
        let (_, state) = fresh_dir("open");
        assert_eq!(accept_on_open(&state, VAULT, 5), Ok(Seen::FirstUse));
        assert_eq!(load(&state, VAULT), Ok(Some(5)));
        assert_eq!(accept_on_open(&state, VAULT, 5), Ok(Seen::Known));
        assert_eq!(accept_on_open(&state, VAULT, 9), Ok(Seen::Known));
        assert_eq!(load(&state, VAULT), Ok(Some(9)));

        // Acceptance: an older vault is refused, and the counter stays.
        let err = accept_on_open(&state, VAULT, 8).unwrap_err();
        assert_eq!(
            err,
            GenError::Rollback {
                what: "vault",
                seen: 8,
                stored: 9
            }
        );
        assert_eq!(load(&state, VAULT), Ok(Some(9)));
    }

    // The save order is crash-safe: the vault rename commits, then the
    // counter is written. A crash in between leaves the counter one behind
    // the vault; the next open accepts it and catches up.
    #[test]
    fn crash_between_vault_rename_and_counter_write() {
        let (_, state) = fresh_dir("crash");
        accept_on_open(&state, VAULT, 5).unwrap();
        // Save of generation 6: vault.age renamed, then the process dies
        // before record_save(6).
        assert_eq!(accept_on_open(&state, VAULT, 6), Ok(Seen::Known));
        assert_eq!(load(&state, VAULT), Ok(Some(6)));
        // A crash before the rename leaves generation 6 on disk: fine too.
        assert_eq!(accept_on_open(&state, VAULT, 6), Ok(Seen::Known));
    }

    // An older vault is reported, not refused, and changes nothing until
    // the user restores it; then the counter follows it down.
    #[test]
    fn older_is_reported_then_restored() {
        let (path, state) = fresh_dir("older");
        accept_on_open(&state, VAULT, 9).unwrap();
        assert_eq!(open_or_older(&state, VAULT, 9), Ok(Ok(Seen::Known)));
        let older = open_or_older(&state, VAULT, 4).unwrap().unwrap_err();
        assert_eq!(older, Older { seen: 4, stored: 9 });
        assert_eq!(load(&state, VAULT), Ok(Some(9)), "unchanged until restored");
        restore(&state, VAULT, older).unwrap();
        assert_eq!(load(&state, VAULT), Ok(Some(4)));
        assert_eq!(open_or_older(&state, VAULT, 4), Ok(Ok(Seen::Known)));
        // A corrupt counter is still an error, never "older".
        fs::write(vault_file(&path), b"x\n").unwrap();
        assert_eq!(
            open_or_older(&state, VAULT, 4),
            Err(GenError::Corrupt { what: "vault" })
        );
    }

    #[test]
    fn record_save_never_lowers() {
        let (_, state) = fresh_dir("save");
        record_save(&state, VAULT, 3).unwrap();
        assert_eq!(load(&state, VAULT), Ok(Some(3)));
        record_save(&state, VAULT, 4).unwrap();
        record_save(&state, VAULT, 4).unwrap();
        assert_eq!(load(&state, VAULT), Ok(Some(4)));
        let err = record_save(&state, VAULT, 2).unwrap_err();
        assert_eq!(
            err,
            GenError::Rollback {
                what: "vault",
                seen: 2,
                stored: 4
            }
        );
        assert_eq!(load(&state, VAULT), Ok(Some(4)));
    }

    // Acceptance: a corrupt counter fails closed and is left as it is,
    // never reset.
    #[test]
    fn corrupt_counter_fails_closed() {
        let (path, state) = fresh_dir("corrupt");
        let file = vault_file(&path);
        let bad: [&[u8]; 6] = [
            b"",
            b"abc\n",
            b"05\n",
            b"5",
            b"-1\n",
            b"18446744073709551616\n",
        ];
        for content in bad {
            fs::write(&file, content).unwrap();
            let err = accept_on_open(&state, VAULT, 7).unwrap_err();
            assert_eq!(err, GenError::Corrupt { what: "vault" }, "{content:?}");
            assert_eq!(fs::read(&file).unwrap(), content, "rewritten");
            assert_eq!(
                record_save(&state, VAULT, 7).unwrap_err(),
                GenError::Corrupt { what: "vault" }
            );
        }
        // Far longer than any counter: only a few bytes are read.
        fs::write(&file, [b'1'; 4096]).unwrap();
        assert_eq!(
            load(&state, VAULT),
            Err(GenError::Corrupt { what: "vault" })
        );
    }

    #[test]
    fn counter_must_be_a_regular_file() {
        let (path, state) = fresh_dir("symlink");
        fs::write(path.join("elsewhere"), b"1\n").unwrap();
        symlink(path.join("elsewhere"), vault_file(&path)).unwrap();
        let err = load(&state, VAULT).unwrap_err();
        assert_eq!(
            err,
            GenError::Read {
                what: "vault",
                error: ReadError::NotARegularFile
            }
        );
        let err = accept_on_open(&state, VAULT, 1).unwrap_err();
        assert!(matches!(err, GenError::Read { .. }), "{err:?}");
    }
}
