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
use crate::vault::format::{self, IDENTITY_FILE, MAX_IDENTITY_FILE, VAULT_FILE, VaultError};
use crate::vault::generation::{self, Counter, GenError, Seen};
use crate::vault::public_file::{ReadError, read_bounded};
use crate::vault::vault_dir::{OpenVaultError, VaultDir};
use crate::vault::{ConfigDir, StateDir, VaultName};

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
    /// `identity.age` or `vault.age` is over its size limit.
    TooLarge(&'static str),
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
            Self::TooLarge(file) => write!(f, "the vault's {file} is too large"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::init;
    use crate::vault::vault_dir::OpenVaultError;
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

    /// A private HOME with Crystal's config and state directories; vaults
    /// are made by `crystal init`'s own code, with passphrase "pw".
    struct World {
        base: PathBuf,
        home: PathBuf,
        config: ConfigDir,
        state: StateDir,
    }

    impl World {
        fn new(test: &str) -> Self {
            let base = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/unit-unlock")
                .join(test);
            let _ = fs::remove_dir_all(&base);
            let home = base.join("home");
            fs::create_dir_all(&home).unwrap();
            set_mode(&base, 0o755);
            set_mode(&home, 0o700);
            let config = ConfigDir::create(Some(home.as_os_str())).unwrap();
            let state = StateDir::open_or_create(Some(home.as_os_str())).unwrap();
            Self {
                base,
                home,
                config,
                state,
            }
        }

        /// `crystal init NAME`: the vault in ~/Vaults/NAME, registered.
        fn init(&self, n: &str) -> PathBuf {
            let cfg = match config::load(&self.config, &self.state, &pass("pw")).unwrap() {
                Some((cfg, _)) => cfg,
                None => config::fresh(&self.state).unwrap(),
            };
            let plan = init::plan(
                self.home.as_os_str(),
                None,
                &name(n),
                &self.config,
                &self.state,
            )
            .unwrap();
            let backup = crate::recovery::RecoveryCode::generate().unwrap();
            init::create(
                plan,
                &name(n),
                &pass("pw"),
                &backup,
                cfg,
                self.home.as_os_str(),
                &self.config,
                &self.state,
            )
            .unwrap();
            self.home.join("Vaults").join(n)
        }

        fn config(&self) -> Config {
            config::load(&self.config, &self.state, &pass("pw"))
                .unwrap()
                .unwrap()
                .0
        }

        /// Rewrites the config with `n`'s registered path set to `path`:
        /// the config is attacker-written as far as unlock is concerned.
        fn point(&self, n: &str, path: &[u8]) {
            let mut cfg = self.config();
            let entry = cfg.vaults.iter_mut().find(|e| e.name == name(n)).unwrap();
            entry.path = String::from_utf8(path.to_vec()).unwrap();
            config::store(&mut cfg, &self.config, &self.state, &pass("pw")).unwrap();
        }

        fn vault_id(&self, n: &str) -> crate::model::VaultId {
            self.config().find(&name(n)).unwrap().vault_id
        }

        /// The only vault counter in the state directory (one vault).
        fn counter_file(&self) -> PathBuf {
            fs::read_dir(self.home.join(".local/state/crystal"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| p.file_name().unwrap() != "config.gen")
                .unwrap()
        }

        fn unlock(&self, n: &str, pw: &str) -> Result<Unlocked, UnlockError> {
            unlock(
                &name(n),
                pass(pw),
                self.home.as_os_str(),
                &self.config,
                &self.state,
            )
        }

        /// Unlocking `n` with "pw" is refused; the message names no path
        /// and no vault.
        fn refused(&self, n: &str) -> UnlockError {
            let Err(e) = self.unlock(n, "pw") else {
                panic!("{n} unlocked")
            };
            let text = e.to_string();
            assert!(!text.contains(self.base.to_str().unwrap()), "{text}");
            assert!(!text.contains(n), "{text}");
            e
        }
    }

    // The whole chain: config, path, identity, vault, pinned id, counter.
    // `crystal init` records the vault's counter, so its own device knows
    // the vault; a device without the counter reports first use once.
    #[test]
    fn unlocks_and_tracks_first_use() {
        let world = World::new("ok");
        world.init("diary");
        let unlocked = world.unlock("diary", "pw").unwrap();
        assert_eq!(unlocked.seen, Seen::Known);
        assert!(unlocked.contents.vault_id == world.vault_id("diary"));
        assert!(unlocked.contents.notes.is_empty());

        let counter = Counter::Vault(world.vault_id("diary"));
        let stored = generation::load(world.state.fd(), counter).unwrap();
        fs::remove_file(world.counter_file()).unwrap();
        assert_eq!(world.unlock("diary", "pw").unwrap().seen, Seen::FirstUse);
        assert_eq!(world.unlock("diary", "pw").unwrap().seen, Seen::Known);
        assert_eq!(generation::load(world.state.fd(), counter).unwrap(), stored);
    }

    #[test]
    fn refuses_without_config() {
        let world = World::new("no_config");
        assert_eq!(world.refused("diary"), UnlockError::NoConfig);
    }

    #[test]
    fn refuses_wrong_passphrase_and_unknown_name() {
        let world = World::new("wrong_pass");
        world.init("diary");
        let Err(e) = world.unlock("diary", "not-pw") else {
            panic!("unlocked")
        };
        assert_eq!(
            e,
            UnlockError::Config(ConfigError::Crypto(CryptoError::WrongPassphrase))
        );
        assert_eq!(world.refused("work"), UnlockError::UnknownName);
    }

    // The config opens, but identity.age was sealed to another passphrase
    // (changed on another device, or planted).
    #[test]
    fn refuses_identity_under_another_passphrase() {
        let world = World::new("identity_other_pass");
        let vault = world.init("diary");
        let other = VaultIdentity::generate()
            .seal_with_passphrase(&pass("other"))
            .unwrap();
        fs::write(vault.join(IDENTITY_FILE), other.as_bytes()).unwrap();
        let e = world.refused("diary");
        assert_eq!(e, UnlockError::Identity(CryptoError::WrongPassphrase));
        assert!(e.to_string().contains("does not open with this passphrase"));
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
    // everything decrypts, only the pinned vault_id tells.
    #[test]
    fn refuses_foreign_vault() {
        let world = World::new("foreign");
        let diary = world.init("diary");
        let other = world.init("other");
        for file in [IDENTITY_FILE, VAULT_FILE] {
            fs::copy(other.join(file), diary.join(file)).unwrap();
        }
        assert_eq!(world.refused("diary"), UnlockError::ForeignVault);
    }

    #[test]
    fn refuses_rolled_back_or_corrupt_counter() {
        let world = World::new("counter");
        world.init("diary");
        let counter = Counter::Vault(world.vault_id("diary"));
        generation::store(world.state.fd(), counter, 99).unwrap();
        assert!(matches!(
            world.refused("diary"),
            UnlockError::Generation(GenError::Rollback {
                what: "vault",
                stored: 99,
                ..
            })
        ));
        let file = world.counter_file();
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

    // A config may point anywhere; unlock walks the path as if an attacker
    // wrote it. Each case is refused before identity.age is read. (A
    // relative path or one with `..` cannot reach here: the config itself
    // refuses it, config::tests.)
    #[test]
    fn refuses_crafted_paths() {
        let world = World::new("crafted");
        let vault = world.init("diary");
        let home = &world.home;
        let ssh = home.join(".ssh");
        fs::create_dir(&ssh).unwrap();
        set_mode(&ssh, 0o700);
        fs::write(ssh.join("id_ed25519"), b"").unwrap();
        let link = home.join("link");
        symlink(&vault, &link).unwrap();
        let bytes = |p: &Path| p.to_str().unwrap().as_bytes().to_vec();

        type Case = (Vec<u8>, fn(&OpenVaultError) -> bool);
        let cases: Vec<Case> = vec![
            (bytes(&link), |e| {
                matches!(e, OpenVaultError::NotARealDirectory { .. })
            }),
            (bytes(&home.join("gone")), |e| {
                matches!(e, OpenVaultError::Missing { .. })
            }),
            (bytes(home), |e| *e == OpenVaultError::IsHome),
            (bytes(&home.join(".config/crystal")), |e| {
                *e == OpenVaultError::InsideCrystalDir
            }),
            (bytes(&home.join(".local/state/crystal")), |e| {
                *e == OpenVaultError::InsideCrystalDir
            }),
            (bytes(&home.join(".local")), |e| {
                *e == OpenVaultError::ContainsCrystalDir
            }),
            (bytes(&ssh), |e| matches!(e, OpenVaultError::Version(_))),
        ];
        for (path, expected) in cases {
            world.point("diary", &path);
            let e = world.refused("diary");
            let shown = String::from_utf8_lossy(&path);
            assert!(
                matches!(&e, UnlockError::Path(p) if expected(p)),
                "{shown}: {e:?}"
            );
        }

        // The real path again, but below a group-writable directory.
        world.point("diary", &bytes(&vault));
        world.unlock("diary", "pw").unwrap();
        set_mode(&home.join("Vaults"), 0o770);
        assert!(matches!(
            world.refused("diary"),
            UnlockError::Path(OpenVaultError::UnsafeParent { .. })
        ));
    }
}
