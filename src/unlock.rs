//! Startup step 8, unlock (docs/design/startup.md): the vault's passphrase
//! opens its `identity.age`, which opens `vault.age`. The vault must be the
//! one pinned to its name on this device, and not older than the newest
//! version seen here. Every check fails closed; no message names a note, a
//! vault or a key.

use std::fmt;

use rustix::fs::{Dir, Mode, OFlags, openat};
use rustix::io::Errno;

use crate::crypto::{self, CryptoError, Passphrase, VaultIdentity};
use crate::vault::codec::VaultContents;
use crate::vault::format::{self, IDENTITY_FILE, MAX_IDENTITY_FILE, VAULT_FILE, VaultError};
use crate::vault::generation::{self, Counter, GenError, Older, Seen};
use crate::vault::pin::{self, PinError, Pinned};
use crate::vault::public_file::{ReadError, read_bounded};
use crate::vault::vault_dir::{OpenVaultError, VaultDir};
use crate::vault::{DataDir, StateDir, VaultName};

/// Syncthing names a conflict copy `vault.sync-conflict-<date>-<time>-<id>.age`.
pub const CONFLICT_PREFIX: &[u8] = b"vault.sync-conflict-";
pub const CONFLICT_SUFFIX: &[u8] = b".age";

/// Why unlock stopped. Fixed text; never a name, path, title or key.
#[derive(Debug, PartialEq, Eq)]
pub enum UnlockError {
    Path(OpenVaultError),
    Read(ReadError),
    /// `identity.age` or `vault.age` is missing.
    MissingFile(&'static str),
    /// `identity.age` or `vault.age` is over its size limit.
    TooLarge(&'static str),
    Identity(CryptoError),
    Vault(VaultError),
    Pin(PinError),
    Generation(GenError),
    /// A sync conflict copy of `vault.age` is present (merge: Phase 4).
    SyncConflict,
    ListDir(Errno),
    /// An older vault was not accepted with `restore`.
    NotRestored,
}

impl fmt::Display for UnlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(e) => e.fmt(f),
            Self::Read(e) => write!(f, "cannot read a vault file ({e})"),
            Self::MissingFile(file) => write!(f, "the vault has no {file}"),
            Self::TooLarge(file) => write!(f, "the vault's {file} is too large"),
            Self::Identity(CryptoError::WrongPassphrase) => write!(
                f,
                "the vault's identity.age does not open with this passphrase"
            ),
            Self::Identity(e) => write!(f, "identity.age: {e}"),
            Self::Vault(e) => write!(f, "vault.age: {e}"),
            Self::Pin(e) => e.fmt(f),
            Self::Generation(e) => e.fmt(f),
            Self::SyncConflict => write!(
                f,
                "the vault has a sync conflict copy (vault.sync-conflict-*.age); merging is not \
                 implemented yet, resolve it by hand"
            ),
            Self::ListDir(errno) => write!(f, "cannot list the vault directory ({errno})"),
            Self::NotRestored => write!(f, "not restored; nothing was changed"),
        }
    }
}

impl std::error::Error for UnlockError {}

/// An unlocked vault. The vault and state fds stay open for the session
/// (the Landlock rule fds in Phase 6); nothing else does.
pub struct Unlocked {
    pub vault_dir: VaultDir,
    pub contents: VaultContents,
    /// `New` on the first unlock of this vault on this device.
    pub pinned: Pinned,
    /// `FirstUse` when this device had no rollback counter for the vault.
    pub seen: Seen,
    /// The vault is older than this device's counter; until the user types
    /// `restore` (`restore_prompt`, `accept_restore`), nothing was changed.
    pub older: Option<Older>,
}

/// What must be typed to accept an older vault.
pub const RESTORE_WORD: &str = "restore";

/// The question for an older vault (docs/decisions/0036-backups-external.md):
/// fixed text and two generation numbers.
pub fn restore_prompt(older: Older) -> String {
    format!(
        "This vault is older than the newest version this device has seen (generation {}, \
         last seen {}).\r\n\r\nIf you restored it from a backup, type restore to accept it; \
         rollback detection then starts again from this version.\r\nIf you did not restore \
         anything, someone may have put back an old copy: do not type restore.\r\n\r\nType \
         restore to accept: ",
        older.seen, older.stored
    )
}

/// The user typed `restore`: the vault's counter goes back to its
/// generation.
pub fn accept_restore(
    state: &StateDir,
    vault_id: crate::model::VaultId,
    older: Older,
) -> Result<(), UnlockError> {
    generation::restore(state.fd(), Counter::Vault(vault_id), older)
        .map_err(UnlockError::Generation)
}

