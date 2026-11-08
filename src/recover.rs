//! `crystal recover NAME` and `crystal check-code NAME`
//! (docs/decisions/0029-recovery-code.md, 0032): the recovery code opens
//! `identity.backup.age`.
//!
//! - `recover`: when the passphrase is forgotten (or `identity.age` is
//!   lost), the vault key from the backup is sealed again with a new
//!   passphrase into `identity.age`. Nothing else changes.
//! - `check-code`: checks, without writing anything, that the code on
//!   paper opens the backup and that it holds the same key as
//!   `identity.age`.
//!
//! The prompts are in `main`; this module does the rest, so the unit tests
//! can drive it without a terminal.

use std::fmt;

use crate::crypto::{self, CryptoError, Passphrase, VaultIdentity};
use crate::model::VaultId;
use crate::recovery::{CodeError, RecoveryCode};
use crate::vault::atomic::{FileName, Payload, WriteError, atomic_write};
use crate::vault::format::{
    self, IDENTITY_BACKUP_FILE, IDENTITY_FILE, MAX_IDENTITY_FILE, VAULT_FILE, VaultError,
};
use crate::vault::generation::{self, Counter, GenError, Older};
use crate::vault::pin::{self, PinError};
use crate::vault::public_file::{ReadError, read_bounded};
use crate::vault::vault_dir::VaultDir;
use crate::vault::{StateDir, VaultName};

/// The prompt for the recovery code. Shown as typed, like at `crystal init`.
pub const CODE_PROMPT: &str = "Recovery code (written down at crystal init): ";

