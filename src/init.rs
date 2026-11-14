//! `crystal init NAME` (docs/design/storage.md, "Key design"; decision
//! 0032): create a vault in `~/.local/share/crystal/NAME`.
//!
//! Everything that can be checked is checked before anything is created:
//! the vault directory must not exist; the passphrase is asked for and the
//! recovery code is shown and typed back
//! (docs/decisions/0029-recovery-code.md). Only then are the directory and
//! files made. If a later step fails, exactly what this run created is
//! removed again, never anything that existed before.

use std::fmt;
use std::os::fd::OwnedFd;

use rustix::fs::{AtFlags, Mode, mkdirat, openat, statat, unlinkat};
use rustix::io::Errno;
use zeroize::Zeroizing;

use crate::crypto::{CryptoError, Passphrase, VaultIdentity};
use crate::model::VaultId;
use crate::recovery::{CodeError, RecoveryCode};
use crate::term::{PromptError, Signals, TerminalGuard};
use crate::vault::atomic::{FileName, Payload, WriteError, atomic_write};
use crate::vault::codec::VaultContents;
use crate::vault::format::{
    self, IDENTITY_BACKUP_FILE, IDENTITY_FILE, VAULT_FILE, VERSION_FILE, VaultError,
};
use crate::vault::generation::{self, Counter, GenError};
use crate::vault::pin::{self, PinError};
use crate::vault::vault_dir::{OpenVaultError, VaultDir};
use crate::vault::{DataDir, OPEN_DIR_FLAGS, StateDir, VaultName};

/// Every file `crystal init` creates in the vault directory, in order.
pub const VAULT_FILES: [&str; 4] = [
    VERSION_FILE,
    IDENTITY_FILE,
    IDENTITY_BACKUP_FILE,
    VAULT_FILE,
];

/// Why `crystal init` stopped. Fixed text; never a path, name or key.
#[derive(Debug, PartialEq, Eq)]
pub enum InitError {
    /// The vault directory already exists.
    Exists,
    Path(OpenVaultError),
    Stat(Errno),
    Create(Errno),
    Random(Errno),
    Crypto(CryptoError),
    Vault(VaultError),
    Write(WriteError),
    Pin(PinError),
    Generation(GenError),
    Prompt(PromptError),
    /// A step after the directory was created failed; everything this run
    /// created was removed again.
    RolledBack(Box<InitError>),
    /// As `RolledBack`, but removing failed: the partly created vault
    /// directory is still there.
    CleanupFailed(Box<InitError>),
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exists => write!(f, "a vault with that name already exists"),
            Self::Path(e) => e.fmt(f),
            Self::Stat(errno) => write!(f, "cannot check the vault directory ({errno})"),
            Self::Create(errno) => write!(f, "cannot create the vault directory ({errno})"),
            Self::Random(errno) => write!(f, "cannot get random bytes ({errno})"),
            Self::Crypto(e) => e.fmt(f),
            Self::Vault(e) => e.fmt(f),
            Self::Write(e) => write!(f, "cannot write a vault file: {e}"),
            Self::Pin(e) => e.fmt(f),
            Self::Generation(e) => e.fmt(f),
            Self::Prompt(e) => e.fmt(f),
            Self::RolledBack(e) => {
                write!(f, "{e}; the partly created vault was removed")
            }
            Self::CleanupFailed(e) => write!(
                f,
                "{e}; removing the partly created vault directory also failed, \
                 remove it by hand"
            ),
        }
    }
}

impl std::error::Error for InitError {}

/// A checked target for a new vault; nothing has been created yet.
pub struct Plan {
    dir_name: String,
}

/// Step 1: the vault directory `NAME` must not exist in the data
/// directory, not even as a symlink.
pub fn plan(data: &DataDir, name: &VaultName) -> Result<Plan, InitError> {
    match statat(data.fd(), name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => Ok(Plan {
            dir_name: name.as_str().to_owned(),
        }),
        Ok(_) => Err(InitError::Exists),
        Err(errno) => Err(InitError::Stat(errno)),
    }
}