/// Opens the vault directory `name`. Before any prompt, so a mistyped
/// name fails at once.
pub fn open_vault_dir(data: &DataDir, name: &VaultName) -> Result<VaultDir, UnlockError> {
    let vault_dir = VaultDir::open(data, name).map_err(UnlockError::Path)?;
    if has_sync_conflict(&vault_dir)? {
        return Err(UnlockError::SyncConflict);
    }
    Ok(vault_dir)
}

/// Step 8: open `identity.age` with `pass`, then wipe the passphrase;
/// decrypt `vault.age`; require the pinned `vault_id` (or pin it); check
/// and update the vault's counter. An older vault is not refused here: it
/// comes back with `older` set and the counter unchanged, for the user to
/// accept or not.
pub fn unlock(
    vault_dir: VaultDir,
    name: &VaultName,
    pass: Passphrase,
    state: &StateDir,
) -> Result<Unlocked, UnlockError> {
    // read_bounded reads one byte past `max` to show the file is longer.
    let read = |file: &'static str, max: usize| {
        let bytes = read_bounded(vault_dir.fd(), file, max)
            .map_err(UnlockError::Read)?
            .ok_or(UnlockError::MissingFile(file))?;
        if bytes.len() > max {
            return Err(UnlockError::TooLarge(file));
        }
        Ok(bytes)
    };
    let identity =
        VaultIdentity::open_with_passphrase(&read(IDENTITY_FILE, MAX_IDENTITY_FILE)?, &pass)
            .map_err(UnlockError::Identity)?;
    // The passphrase has done its job: wipe it before the vault is read.
    drop(pass);
    let contents = format::open(
        &read(VAULT_FILE, crypto::MAX_CIPHERTEXT)?,
        &identity,
        vault_dir.version(),
    )
    .map_err(UnlockError::Vault)?;
    let pinned = pin::check(state.fd(), name, &contents.vault_id).map_err(UnlockError::Pin)?;
    let (seen, older) = match generation::open_or_older(
        state.fd(),
        Counter::Vault(contents.vault_id),
        contents.generation,
    )
    .map_err(UnlockError::Generation)?
    {
        Ok(seen) => (seen, None),
        Err(older) => (Seen::Known, Some(older)),
    };
    Ok(Unlocked {
        vault_dir,
        contents,
        pinned,
        seen,
        older,
    })
}

