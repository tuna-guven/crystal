//! `crystal recover NAME DIR` (docs/decisions/0029-recovery-code.md):
//! open a vault with its recovery code when the passphrase is forgotten
//! (or `identity.age` is lost), then seal the vault key again with this
//! device's passphrase and register the vault.
//!
//! The prompts are in `main::recover_vault`; this module does the rest, so
//! the unit tests can drive it without a terminal.

use std::fmt;

use crate::config::{self, Config, ConfigError, VaultEntry};
use crate::crypto::{self, CryptoError, Passphrase, VaultIdentity};
use crate::model::VaultId;
use crate::recovery::{CodeError, RecoveryCode};
use crate::vault::atomic::{FileName, Payload, WriteError, atomic_write};
use crate::vault::format::{
    self, IDENTITY_BACKUP_FILE, IDENTITY_FILE, MAX_IDENTITY_FILE, VAULT_FILE, VaultError,
};
use crate::vault::generation::{self, Counter, GenError};
use crate::vault::public_file::{ReadError, read_bounded};
use crate::vault::vault_dir::VaultDir;
use crate::vault::{ConfigDir, StateDir, VaultName};

/// The prompt for the recovery code. Shown as typed, like at `crystal init`.
pub const CODE_PROMPT: &str = "Recovery code (written down at crystal init): ";
/// On a device with a config: its passphrase, or Enter if forgotten too.
pub const DEVICE_PROMPT: &str = "Passphrase of this device's Crystal config \
                                 (press Enter if you forgot it too): ";
/// Asked before a forgotten config is replaced.
pub const REPLACE_PROMPT: &str = "This device's config.age opens only with the forgotten \
     passphrase. It will be replaced by a new config that holds this vault; vaults \
     registered with the old passphrase must be recovered or added again.\r\n\
     Type replace to continue: ";
/// What must be typed at `REPLACE_PROMPT`.
pub const REPLACE_WORD: &str = "replace";

/// Why `crystal recover` stopped. Fixed text; never a path, name, code or key.
#[derive(Debug, PartialEq, Eq)]
pub enum RecoverError {
    Code(CodeError),
    /// The code is well formed but does not open `identity.backup.age`.
    WrongCode,
    Read(ReadError),
    /// `identity.backup.age` or `vault.age` is missing.
    MissingFile(&'static str),
    TooLarge(&'static str),
    Identity(CryptoError),
    Vault(VaultError),
    Config(ConfigError),
    Generation(GenError),
    Write(WriteError),
    /// The forgotten config was not replaced; nothing changed.
    NotReplaced,
}

impl fmt::Display for RecoverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Code(e) => e.fmt(f),
            Self::WrongCode => write!(
                f,
                "this recovery code does not open the vault's identity.backup.age \
                 (is it the code of another vault?)"
            ),
            Self::Read(e) => write!(f, "cannot read a vault file ({e})"),
            Self::MissingFile(file) => write!(f, "the vault has no {file}"),
            Self::TooLarge(file) => write!(f, "the vault's {file} is too large"),
            Self::Identity(e) => write!(f, "identity.backup.age: {e}"),
            Self::Vault(e) => write!(f, "vault.age: {e}"),
            Self::Config(e) => e.fmt(f),
            Self::Generation(e) => e.fmt(f),
            Self::Write(e) => write!(f, "cannot write identity.age: {e}"),
            Self::NotReplaced => write!(f, "not replaced; nothing was changed"),
        }
    }
}

impl std::error::Error for RecoverError {}

/// The vault key, opened with the recovery code, and what the vault says
/// about itself.
pub struct Opened {
    identity: VaultIdentity,
    vault_id: VaultId,
    generation: u64,
}