/// Why `crystal recover` or `check-code` stopped. Fixed text; never a
/// path, name, code or key.
#[derive(Debug, PartialEq, Eq)]
pub enum RecoverError {
    Code(CodeError),
    /// The code is well formed but does not open `identity.backup.age`.
    WrongCode,
    Read(ReadError),
    /// `identity.age`, `identity.backup.age` or `vault.age` is missing.
    MissingFile(&'static str),
    TooLarge(&'static str),
    Identity(CryptoError),
    /// `check-code`: the passphrase does not open `identity.age`.
    WrongPassphrase,
    /// `check-code`: the backup holds another key than `identity.age`.
    KeysDiffer,
    Vault(VaultError),
    Pin(PinError),
    Generation(GenError),
    Write(WriteError),
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
            Self::WrongPassphrase => write!(
                f,
                "the recovery code is right, but the vault's identity.age does not open with \
                 this passphrase"
            ),
            Self::KeysDiffer => write!(
                f,
                "identity.backup.age and identity.age hold different keys: the recovery code \
                 opens a key that does not open this vault"
            ),
            Self::Vault(e) => write!(f, "vault.age: {e}"),
            Self::Pin(e) => e.fmt(f),
            Self::Generation(e) => e.fmt(f),
            Self::Write(e) => write!(f, "cannot write identity.age: {e}"),
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

/// Reads one vault file, refusing a missing or oversized one.
fn read(vault_dir: &VaultDir, file: &'static str, max: usize) -> Result<Vec<u8>, RecoverError> {
    // read_bounded reads one byte past `max` to show the file is longer.
    let bytes = read_bounded(vault_dir.fd(), file, max)
        .map_err(RecoverError::Read)?
        .ok_or(RecoverError::MissingFile(file))?;
    if bytes.len() > max {
        return Err(RecoverError::TooLarge(file));
    }
    Ok(bytes)
}

/// Opens `identity.backup.age` with `code`, then `vault.age` with the key
/// inside, to learn the vault's id and generation. Nothing is written.
pub fn open_backup(vault_dir: &VaultDir, code: &RecoveryCode) -> Result<Opened, RecoverError> {
    let backup = read(vault_dir, IDENTITY_BACKUP_FILE, MAX_IDENTITY_FILE)?;
    let identity =
        VaultIdentity::open_with_passphrase(&backup, &code.passphrase()).map_err(|e| match e {
            CryptoError::WrongPassphrase => RecoverError::WrongCode,
            e => RecoverError::Identity(e),
        })?;
    let contents = format::open(
        &read(vault_dir, VAULT_FILE, crypto::MAX_CIPHERTEXT)?,
        &identity,
        vault_dir.version(),
    )
    .map_err(RecoverError::Vault)?;
    Ok(Opened {
        identity,
        vault_id: contents.vault_id,
        generation: contents.generation,
    })
}

/// `recover`, before the new passphrase is asked for: the vault must be
/// the one pinned to `name` (or is pinned now). An older vault than seen
/// here is reported, with the counter unchanged, for the user to accept
/// with `restore` (`accept_restore`) or not.
pub fn check_seen(
    opened: &Opened,
    name: &VaultName,
    state: &StateDir,
) -> Result<Option<Older>, RecoverError> {
    pin::check(state.fd(), name, &opened.vault_id).map_err(RecoverError::Pin)?;
    let result = generation::open_or_older(
        state.fd(),
        Counter::Vault(opened.vault_id),
        opened.generation,
    )
    .map_err(RecoverError::Generation)?;
    Ok(result.err())
}

/// The user typed `restore`: the counter goes back to this vault's
/// generation.
pub fn accept_restore(opened: &Opened, state: &StateDir, older: Older) -> Result<(), RecoverError> {
    generation::restore(state.fd(), Counter::Vault(opened.vault_id), older)
        .map_err(RecoverError::Generation)
}

/// `recover`: writes a new `identity.age`, the vault key sealed with
/// `pass`. The only file that changes.
pub fn finish(opened: Opened, pass: &Passphrase, vault_dir: &VaultDir) -> Result<(), RecoverError> {
    let sealed = opened
        .identity
        .seal_with_passphrase(pass)
        .map_err(RecoverError::Identity)?;
    // A fixed, valid name: FileName::new cannot refuse it.
    let file = FileName::new(IDENTITY_FILE).ok_or(RecoverError::MissingFile(IDENTITY_FILE))?;
    atomic_write(vault_dir.fd(), &file, Payload::Ciphertext(&sealed)).map_err(RecoverError::Write)
}

/// `check-code`: `pass` must open `identity.age`, and it must hold the
/// same key as the backup. Writes nothing.
pub fn check_code(
    opened: &Opened,
    pass: &Passphrase,
    vault_dir: &VaultDir,
) -> Result<(), RecoverError> {
    let daily = read(vault_dir, IDENTITY_FILE, MAX_IDENTITY_FILE)?;
    let identity = VaultIdentity::open_with_passphrase(&daily, pass).map_err(|e| match e {
        CryptoError::WrongPassphrase => RecoverError::WrongPassphrase,
        e => RecoverError::Identity(e),
    })?;
    // Public keys: comparing them reveals nothing secret.
    if identity.recipient().to_string() == opened.identity.recipient().to_string() {
        Ok(())
    } else {
        Err(RecoverError::KeysDiffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init;
    use crate::unlock;
    use crate::vault::DataDir;
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

    struct World {
        home: PathBuf,
        data: DataDir,
        state: StateDir,
    }

    impl World {
        fn new(test: &str) -> Self {
            let home = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/unit-recover")
                .join(test);
            let _ = fs::remove_dir_all(&home);
            fs::create_dir_all(&home).unwrap();
            fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
            let data = DataDir::create(Some(home.as_os_str())).unwrap();
            let state = StateDir::open_or_create(Some(home.as_os_str())).unwrap();
            Self { home, data, state }
        }

        /// A vault `n` with passphrase "pw"; returns its recovery code.
        fn init(&self, n: &str) -> RecoveryCode {
            let code = RecoveryCode::generate().unwrap();
            let plan = init::plan(&self.data, &name(n)).unwrap();
            init::create(plan, &name(n), &pass("pw"), &code, &self.data, &self.state).unwrap();
            code
        }

        fn vault(&self, n: &str) -> PathBuf {
            self.home.join(".local/share/crystal").join(n)
        }

        fn dir(&self, n: &str) -> VaultDir {
            VaultDir::open(&self.data, &name(n)).unwrap()
        }

        fn unlocks(&self, n: &str, pw: &str) -> bool {
            let dir = unlock::open_vault_dir(&self.data, &name(n)).unwrap();
            unlock::unlock(dir, &name(n), pass(pw), &self.state).is_ok()
        }

        /// The whole `recover` flow with a new passphrase.
        fn recover(&self, n: &str, code: &RecoveryCode, new: &str) -> Result<(), RecoverError> {
            let dir = self.dir(n);
            let opened = open_backup(&dir, code)?;
            if let Some(older) = check_seen(&opened, &name(n), &self.state)? {
                accept_restore(&opened, &self.state, older)?;
            }
            finish(opened, &pass(new), &dir)
        }
    }

    #[test]
    fn recovers_a_forgotten_passphrase() {
        let world = World::new("forgotten");
        let code = world.init("diary");
        let before = fs::read(world.vault("diary").join(VAULT_FILE)).unwrap();
        world.recover("diary", &code, "new-pw").unwrap();
        assert!(world.unlocks("diary", "new-pw"));
        assert!(!world.unlocks("diary", "pw"));
        // Only identity.age changed; the code still works.
        assert_eq!(
            fs::read(world.vault("diary").join(VAULT_FILE)).unwrap(),
            before
        );
        world.recover("diary", &code, "third-pw").unwrap();
        assert!(world.unlocks("diary", "third-pw"));
    }

    #[test]
    fn restores_a_lost_identity() {
        let world = World::new("lost_identity");
        let code = world.init("diary");
        fs::remove_file(world.vault("diary").join(IDENTITY_FILE)).unwrap();
        world.recover("diary", &code, "pw").unwrap();
        assert!(world.unlocks("diary", "pw"));
    }

    #[test]
    fn refuses_another_vaults_code() {
        let world = World::new("other_code");
        world.init("diary");
        let other = world.init("other");
        assert_eq!(
            world.recover("diary", &other, "new-pw").err(),
            Some(RecoverError::WrongCode)
        );
        assert!(world.unlocks("diary", "pw"));
    }

    // identity.backup.age and vault.age swapped in from another vault: the
    // code opens them, but the pin refuses the vault, and nothing is
    // written.
    #[test]
    fn refuses_a_swapped_vault_and_writes_nothing() {
        let world = World::new("swapped");
        world.init("diary");
        let other = world.init("other");
        for file in [IDENTITY_BACKUP_FILE, VAULT_FILE] {
            fs::copy(
                world.vault("other").join(file),
                world.vault("diary").join(file),
            )
            .unwrap();
        }
        let identity = fs::read(world.vault("diary").join(IDENTITY_FILE)).unwrap();
        assert_eq!(
            world.recover("diary", &other, "new-pw").err(),
            Some(RecoverError::Pin(PinError::Mismatch))
        );
        assert_eq!(
            fs::read(world.vault("diary").join(IDENTITY_FILE)).unwrap(),
            identity
        );
    }

    // An older vault (restored from a backup) is reported before anything
    // is written; once accepted, recover goes on and the counter follows.
    #[test]
    fn reports_an_older_vault_before_writing() {
        let world = World::new("older");
        let code = world.init("diary");
        let id = pin::load(world.state.fd(), &name("diary"))
            .unwrap()
            .unwrap();
        generation::store(world.state.fd(), Counter::Vault(id), 9).unwrap();
        let identity = fs::read(world.vault("diary").join(IDENTITY_FILE)).unwrap();
        let opened = open_backup(&world.dir("diary"), &code).unwrap();
        let older = check_seen(&opened, &name("diary"), &world.state).unwrap();
        assert_eq!(older, Some(Older { seen: 1, stored: 9 }));
        assert_eq!(
            fs::read(world.vault("diary").join(IDENTITY_FILE)).unwrap(),
            identity
        );
        assert_eq!(
            generation::load(world.state.fd(), Counter::Vault(id)),
            Ok(Some(9))
        );
        world.recover("diary", &code, "new-pw").unwrap();
        assert_eq!(
            generation::load(world.state.fd(), Counter::Vault(id)),
            Ok(Some(1))
        );
        assert!(world.unlocks("diary", "new-pw"));
    }

    #[test]
    fn refuses_missing_or_oversized_backup() {
        let world = World::new("backup_files");
        let code = world.init("diary");
        let backup = world.vault("diary").join(IDENTITY_BACKUP_FILE);
        fs::write(&backup, vec![b'x'; MAX_IDENTITY_FILE + 1]).unwrap();
        assert_eq!(
            open_backup(&world.dir("diary"), &code).err(),
            Some(RecoverError::TooLarge(IDENTITY_BACKUP_FILE))
        );
        fs::remove_file(&backup).unwrap();
        assert_eq!(
            open_backup(&world.dir("diary"), &code).err(),
            Some(RecoverError::MissingFile(IDENTITY_BACKUP_FILE))
        );
    }

    #[test]
    fn check_code_finds_each_problem() {
        let world = World::new("check_code");
        let code = world.init("diary");
        let other = world.init("other");
        let dir = world.dir("diary");
        let opened = open_backup(&dir, &code).unwrap();
        assert_eq!(check_code(&opened, &pass("pw"), &dir), Ok(()));
        assert_eq!(
            check_code(&opened, &pass("not-pw"), &dir),
            Err(RecoverError::WrongPassphrase)
        );
        assert_eq!(
            open_backup(&dir, &other).err(),
            Some(RecoverError::WrongCode)
        );
        // identity.age of another vault under the same passphrase.
        fs::copy(
            world.vault("other").join(IDENTITY_FILE),
            world.vault("diary").join(IDENTITY_FILE),
        )
        .unwrap();
        assert_eq!(
            check_code(&opened, &pass("pw"), &dir),
            Err(RecoverError::KeysDiffer)
        );
    }

    #[test]
    fn errors_never_show_paths_names_or_codes() {
        let code = RecoveryCode::generate().unwrap();
        let grouped = code.grouped();
        let errors = [
            RecoverError::WrongCode,
            RecoverError::MissingFile(IDENTITY_BACKUP_FILE),
            RecoverError::TooLarge(VAULT_FILE),
            RecoverError::WrongPassphrase,
            RecoverError::KeysDiffer,
            RecoverError::Pin(PinError::Mismatch),
        ];
        for e in errors {
            let shown = e.to_string();
            assert!(!shown.contains(grouped.as_str()), "{shown}");
            assert!(!shown.contains("diary"), "{shown}");
        }
    }
}