/// Whether the vault directory holds a Syncthing conflict copy of
/// `vault.age`. Lists the directory through a fresh fd for `.`.
fn has_sync_conflict(vault_dir: &VaultDir) -> Result<bool, UnlockError> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let listing =
        openat(vault_dir.fd(), c".", flags, Mode::empty()).map_err(UnlockError::ListDir)?;
    for entry in Dir::new(listing).map_err(UnlockError::ListDir)? {
        let entry = entry.map_err(UnlockError::ListDir)?;
        if is_sync_conflict(entry.file_name().to_bytes()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `vault.sync-conflict-*.age`.
pub fn is_sync_conflict(file: &[u8]) -> bool {
    file.starts_with(CONFLICT_PREFIX) && file.ends_with(CONFLICT_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init;
    use crate::model::VaultId;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use zeroize::Zeroizing;

    fn pass(s: &str) -> Passphrase {
        Passphrase::new(Zeroizing::new(s.as_bytes().to_vec())).unwrap()
    }

    fn name(s: &str) -> VaultName {
        VaultName::new(s.into()).unwrap()
    }

    fn set_mode(p: &Path, mode: u32) {
        fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// A private HOME with Crystal's data and state directories; vaults
    /// are made by `crystal init`'s own code, with passphrase "pw".
    struct World {
        home: PathBuf,
        data: DataDir,
        state: StateDir,
    }

    impl World {
        fn new(test: &str) -> Self {
            let home = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/unit-unlock")
                .join(test);
            let _ = fs::remove_dir_all(&home);
            fs::create_dir_all(&home).unwrap();
            set_mode(&home, 0o700);
            let data = DataDir::create(Some(home.as_os_str())).unwrap();
            let state = StateDir::open_or_create(Some(home.as_os_str())).unwrap();
            Self { home, data, state }
        }

        /// `crystal init NAME`: the vault in ~/.local/share/crystal/NAME.
        fn init(&self, n: &str) -> PathBuf {
            let plan = init::plan(&self.data, &name(n)).unwrap();
            let code = crate::recovery::RecoveryCode::generate().unwrap();
            init::create(plan, &name(n), &pass("pw"), &code, &self.data, &self.state).unwrap();
            self.home.join(".local/share/crystal").join(n)
        }

        fn vault_id(&self, n: &str) -> VaultId {
            pin::load(self.state.fd(), &name(n)).unwrap().unwrap()
        }

        fn unlock(&self, n: &str, pw: &str) -> Result<Unlocked, UnlockError> {
            let dir = open_vault_dir(&self.data, &name(n))?;
            unlock(dir, &name(n), pass(pw), &self.state)
        }

        /// Unlocking `n` with "pw" is refused; the message names no path
        /// and no vault.
        fn refused(&self, n: &str) -> UnlockError {
            let Err(e) = self.unlock(n, "pw") else {
                panic!("{n} unlocked")
            };
            let text = e.to_string();
            assert!(!text.contains(self.home.to_str().unwrap()), "{text}");
            assert!(!text.contains(n), "{text}");
            e
        }
    }

    // The whole chain: path, identity, vault, pin, counter. `crystal init`
    // pins the vault and records its counter, so its own device knows it;
    // a device without them pins it and reports first use once.
    #[test]
    fn unlocks_and_tracks_first_use() {
        let world = World::new("ok");
        world.init("diary");
        let unlocked = world.unlock("diary", "pw").unwrap();
        assert_eq!(
            (unlocked.pinned, unlocked.seen),
            (Pinned::Known, Seen::Known)
        );
        assert!(unlocked.contents.vault_id == world.vault_id("diary"));
        assert!(unlocked.contents.notes.is_empty());

        let id = world.vault_id("diary");
        let state = world.home.join(".local/state/crystal");
        fs::remove_file(state.join("diary.pin")).unwrap();
        fs::remove_file(state.join(format!("{}.gen", id.hex()))).unwrap();
        let first = world.unlock("diary", "pw").unwrap();
        assert_eq!((first.pinned, first.seen), (Pinned::New, Seen::FirstUse));
        let again = world.unlock("diary", "pw").unwrap();
        assert_eq!((again.pinned, again.seen), (Pinned::Known, Seen::Known));
        assert!(world.vault_id("diary") == id);
    }

    #[test]
    fn refuses_wrong_passphrase_and_unknown_name() {
        let world = World::new("wrong_pass");
        world.init("diary");
        let Err(e) = world.unlock("diary", "not-pw") else {
            panic!("unlocked")
        };
        assert_eq!(e, UnlockError::Identity(CryptoError::WrongPassphrase));
        assert!(e.to_string().contains("does not open with this passphrase"));
        assert_eq!(
            world.refused("work"),
            UnlockError::Path(OpenVaultError::NoSuchVault)
        );
    }

    #[test]
    fn refuses_identity_that_is_not_a_key() {
        let world = World::new("identity_not_a_key");
        let vault = world.init("diary");
        let junk = crypto::encrypt_to_passphrase(b"not a key", &pass("pw")).unwrap();
        fs::write(vault.join(IDENTITY_FILE), junk.as_bytes()).unwrap();
        assert_eq!(
            world.refused("diary"),
            UnlockError::Identity(CryptoError::Corrupt)
        );
    }

    // identity.age from another vault under the same passphrase: it opens,
    // but vault.age is not encrypted to it.
    #[test]
    fn refuses_identity_of_another_vault() {
        let world = World::new("identity_swapped");
        let diary = world.init("diary");
        let other = world.init("other");
        fs::copy(other.join(IDENTITY_FILE), diary.join(IDENTITY_FILE)).unwrap();
        let e = world.refused("diary");
        assert!(matches!(e, UnlockError::Vault(_)), "{e:?}");
        assert!(e.to_string().starts_with("vault.age: "), "{e}");
    }

    #[test]
    fn refuses_missing_files() {
        let world = World::new("missing_files");
        let vault = world.init("diary");
        fs::remove_file(vault.join(VAULT_FILE)).unwrap();
        assert_eq!(world.refused("diary"), UnlockError::MissingFile(VAULT_FILE));
        fs::remove_file(vault.join(IDENTITY_FILE)).unwrap();
        assert_eq!(
            world.refused("diary"),
            UnlockError::MissingFile(IDENTITY_FILE)
        );
    }

    #[test]
    fn refuses_oversized_identity() {
        let world = World::new("identity_too_large");
        let vault = world.init("diary");
        fs::write(vault.join(IDENTITY_FILE), vec![b'x'; MAX_IDENTITY_FILE + 1]).unwrap();
        assert_eq!(world.refused("diary"), UnlockError::TooLarge(IDENTITY_FILE));
        // At the limit it is read, and refused only as not decrypting.
        fs::write(vault.join(IDENTITY_FILE), vec![b'x'; MAX_IDENTITY_FILE]).unwrap();
        assert!(matches!(world.refused("diary"), UnlockError::Identity(_)));
    }

    // Both key and vault swapped in from another vault (same passphrase):
    // everything decrypts, only the pin tells.
    #[test]
    fn refuses_swapped_vault() {
        let world = World::new("swapped");
        let diary = world.init("diary");
        let other = world.init("other");
        for file in [IDENTITY_FILE, VAULT_FILE] {
            fs::copy(other.join(file), diary.join(file)).unwrap();
        }
        assert_eq!(world.refused("diary"), UnlockError::Pin(PinError::Mismatch));
    }

    // An older vault opens with `older` set and the counter unchanged;
    // accepting it lowers the counter, and the next unlock is plain.
    #[test]
    fn older_vault_waits_for_restore() {
        let world = World::new("older");
        world.init("diary");
        let id = world.vault_id("diary");
        generation::store(world.state.fd(), Counter::Vault(id), 99).unwrap();
        let unlocked = world.unlock("diary", "pw").unwrap();
        let older = unlocked.older.unwrap();
        assert_eq!(
            older,
            Older {
                seen: 1,
                stored: 99
            }
        );
        assert_eq!(
            generation::load(world.state.fd(), Counter::Vault(id)),
            Ok(Some(99))
        );
        let prompt = restore_prompt(older);
        assert!(prompt.contains("generation 1, last seen 99") && !prompt.contains("diary"));
        accept_restore(&world.state, id, older).unwrap();
        assert_eq!(
            generation::load(world.state.fd(), Counter::Vault(id)),
            Ok(Some(1))
        );
        assert_eq!(world.unlock("diary", "pw").unwrap().older, None);
    }

    #[test]
    fn refuses_corrupt_counter() {
        let world = World::new("counter");
        world.init("diary");
        let id = world.vault_id("diary");
        let file = world
            .home
            .join(".local/state/crystal")
            .join(format!("{}.gen", id.hex()));
        fs::write(&file, b"nine\n").unwrap();
        assert_eq!(
            world.refused("diary"),
            UnlockError::Generation(GenError::Corrupt { what: "vault" })
        );
    }

    // Syncthing's conflict copies of vault.age are refused; other names,
    // including near misses, are not mistaken for one.
    #[test]
    fn sync_conflict_names() {
        let world = World::new("sync_conflict");
        let vault = world.init("diary");
        for harmless in [
            "vault.sync-conflict-20261106-120000-ABCDEFG.age.tmp",
            "notes.sync-conflict-20261106-120000-ABCDEFG.age",
            "vault.age.sync-conflict-20261106-120000-ABCDEFG",
            "vault.sync-conflict",
        ] {
            fs::write(vault.join(harmless), b"").unwrap();
            assert!(world.unlock("diary", "pw").is_ok(), "{harmless}");
            fs::remove_file(vault.join(harmless)).unwrap();
        }
        for conflict in [
            "vault.sync-conflict-20261106-120000-ABCDEFG.age",
            "vault.sync-conflict-.age",
        ] {
            fs::write(vault.join(conflict), b"").unwrap();
            assert_eq!(
                world.refused("diary"),
                UnlockError::SyncConflict,
                "{conflict}"
            );
            fs::remove_file(vault.join(conflict)).unwrap();
        }
    }

    // The vault directory itself: a symlink, a loose mode or a missing
    // VERSION are refused before identity.age is read.
    #[test]
    fn refuses_unsafe_vault_directories() {
        let world = World::new("unsafe_dir");
        let vault = world.init("diary");
        let data = world.home.join(".local/share/crystal");
        symlink(&vault, data.join("alias")).unwrap();
        assert_eq!(
            world.refused("alias"),
            UnlockError::Path(OpenVaultError::NotARealDirectory)
        );
        set_mode(&vault, 0o750);
        assert_eq!(
            world.refused("diary"),
            UnlockError::Path(OpenVaultError::BadMode(0o750))
        );
        set_mode(&vault, 0o700);
        fs::remove_file(vault.join("VERSION")).unwrap();
        assert!(matches!(
            world.refused("diary"),
            UnlockError::Path(OpenVaultError::Version(_))
        ));
    }
}