/// Opens `identity.backup.age` with `code`, then `vault.age` with the key
/// inside, to learn the vault's id and generation. Nothing is written.
pub fn open_backup(vault_dir: &VaultDir, code: &RecoveryCode) -> Result<Opened, RecoverError> {
    let read = |file: &'static str, max: usize| {
        let bytes = read_bounded(vault_dir.fd(), file, max)
            .map_err(RecoverError::Read)?
            .ok_or(RecoverError::MissingFile(file))?;
        if bytes.len() > max {
            return Err(RecoverError::TooLarge(file));
        }
        Ok(bytes)
    };
    let backup = read(IDENTITY_BACKUP_FILE, MAX_IDENTITY_FILE)?;
    let identity =
        VaultIdentity::open_with_passphrase(&backup, &code.passphrase()).map_err(|e| match e {
            CryptoError::WrongPassphrase => RecoverError::WrongCode,
            e => RecoverError::Identity(e),
        })?;
    let contents = format::open(&read(VAULT_FILE, crypto::MAX_CIPHERTEXT)?, &identity)
        .map_err(RecoverError::Vault)?;
    Ok(Opened {
        identity,
        vault_id: contents.vault_id,
        generation: contents.generation,
    })
}

/// Registers the vault in `cfg` as `name` unless it is already there
/// (same vault_id), checks and records its rollback counter, writes a new
/// `identity.age` sealed with `pass`, then stores the config if it
/// changed. `identity.age` comes first: if storing the config fails, the
/// vault already opens with `pass`, and `crystal add` registers it.
#[expect(
    clippy::too_many_arguments,
    reason = "one call site, each input distinct"
)]
pub fn finish(
    opened: Opened,
    name: &VaultName,
    path: String,
    pass: &Passphrase,
    mut cfg: Config,
    vault_dir: &VaultDir,
    config_dir: &ConfigDir,
    state: &StateDir,
) -> Result<(), RecoverError> {
    let registered = cfg.vaults.iter().any(|v| v.vault_id == opened.vault_id);
    if !registered {
        cfg.add(VaultEntry {
            name: VaultName::new(name.as_str().into()).ok_or(ConfigError::BadName)?,
            path,
            vault_id: opened.vault_id,
            lock_after: None,
        })?;
    }
    // A vault older than this device has seen is refused, as at unlock.
    generation::accept_on_open(
        state.fd(),
        Counter::Vault(opened.vault_id),
        opened.generation,
    )
    .map_err(RecoverError::Generation)?;
    let sealed = opened
        .identity
        .seal_with_passphrase(pass)
        .map_err(RecoverError::Identity)?;
    // A fixed, valid name: FileName::new cannot refuse it.
    let file = FileName::new(IDENTITY_FILE).ok_or(RecoverError::MissingFile(IDENTITY_FILE))?;
    atomic_write(vault_dir.fd(), &file, Payload::Ciphertext(&sealed))
        .map_err(RecoverError::Write)?;
    if !registered {
        config::store(&mut cfg, config_dir, state, pass)?;
    }
    Ok(())
}

