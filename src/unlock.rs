//! Startup step 8, unlock (docs/design/startup.md): one passphrase opens
//! the config, then the named vault's identity, then the vault. Every
//! check fails closed; no message names a note, a path or a key.

use std::ffi::OsStr;
use std::fmt;

use rustix::fs::{Dir, Mode, OFlags, openat};
use rustix::io::Errno;

use crate::config::{self, ConfigError};
use crate::crypto::{self, CryptoError, Passphrase, VaultIdentity};
use crate::vault::codec::VaultContents;
use crate::vault::format::{self, VaultError};
use crate::vault::generation::{self, Counter, GenError, Seen};
use crate::vault::public_file::{ReadError, read_bounded};
use crate::vault::vault_dir::{OpenVaultError, VaultDir};
use crate::vault::{ConfigDir, StateDir, VaultName};

const IDENTITY_FILE: &str = "identity.age";
const VAULT_FILE: &str = "vault.age";
/// `identity.age` holds one age secret key: far below this.
const MAX_IDENTITY_FILE: usize = 64 << 10;
/// Syncthing names a conflict copy `vault.sync-conflict-<date>-<time>-<id>.age`.
const CONFLICT_PREFIX: &[u8] = b"vault.sync-conflict-";
const CONFLICT_SUFFIX: &[u8] = b".age";

/// Why unlock stopped. Fixed text; never a name, path, title or key.
#[derive(Debug, PartialEq, Eq)]
pub enum UnlockError {
    /// No `config.age` on this device.
    NoConfig,
    Config(ConfigError),
    UnknownName,
    Path(OpenVaultError),
    Read(ReadError),
    /// `identity.age` or `vault.age` is missing.
    MissingFile(&'static str),
    Identity(CryptoError),
    Vault(VaultError),
    /// The directory at the registered path holds a different vault than
    /// the one pinned in the config.
    ForeignVault,
    Generation(GenError),
    /// A sync conflict copy of `vault.age` is present (merge: #31).
    SyncConflict,
    ListDir(Errno),
}

impl fmt::Display for UnlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoConfig => write!(
                f,
                "no vault is registered on this device; create one with crystal init or register \
                 one with crystal add"
            ),
            Self::Config(e) => e.fmt(f),
            Self::UnknownName => write!(f, "no vault with that name is registered"),
            Self::Path(e) => e.fmt(f),
            Self::Read(e) => write!(f, "cannot read a vault file ({e})"),
            Self::MissingFile(file) => write!(f, "the vault has no {file}"),
            Self::Identity(CryptoError::WrongPassphrase) => write!(
                f,
                "the vault's identity.age does not open with this passphrase"
            ),
            Self::Identity(e) => write!(f, "identity.age: {e}"),
            Self::Vault(e) => write!(f, "vault.age: {e}"),
            Self::ForeignVault => write!(
                f,
                "the directory registered under this name holds a different vault (its \
                 vault_id is not the pinned one): refusing a swapped vault"
            ),
            Self::Generation(e) => e.fmt(f),
            Self::SyncConflict => write!(
                f,
                "the vault has a sync conflict copy (vault.sync-conflict-*.age); merging is not \
                 implemented yet, resolve it by hand"
            ),
            Self::ListDir(errno) => write!(f, "cannot list the vault directory ({errno})"),
        }
    }
}

impl std::error::Error for UnlockError {}

/// An unlocked vault. The vault and state fds stay open for the session
/// (the Landlock rule fds in Phase 6); nothing else does.
pub struct Unlocked {
    #[expect(
        dead_code,
        reason = "held open for the session; the Landlock rule fd in Phase 6"
    )]
    pub vault_dir: VaultDir,
    pub contents: VaultContents,
    /// `FirstUse` on the first unlock of this vault on this device.
    pub seen: Seen,
}

/// Step 8. With the config's passphrase: decrypt `config.age` and check
/// `config.gen`; find `name`; walk and check its path; refuse a sync
/// conflict; open `identity.age`, then wipe the passphrase; decrypt
/// `vault.age`; require the pinned `vault_id`; check and update the
/// vault's counter.
pub fn unlock(
    name: &VaultName,
    pass: Passphrase,
    home: &OsStr,
    config_dir: &ConfigDir,
    state: &StateDir,
) -> Result<Unlocked, UnlockError> {
    let (cfg, _) = config::load(config_dir, state, &pass)
        .map_err(UnlockError::Config)?
        .ok_or(UnlockError::NoConfig)?;
    let entry = cfg.find(name).ok_or(UnlockError::UnknownName)?;
    let vault_dir = VaultDir::open_checked(entry.path.as_ref(), home, config_dir, state)
        .map_err(UnlockError::Path)?;
    if has_sync_conflict(&vault_dir)? {
        return Err(UnlockError::SyncConflict);
    }

    let read = |file: &'static str, max| {
        read_bounded(vault_dir.fd(), file, max)
            .map_err(UnlockError::Read)?
            .ok_or(UnlockError::MissingFile(file))
    };
    let identity =
        VaultIdentity::open_with_passphrase(&read(IDENTITY_FILE, MAX_IDENTITY_FILE)?, &pass)
            .map_err(UnlockError::Identity)?;
    // The passphrase has done its job: wipe it before the vault is read.
    drop(pass);
    let contents = format::open(&read(VAULT_FILE, crypto::MAX_CIPHERTEXT)?, &identity)
        .map_err(UnlockError::Vault)?;
    if contents.vault_id != entry.vault_id {
        return Err(UnlockError::ForeignVault);
    }
    let seen = generation::accept_on_open(
        state.fd(),
        Counter::Vault(contents.vault_id),
        contents.generation,
    )
    .map_err(UnlockError::Generation)?;
    Ok(Unlocked {
        vault_dir,
        contents,
        seen,
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
        let file = entry.file_name().to_bytes();
        if file.starts_with(CONFLICT_PREFIX) && file.ends_with(CONFLICT_SUFFIX) {
            return Ok(true);
        }
    }
    Ok(false)
}