/// Shows the new vault's recovery code once and has it typed back, so a
/// copying mistake is found now, not on the day it is needed. A wrong
/// entry shows the code again. The code is on the alternate screen only,
/// which the next prompt clears; every copy is wiped when dropped.
pub fn confirm_recovery_code(
    terminal: &mut TerminalGuard,
    signals: &Signals,
    code: &RecoveryCode,
) -> Result<(), InitError> {
    let mut note = "";
    loop {
        let grouped = code.grouped();
        let shown = Zeroizing::new(format!(
            "{note}Recovery code for this vault. Write it on paper and keep it away from this \
             computer.\r\nIt opens the vault if you forget the passphrase; anyone who has it \
             can open the vault too.\r\n\r\n    {}\r\n\r\n{SHOWN_PROMPT}",
            grouped.as_str()
        ));
        // What is typed here does not matter; it is wiped on drop.
        terminal
            .read_public_line(signals, &shown)
            .map_err(InitError::Prompt)?;
        let typed = terminal
            .read_public_line(signals, CONFIRM_PROMPT)
            .map_err(InitError::Prompt)?;
        match RecoveryCode::parse(&typed) {
            Ok(back) if back.passphrase().matches(&code.passphrase()) => return Ok(()),
            Ok(_) => note = "That is not the code shown; here it is again.\r\n\r\n",
            Err(e) => {
                note = match e {
                    CodeError::WrongLength => {
                        "That has the wrong length (7 groups of 4); here is the code again.\r\n\r\n"
                    }
                    CodeError::BadCharacter | CodeError::Mistyped => {
                        "That has a typo; here is the code again.\r\n\r\n"
                    }
                }
            }
        }
    }
}

/// The last line of the screen that shows the recovery code.
pub const SHOWN_PROMPT: &str = "Press Enter once you have written it down.";
/// The prompt to type the recovery code back.
pub const CONFIRM_PROMPT: &str = "Type the recovery code to confirm: ";

/// What this run created, to remove again if a later step fails.
#[derive(Default)]
struct Created {
    vault_dir: Option<OwnedFd>,
    pinned: bool,
}

/// Step 2: creates the vault as planned, pins it to its name and starts
/// its rollback counter at generation 1. All or nothing.
pub fn create(
    plan: Plan,
    name: &VaultName,
    pass: &Passphrase,
    recovery: &RecoveryCode,
    data: &DataDir,
    state: &StateDir,
) -> Result<(), InitError> {
    let mut created = Created::default();
    match build(&plan, name, pass, recovery, data, state, &mut created) {
        Ok(()) => Ok(()),
        Err(e) => match remove_created(&plan, name, data, state, created) {
            Ok(()) => Err(InitError::RolledBack(Box::new(e))),
            Err(()) => Err(InitError::CleanupFailed(Box::new(e))),
        },
    }
}

fn build(
    plan: &Plan,
    name: &VaultName,
    pass: &Passphrase,
    recovery: &RecoveryCode,
    data: &DataDir,
    state: &StateDir,
    created: &mut Created,
) -> Result<(), InitError> {
    mkdirat(data.fd(), plan.dir_name.as_str(), Mode::RWXU).map_err(|errno| match errno {
        Errno::EXIST => InitError::Exists,
        other => InitError::Create(other),
    })?;
    let dir = openat(
        data.fd(),
        plan.dir_name.as_str(),
        OPEN_DIR_FLAGS,
        Mode::empty(),
    )
    .map_err(InitError::Create)?;
    let dir = created.vault_dir.insert(dir);

    format::write_version(dir).map_err(InitError::Write)?;
    let identity = VaultIdentity::generate().map_err(InitError::Crypto)?;
    let vault_id = VaultId::random().map_err(InitError::Random)?;
    let write = |file: &str, sealed: &crate::crypto::Ciphertext| {
        // The four names are fixed and valid: this cannot fail.
        let name = FileName::new(file).ok_or(InitError::Create(Errno::INVAL))?;
        atomic_write(dir, &name, Payload::Ciphertext(sealed)).map_err(InitError::Write)
    };
    write(
        IDENTITY_FILE,
        &identity
            .seal_with_passphrase(pass)
            .map_err(InitError::Crypto)?,
    )?;
    write(
        IDENTITY_BACKUP_FILE,
        &identity
            .seal_with_passphrase(&recovery.passphrase())
            .map_err(InitError::Crypto)?,
    )?;
    let empty = VaultContents {
        vault_id,
        generation: 1,
        notes: Vec::new(),
    };
    write(
        VAULT_FILE,
        &format::seal(&empty, &identity.vault_recipient()).map_err(InitError::Vault)?,
    )?;

    // The finished directory must pass the same check as any vault.
    VaultDir::open(data, name).map_err(InitError::Path)?;
    created.pinned = true;
    pin::store(state.fd(), name, &vault_id).map_err(InitError::Pin)?;
    generation::record_save(state.fd(), Counter::Vault(vault_id), 1)
        .map_err(InitError::Generation)?;
    Ok(())
}