impl From<ConfigError> for RecoverError {
    fn from(e: ConfigError) -> Self {
        Self::Config(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init;
    use crate::unlock::{self, UnlockError};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use zeroize::Zeroizing;

    fn pass(s: &str) -> Passphrase {
        Passphrase::new(Zeroizing::new(s.as_bytes().to_vec())).unwrap()
    }

    fn name(s: &str) -> VaultName {
        VaultName::new(s.into()).unwrap()
    }

    /// A HOME with Crystal's directories; vaults are made by init's own
    /// code with passphrase "pw", each with its recovery code.
    struct World {
        home: PathBuf,
        config: ConfigDir,
        state: StateDir,
    }

    impl World {
        fn new(test: &str) -> Self {
            let base = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/unit-recover")
                .join(test);
            let _ = fs::remove_dir_all(&base);
            let home = base.join("home");
            fs::create_dir_all(&home).unwrap();
            fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
            let config = ConfigDir::create(Some(home.as_os_str())).unwrap();
            let state = StateDir::open_or_create(Some(home.as_os_str())).unwrap();
            Self {
                home,
                config,
                state,
            }
        }

        fn init(&self, n: &str) -> (PathBuf, RecoveryCode) {
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
            let code = RecoveryCode::generate().unwrap();
            init::create(
                plan,
                &name(n),
                &pass("pw"),
                &code,
                cfg,
                self.home.as_os_str(),
                &self.config,
                &self.state,
            )
            .unwrap();
            (self.home.join("Vaults").join(n), code)
        }

        fn vault_dir(&self, path: &Path) -> VaultDir {
            VaultDir::open_checked(
                path.as_os_str(),
                self.home.as_os_str(),
                &self.config,
                &self.state,
            )
            .unwrap()
        }

        fn config_file(&self) -> PathBuf {
            self.home.join(".config/crystal/config.age")
        }

        /// The passphrase was forgotten: config.age is unusable, so the
        /// device starts a fresh config (config.gen stays).
        fn forget(&self) {
            fs::remove_file(self.config_file()).unwrap();
        }

        fn recover(
            &self,
            n: &str,
            path: &Path,
            code: &RecoveryCode,
            new: &str,
            cfg: Config,
        ) -> Result<(), RecoverError> {
            let dir = self.vault_dir(path);
            let opened = open_backup(&dir, code)?;
            finish(
                opened,
                &name(n),
                path.to_str().unwrap().to_owned(),
                &pass(new),
                cfg,
                &dir,
                &self.config,
                &self.state,
            )
        }

        fn unlock(&self, n: &str, pw: &str) -> Result<(), UnlockError> {
            unlock::unlock(
                &name(n),
                pass(pw),
                self.home.as_os_str(),
                &self.config,
                &self.state,
            )
            .map(|_| ())
        }
    }

    const NEW: &str = "a-new-passphrase-after-recovery";

    // The whole point: passphrase forgotten, config gone with it; the code
    // opens the vault, the new passphrase unlocks it, the old one is dead.
    #[test]
    fn recovers_a_forgotten_passphrase() {
        let world = World::new("forgotten");
        let (vault, code) = world.init("diary");
        world.forget();
        let fresh = config::fresh(&world.state).unwrap();
        world.recover("diary", &vault, &code, NEW, fresh).unwrap();
        world.unlock("diary", NEW).unwrap();
        assert!(world.unlock("diary", "pw").is_err());
        // The recovery code still works afterwards: it was not changed.
        let again = open_backup(&world.vault_dir(&vault), &code);
        assert!(again.is_ok());
    }

    // identity.age lost, passphrase known: the config opens, the vault is
    // already registered, so config.age is not rewritten.
    #[test]
    fn restores_a_lost_identity_with_the_known_passphrase() {
        let world = World::new("lost_identity");
        let (vault, code) = world.init("diary");
        fs::remove_file(vault.join(IDENTITY_FILE)).unwrap();
        let config_before = fs::read(world.config_file()).unwrap();
        let (cfg, _) = config::load(&world.config, &world.state, &pass("pw"))
            .unwrap()
            .unwrap();
        world.recover("diary", &vault, &code, "pw", cfg).unwrap();
        assert_eq!(fs::read(world.config_file()).unwrap(), config_before);
        world.unlock("diary", "pw").unwrap();
    }

    #[test]
    fn refuses_another_vaults_code() {
        let world = World::new("wrong_code");
        let (diary, _) = world.init("diary");
        let (_, other_code) = world.init("other");
        let identity = fs::read(diary.join(IDENTITY_FILE)).unwrap();
        let fresh = config::fresh(&world.state).unwrap();
        let err = world
            .recover("diary", &diary, &other_code, NEW, fresh)
            .unwrap_err();
        assert_eq!(err, RecoverError::WrongCode);
        assert_eq!(fs::read(diary.join(IDENTITY_FILE)).unwrap(), identity);
    }

    // identity.backup.age swapped in from another vault, with its code:
    // it opens, but vault.age is not encrypted to that key.
    #[test]
    fn refuses_a_backup_of_another_vault() {
        let world = World::new("swapped_backup");
        let (diary, _) = world.init("diary");
        let (other, other_code) = world.init("other");
        fs::copy(
            other.join(IDENTITY_BACKUP_FILE),
            diary.join(IDENTITY_BACKUP_FILE),
        )
        .unwrap();
        let err = open_backup(&world.vault_dir(&diary), &other_code).err();
        assert!(matches!(err, Some(RecoverError::Vault(_))), "{err:?}");
    }

    #[test]
    fn refuses_missing_or_oversized_backup() {
        let world = World::new("backup_files");
        let (vault, code) = world.init("diary");
        let dir = world.vault_dir(&vault);
        fs::write(
            vault.join(IDENTITY_BACKUP_FILE),
            vec![b'x'; MAX_IDENTITY_FILE + 1],
        )
        .unwrap();
        assert_eq!(
            open_backup(&dir, &code).err(),
            Some(RecoverError::TooLarge(IDENTITY_BACKUP_FILE))
        );
        fs::remove_file(vault.join(IDENTITY_BACKUP_FILE)).unwrap();
        assert_eq!(
            open_backup(&dir, &code).err(),
            Some(RecoverError::MissingFile(IDENTITY_BACKUP_FILE))
        );
    }

    // A rolled-back vault is refused before anything is written.
    #[test]
    fn refuses_a_rolled_back_vault_and_writes_nothing() {
        let world = World::new("rollback");
        let (vault, code) = world.init("diary");
        let id = world.vault_dir(&vault);
        let opened = open_backup(&id, &code).unwrap();
        generation::store(world.state.fd(), Counter::Vault(opened.vault_id), 9).unwrap();
        world.forget();
        let identity = fs::read(vault.join(IDENTITY_FILE)).unwrap();
        let fresh = config::fresh(&world.state).unwrap();
        let err = world
            .recover("diary", &vault, &code, NEW, fresh)
            .unwrap_err();
        assert!(
            matches!(err, RecoverError::Generation(GenError::Rollback { .. })),
            "{err:?}"
        );
        assert_eq!(fs::read(vault.join(IDENTITY_FILE)).unwrap(), identity);
        assert!(!world.config_file().exists());
    }

    // NAME taken by another vault in the config that opens: refused before
    // anything is written.
    #[test]
    fn refuses_a_taken_name_and_writes_nothing() {
        let world = World::new("taken");
        let (diary, code) = world.init("diary");
        world.init("other");
        let (mut cfg, _) = config::load(&world.config, &world.state, &pass("pw"))
            .unwrap()
            .unwrap();
        cfg.remove(&name("diary")).unwrap();
        let identity = fs::read(diary.join(IDENTITY_FILE)).unwrap();
        let err = world
            .recover("other", &diary, &code, "pw", cfg)
            .unwrap_err();
        assert_eq!(err, RecoverError::Config(ConfigError::NameTaken));
        assert_eq!(fs::read(diary.join(IDENTITY_FILE)).unwrap(), identity);
    }

    #[test]
    fn errors_never_show_paths_names_or_codes() {
        let code = RecoveryCode::generate().unwrap();
        let grouped = code.grouped();
        for err in [
            RecoverError::WrongCode,
            RecoverError::NotReplaced,
            RecoverError::MissingFile(IDENTITY_BACKUP_FILE),
            RecoverError::TooLarge(IDENTITY_BACKUP_FILE),
            RecoverError::Code(CodeError::Mistyped),
        ] {
            let text = err.to_string();
            assert!(!text.contains('/') && !text.contains("diary"), "{text}");
            assert!(!text.contains(grouped.as_str()), "{text}");
        }
    }
}