/// Removes what `build` created: the files in the new vault directory,
/// the directory, and the pin. The directory was created by this run, so
/// removing it never touches anything older. A rollback counter for the
/// new, random vault id is harmless and stays if it was written.
fn remove_created(
    plan: &Plan,
    name: &VaultName,
    data: &DataDir,
    state: &StateDir,
    created: Created,
) -> Result<(), ()> {
    let mut ok = true;
    if let Some(dir) = created.vault_dir {
        for file in VAULT_FILES {
            match unlinkat(&dir, file, AtFlags::empty()) {
                Ok(()) | Err(Errno::NOENT) => {}
                Err(_) => ok = false,
            }
        }
        drop(dir);
        ok &= unlinkat(data.fd(), plan.dir_name.as_str(), AtFlags::REMOVEDIR).is_ok();
    }
    if created.pinned {
        ok &= pin::remove(state.fd(), name).is_ok();
    }
    if ok { Ok(()) } else { Err(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::decrypt_with_identity;
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

    fn mode(p: &Path) -> u32 {
        fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
    }

    fn set_mode(p: &Path, mode: u32) {
        fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
    }

    struct World {
        home: PathBuf,
        data: DataDir,
        state: StateDir,
    }

    impl World {
        fn new(test: &str) -> Self {
            let home = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/unit-init")
                .join(test);
            let _ = fs::remove_dir_all(&home);
            fs::create_dir_all(&home).unwrap();
            set_mode(&home, 0o700);
            let data = DataDir::create(Some(home.as_os_str())).unwrap();
            let state = StateDir::open_or_create(Some(home.as_os_str())).unwrap();
            Self { home, data, state }
        }

        fn data_path(&self) -> PathBuf {
            self.home.join(".local/share/crystal")
        }

        fn state_path(&self) -> PathBuf {
            self.home.join(".local/state/crystal")
        }

        fn create(&self, n: &str, code: &RecoveryCode) -> Result<(), InitError> {
            let plan = plan(&self.data, &name(n))?;
            create(plan, &name(n), &pass("pw"), code, &self.data, &self.state)
        }
    }

    // Acceptance: the layout matches docs/design/storage.md exactly, and
    // identity.backup.age recovers the vault without the passphrase.
    #[test]
    fn creates_the_documented_layout() {
        let world = World::new("layout");
        let code = RecoveryCode::generate().unwrap();
        world.create("diary", &code).unwrap();

        let vault = world.data_path().join("diary");
        assert_eq!((mode(&world.data_path()), mode(&vault)), (0o700, 0o700));
        let mut files: Vec<String> = fs::read_dir(&vault)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        files.sort();
        assert_eq!(
            files,
            [
                "VERSION",
                "identity.age",
                "identity.backup.age",
                "vault.age"
            ]
        );
        for file in &files {
            assert_eq!(mode(&vault.join(file)), 0o600, "{file}");
        }
        assert_eq!(fs::read(vault.join("VERSION")).unwrap(), b"2\n");

        // The vault is pinned and its counter is at generation 1.
        let id = pin::load(world.state.fd(), &name("diary"))
            .unwrap()
            .unwrap();
        assert_eq!(
            generation::load(world.state.fd(), Counter::Vault(id)),
            Ok(Some(1))
        );
        let mut state: Vec<String> = fs::read_dir(world.state_path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        // Sorted by a fixed key: the random id may sort either side of
        // the pin.
        state.sort_by_key(|f| f.ends_with(".pin"));
        assert_eq!(state, [format!("{}.gen", id.hex()), "diary.pin".to_owned()]);

        // The passphrase opens identity.age, and that opens vault.age.
        let identity = VaultIdentity::open_with_passphrase(
            &fs::read(vault.join("identity.age")).unwrap(),
            &pass("pw"),
        )
        .unwrap();
        let contents = format::open(
            &fs::read(vault.join("vault.age")).unwrap(),
            &identity,
            crate::vault::codec::FORMAT,
        );
        assert!(contents.unwrap().vault_id == id);

        // Recovery without the passphrase: the recovery code opens
        // identity.backup.age, and the identity inside opens vault.age.
        let recovered = VaultIdentity::open_with_passphrase(
            &fs::read(vault.join("identity.backup.age")).unwrap(),
            &code.passphrase(),
        )
        .unwrap();
        let vault_age = fs::read(vault.join("vault.age")).unwrap();
        assert!(decrypt_with_identity(&vault_age, &recovered).is_ok());
        // As any age tool opens it (decision 0029): the shown code without
        // dashes is the scrypt passphrase, given as plain bytes to the age
        // code, past Crystal's code parser; inside is an age secret key
        // string for the vault's recipient.
        let typed = Zeroizing::new(code.grouped().replace('-', "").into_bytes());
        let by_age = crate::age_v1::Identity::Passphrase {
            passphrase: &typed,
            max_log_n: 22,
        };
        let backup = fs::read(vault.join("identity.backup.age")).unwrap();
        let secret = crate::age_v1::decrypt(&by_age, &backup).unwrap();
        let secret = std::str::from_utf8(&secret).unwrap();
        let key = crate::age_v1::x25519::Identity::parse(secret).unwrap();
        assert_eq!(
            key.to_public().to_string(),
            recovered.recipient().to_string()
        );
        // The passphrase does not open it.
        assert!(
            VaultIdentity::open_with_passphrase(
                &fs::read(vault.join("identity.backup.age")).unwrap(),
                &pass("pw"),
            )
            .is_err()
        );
    }

    #[test]
    fn plan_refuses_an_existing_name_without_creating_anything() {
        let world = World::new("plan");
        fs::create_dir(world.data_path().join("taken")).unwrap();
        assert_eq!(
            plan(&world.data, &name("taken")).err(),
            Some(InitError::Exists)
        );
        symlink("/nowhere", world.data_path().join("link")).unwrap();
        assert_eq!(
            plan(&world.data, &name("link")).err(),
            Some(InitError::Exists),
            "a dangling symlink at the name exists too"
        );
        fs::write(world.data_path().join("file"), b"").unwrap();
        assert_eq!(
            plan(&world.data, &name("file")).err(),
            Some(InitError::Exists)
        );
        assert!(plan(&world.data, &name("free")).is_ok());
        assert!(!world.data_path().join("free").exists());
    }

    // A failure after the directory exists (here: the pin cannot be
    // written) removes everything this run created, and the data
    // directory and other vaults stay.
    #[test]
    fn late_failure_removes_what_was_created() {
        let world = World::new("rollback");
        let code = RecoveryCode::generate().unwrap();
        world.create("other", &code).unwrap();
        set_mode(&world.state_path(), 0o500);
        let err = world.create("diary", &code).unwrap_err();
        set_mode(&world.state_path(), 0o700);
        assert!(
            matches!(&err, InitError::RolledBack(e) if matches!(**e, InitError::Pin(_))),
            "{err:?}"
        );
        assert!(
            err.to_string()
                .ends_with("the partly created vault was removed")
        );
        assert!(!world.data_path().join("diary").exists());
        assert!(world.data_path().join("other/vault.age").exists());
        assert!(!world.state_path().join("diary.pin").exists());
    }

    // A directory fsync failing for a vault file is an ordinary late
    // failure: everything is removed.
    #[test]
    fn vault_file_not_synced_rolls_back() {
        use crate::vault::atomic::{FailAt, Step};
        let world = World::new("dirfsync_vault");
        let code = RecoveryCode::generate().unwrap();
        let err = {
            let _armed = FailAt::arm_for(Step::DirFsync, "vault.age");
            world.create("diary", &code).unwrap_err()
        };
        assert!(matches!(err, InitError::RolledBack(_)), "{err:?}");
        assert!(!world.data_path().join("diary").exists());
        assert!(!world.state_path().join("diary.pin").exists());
    }

    // If undoing fails (here: a file the run did not make is in the new
    // vault directory, so it cannot be removed), the error says so instead
    // of claiming the vault was removed.
    #[test]
    fn failed_cleanup_is_reported() {
        let world = World::new("cleanup_fails");
        let plan = plan(&world.data, &name("diary")).unwrap();
        let code = RecoveryCode::generate().unwrap();
        let mut created = Created::default();
        set_mode(&world.state_path(), 0o500);
        let err = build(
            &plan,
            &name("diary"),
            &pass("pw"),
            &code,
            &world.data,
            &world.state,
            &mut created,
        )
        .unwrap_err();
        set_mode(&world.state_path(), 0o700);
        assert!(matches!(err, InitError::Pin(_)), "{err:?}");
        let vault = world.data_path().join("diary");
        fs::write(vault.join("stray"), b"x").unwrap();
        assert_eq!(
            remove_created(&plan, &name("diary"), &world.data, &world.state, created),
            Err(())
        );
        assert!(vault.join("stray").exists());
        assert!(
            !vault.join("vault.age").exists(),
            "its own files are still removed"
        );

        let shown = InitError::CleanupFailed(Box::new(err)).to_string();
        assert!(shown.ends_with("remove it by hand"), "{shown}");
    }

    #[test]
    fn errors_never_show_the_name() {
        let world = World::new("errors");
        let code = RecoveryCode::generate().unwrap();
        world.create("diary", &code).unwrap();
        let err = world.create("diary", &code).unwrap_err();
        assert_eq!(err, InitError::Exists);
        assert!(!err.to_string().contains("diary"));
    }
}
