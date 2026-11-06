//! `crystal init NAME [--at DIR]` (docs/design/storage.md, "Key design",
//! "On-disk layout"): create a vault and register it.
//!
//! Everything that can be checked is checked before anything is created:
//! the target is resolved, must not exist, and its parent must pass the
//! vault path walk; the name must be new in the config; the passphrase and
//! backup recipients are asked for. Only then are the directory and files
//! made. If a later step fails, exactly what this run created is removed
//! again, never anything that existed before.

use std::ffi::OsStr;
use std::fmt;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::str::FromStr;

use age::x25519;
use rustix::fs::{AtFlags, Mode, mkdirat, openat, statat, unlinkat};
use rustix::io::Errno;

use crate::config::{self, Config, ConfigError, VaultEntry};
use crate::crypto::{CryptoError, Passphrase, VaultIdentity};
use crate::model::VaultId;
use crate::term::{PromptError, Signals, TerminalGuard};
use crate::vault::atomic::{FileName, Payload, WriteError, atomic_write};
use crate::vault::codec::VaultContents;
use crate::vault::format::{self, VaultError};
use crate::vault::generation::{self, Counter, GenError};
use crate::vault::vault_dir::{OpenVaultError, VaultDir, components, open_parent_checked};
use crate::vault::{ConfigDir, OPEN_DIR_FLAGS, StateDir, VaultName};

/// The default parent of new vaults, inside HOME.
const VAULTS_DIR: &str = "Vaults";
/// Every file `crystal init` creates in the vault directory, in order.
const VAULT_FILES: [&str; 4] = [
    "VERSION",
    "identity.age",
    "identity.backup.age",
    "vault.age",
];
/// More backup recipients than anyone needs; bounds the prompt loop.
pub const MAX_BACKUP_RECIPIENTS: usize = 8;

/// Why `crystal init` stopped. Fixed text; never a path, name or key.
#[derive(Debug, PartialEq, Eq)]
pub enum InitError {
    /// `--at DIR` ends in `.`, `..` or `/`, or is not valid UTF-8.
    BadTarget,
    /// The target or its parent could not be resolved.
    Resolve(std::io::ErrorKind),
    /// The vault directory already exists.
    Exists,
    Path(OpenVaultError),
    Stat(Errno),
    Create(Errno),
    Random(Errno),
    Crypto(CryptoError),
    Vault(VaultError),
    Write(WriteError),
    Config(ConfigError),
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
            Self::BadTarget => write!(
                f,
                "--at must name the new vault directory itself, as valid UTF-8"
            ),
            Self::Resolve(kind) => {
                write!(f, "cannot resolve where the vault should go ({kind})")
            }
            Self::Exists => write!(f, "the vault directory already exists"),
            Self::Path(e) => e.fmt(f),
            Self::Stat(errno) => write!(f, "cannot check the vault directory ({errno})"),
            Self::Create(errno) => write!(f, "cannot create the vault directory ({errno})"),
            Self::Random(errno) => write!(f, "cannot get random bytes ({errno})"),
            Self::Crypto(e) => e.fmt(f),
            Self::Vault(e) => e.fmt(f),
            Self::Write(e) => write!(f, "cannot write a vault file: {e}"),
            Self::Config(e) => e.fmt(f),
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

/// Where the parent of the new vault directory stands.
enum Parent {
    /// It exists and passed the walk.
    Exists(OwnedFd),
    /// `~/Vaults` does not exist yet: it is created in HOME (which passed
    /// the walk) when the vault is.
    CreateIn { home: OwnedFd },
}

/// A checked target for a new vault; nothing has been created yet.
pub struct Plan {
    parent: Parent,
    dir_name: String,
    /// The resolved path the config will store.
    path: String,
}

/// Resolves `p` to a real path in UTF-8.
fn resolve(p: &Path) -> Result<String, InitError> {
    std::fs::canonicalize(p)
        .map_err(|e| InitError::Resolve(e.kind()))?
        .into_os_string()
        .into_string()
        .map_err(|_| InitError::BadTarget)
}

/// Step 1: where the vault goes, checked, with nothing created. Without
/// `--at`, `~/Vaults/NAME` (with HOME resolved, so the stored path has no
/// symlink); with `--at DIR`, DIR itself, which must not exist yet.
pub fn plan(
    home: &OsStr,
    at: Option<&Path>,
    name: &VaultName,
    config_dir: &ConfigDir,
    state: &StateDir,
) -> Result<Plan, InitError> {
    // `resolved_home` is set only for the default location, where a
    // missing ~/Vaults is created in it.
    let (parent_path, dir_name, resolved_home) = match at {
        None => {
            let home = resolve(Path::new(home))?;
            (
                format!("{home}/{VAULTS_DIR}"),
                name.as_str().to_owned(),
                Some(home),
            )
        }
        Some(dir) => {
            let dir_name = dir
                .file_name()
                .and_then(OsStr::to_str)
                .filter(|_| !dir.as_os_str().as_encoded_bytes().ends_with(b"/"))
                .ok_or(InitError::BadTarget)?
                .to_owned();
            let parent = match dir.parent() {
                Some(p) if !p.as_os_str().is_empty() => p,
                _ => Path::new("."),
            };
            (resolve(parent)?, dir_name, None)
        }
    };
    let path = format!("{parent_path}/{dir_name}");
    components(path.as_bytes()).map_err(InitError::Path)?;

    let parent = match open_parent_checked(parent_path.as_ref(), config_dir, state) {
        Ok(fd) => Parent::Exists(fd),
        // Only ~/Vaults itself may be missing, and only by default: it is
        // created in the resolved HOME (never found by trimming the path,
        // which would go wrong for a HOME that itself ends in "Vaults").
        Err(OpenVaultError::Missing { component })
            if resolved_home.is_some()
                && component == components(parent_path.as_bytes()).map_or(0, |c| c.len()) =>
        {
            let home = resolved_home.as_deref().unwrap_or_default();
            Parent::CreateIn {
                home: open_parent_checked(home.as_ref(), config_dir, state)
                    .map_err(InitError::Path)?,
            }
        }
        Err(e) => return Err(InitError::Path(e)),
    };
    if let Parent::Exists(fd) = &parent {
        match statat(fd, dir_name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
            Err(Errno::NOENT) => {}
            Ok(_) => return Err(InitError::Exists),
            Err(errno) => return Err(InitError::Stat(errno)),
        }
    }
    Ok(Plan {
        parent,
        dir_name,
        path,
    })
}

/// Asks for one or more backup recipients (`age1…` public keys), echoed
/// because they are public. An invalid key is asked for again; an empty
/// line ends the list once there is at least one.
pub fn read_backup_recipients(
    terminal: &mut TerminalGuard,
    signals: &Signals,
) -> Result<Vec<x25519::Recipient>, InitError> {
    let mut recipients: Vec<x25519::Recipient> = Vec::new();
    let mut note = "identity.backup.age lets you recover this vault without the passphrase.\r\n\
                    Enter the age public key of a key you keep offline (paper key or second \
                    hardware key).\r\n\r\n";
    while recipients.len() < MAX_BACKUP_RECIPIENTS {
        let n = recipients.len() + 1;
        let prompt = format!("{note}Backup recipient {n} (age1..., empty line when done): ");
        let line = terminal
            .read_public_line(signals, &prompt)
            .map_err(InitError::Prompt)?;
        if line.is_empty() {
            if recipients.is_empty() {
                note = "At least one backup recipient is needed.\r\n";
                continue;
            }
            break;
        }
        match x25519::Recipient::from_str(&line) {
            Ok(recipient) => {
                recipients.push(recipient);
                note = "";
            }
            Err(_) => note = "That is not an age X25519 public key (age1...); try again.\r\n",
        }
    }
    Ok(recipients)
}

/// What this run created, to remove again if a later step fails.
#[derive(Default)]
struct Created {
    vaults_dir: Option<OwnedFd>,
    vault_dir: Option<(OwnedFd, OwnedFd)>,
}

/// Step 2: creates the vault as planned, registers it in `cfg` and stores
/// the config, then starts the vault's rollback counter at generation 1.
#[expect(
    clippy::too_many_arguments,
    reason = "one call site, each input distinct"
)]
pub fn create(
    plan: Plan,
    name: &VaultName,
    pass: &Passphrase,
    backup: &[x25519::Recipient],
    mut cfg: Config,
    home: &OsStr,
    config_dir: &ConfigDir,
    state: &StateDir,
) -> Result<(), InitError> {
    let mut created = Created::default();
    match build(
        &plan,
        name,
        pass,
        backup,
        &mut cfg,
        home,
        config_dir,
        state,
        &mut created,
    ) {
        Ok(vault_id) => generation::record_save(state.fd(), Counter::Vault(vault_id), 1)
            .map_err(InitError::Generation),
        // config.age was written; only config.gen failed. The vault is
        // complete and registered, so it stays.
        Err(e @ InitError::Config(ConfigError::Generation(_))) => Err(e),
        Err(e) => match remove_created(&plan, created) {
            Ok(()) => Err(InitError::RolledBack(Box::new(e))),
            Err(()) => Err(InitError::CleanupFailed(Box::new(e))),
        },
    }
}

#[expect(clippy::too_many_arguments, reason = "the steps of create, split out")]
fn build(
    plan: &Plan,
    name: &VaultName,
    pass: &Passphrase,
    backup: &[x25519::Recipient],
    cfg: &mut Config,
    home: &OsStr,
    config_dir: &ConfigDir,
    state: &StateDir,
    created: &mut Created,
) -> Result<VaultId, InitError> {
    let parent = match &plan.parent {
        Parent::Exists(fd) => fd,
        Parent::CreateIn { home } => {
            mkdirat(home, VAULTS_DIR, Mode::RWXU).map_err(InitError::Create)?;
            let fd = openat(home, VAULTS_DIR, OPEN_DIR_FLAGS, Mode::empty())
                .map_err(InitError::Create)?;
            created.vaults_dir.insert(fd)
        }
    };
    mkdirat(parent, plan.dir_name.as_str(), Mode::RWXU).map_err(|errno| match errno {
        Errno::EXIST => InitError::Exists,
        other => InitError::Create(other),
    })?;
    let dir = openat(
        parent,
        plan.dir_name.as_str(),
        OPEN_DIR_FLAGS,
        Mode::empty(),
    )
    .map_err(InitError::Create)?;
    let parent_copy = parent
        .try_clone()
        .map_err(|e| InitError::Create(Errno::from_io_error(&e).unwrap_or(Errno::IO)))?;
    let (_, dir) = created.vault_dir.insert((parent_copy, dir));

    format::write_version(dir).map_err(InitError::Write)?;
    let identity = VaultIdentity::generate();
    let vault_id = VaultId::random().map_err(InitError::Random)?;
    let write = |file: &str, sealed: &crate::crypto::Ciphertext| {
        let name = FileName::new(file).ok_or(InitError::BadTarget)?;
        atomic_write(dir, &name, Payload::Ciphertext(sealed)).map_err(InitError::Write)
    };
    write(
        VAULT_FILES[1],
        &identity
            .seal_with_passphrase(pass)
            .map_err(InitError::Crypto)?,
    )?;
    write(
        VAULT_FILES[2],
        &identity
            .seal_to_recipients(backup)
            .map_err(InitError::Crypto)?,
    )?;
    let empty = VaultContents {
        vault_id,
        generation: 1,
        notes: Vec::new(),
    };
    write(
        VAULT_FILES[3],
        &format::seal(&empty, &identity.recipient()).map_err(InitError::Vault)?,
    )?;

    // The finished directory must pass the same check as any vault.
    VaultDir::open_checked(plan.path.as_ref(), home, config_dir, state).map_err(InitError::Path)?;
    cfg.add(VaultEntry {
        name: VaultName::new(name.as_str().into()).ok_or(InitError::BadTarget)?,
        path: plan.path.clone(),
        vault_id,
        lock_after: None,
    })
    .map_err(InitError::Config)?;
    config::store(cfg, config_dir, state, pass).map_err(InitError::Config)?;
    Ok(vault_id)
}

/// Removes what `build` created: the files in the new vault directory,
/// the directory, and `~/Vaults` if this run made it. Each directory was
/// created by this run, so removing it never touches anything older.
fn remove_created(plan: &Plan, created: Created) -> Result<(), ()> {
    let mut ok = true;
    if let Some((parent, dir)) = created.vault_dir {
        for file in VAULT_FILES {
            match unlinkat(&dir, file, AtFlags::empty()) {
                Ok(()) | Err(Errno::NOENT) => {}
                Err(_) => ok = false,
            }
        }
        drop(dir);
        ok &= unlinkat(&parent, plan.dir_name.as_str(), AtFlags::REMOVEDIR).is_ok();
    }
    if created.vaults_dir.is_some()
        && let Parent::CreateIn { home } = &plan.parent
    {
        ok &= unlinkat(home, VAULTS_DIR, AtFlags::REMOVEDIR).is_ok();
    }
    if ok { Ok(()) } else { Err(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::decrypt_with_identity;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;
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

    struct World {
        base: PathBuf,
        home: PathBuf,
        config: ConfigDir,
        state: StateDir,
    }

    impl World {
        fn new(test: &str) -> Self {
            Self::with_home(test, "home")
        }

        /// As `new`, with HOME's last component named `home_name`.
        fn with_home(test: &str, home_name: &str) -> Self {
            let base = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/unit-init")
                .join(test);
            let _ = fs::remove_dir_all(&base);
            let home = base.join(home_name);
            fs::create_dir_all(&home).unwrap();
            fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
            let config = ConfigDir::create(Some(home.as_os_str())).unwrap();
            let state = StateDir::open_or_create(Some(home.as_os_str())).unwrap();
            Self {
                base,
                home,
                config,
                state,
            }
        }

        fn plan(&self, at: Option<&Path>, n: &str) -> Result<Plan, InitError> {
            plan(
                self.home.as_os_str(),
                at,
                &name(n),
                &self.config,
                &self.state,
            )
        }

        fn create(&self, plan: Plan, n: &str, backup: &VaultIdentity) -> Result<(), InitError> {
            create(
                plan,
                &name(n),
                &pass("pw"),
                &[backup.recipient()],
                Config::new(),
                self.home.as_os_str(),
                &self.config,
                &self.state,
            )
        }
    }

    // Acceptance: the layout matches docs/design/storage.md exactly, and
    // identity.backup.age recovers the vault without the passphrase.
    #[test]
    fn creates_the_documented_layout() {
        let world = World::new("layout");
        let backup = VaultIdentity::generate();
        let plan = world.plan(None, "diary").unwrap();
        assert!(matches!(plan.parent, Parent::CreateIn { .. }));
        world.create(plan, "diary", &backup).unwrap();

        let vaults = world.home.join("Vaults");
        let vault = vaults.join("diary");
        assert_eq!((mode(&vaults), mode(&vault)), (0o700, 0o700));
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
        assert_eq!(fs::read(vault.join("VERSION")).unwrap(), b"1\n");

        // The config holds the entry with the vault's real id and resolved
        // path; both rollback counters exist.
        let (cfg, _) = config::load(&world.config, &world.state, &pass("pw"))
            .unwrap()
            .unwrap();
        let entry = cfg.find(&name("diary")).unwrap();
        let resolved = fs::canonicalize(&vault).unwrap();
        assert_eq!(Path::new(&entry.path), resolved);
        let vault_dir = VaultDir::open_checked(
            entry.path.as_ref(),
            world.home.as_os_str(),
            &world.config,
            &world.state,
        )
        .unwrap();
        let (id, generation) = config::identify_vault(&vault_dir, &pass("pw")).unwrap();
        assert_eq!((id, generation), (entry.vault_id, 1));
        assert_eq!(
            generation::load(world.state.fd(), Counter::Vault(id)),
            Ok(Some(1))
        );
        assert_eq!(
            generation::load(world.state.fd(), Counter::Config),
            Ok(Some(1))
        );

        // Recovery without the passphrase: the backup key opens
        // identity.backup.age, and the identity inside opens vault.age.
        let secret = decrypt_with_identity(
            &fs::read(vault.join("identity.backup.age")).unwrap(),
            &backup,
        )
        .unwrap();
        let recovered = x25519::Identity::from_str(std::str::from_utf8(&secret).unwrap()).unwrap();
        let vault_age = fs::read(vault.join("vault.age")).unwrap();
        assert!(age::decrypt(&recovered, &vault_age).is_ok());
    }

    // Regression: ~/Vaults was once found by trimming "Vaults" off the
    // parent path, which also stripped a HOME that itself ends in "Vaults".
    #[test]
    fn home_ending_in_vaults() {
        let world = World::with_home("home_vaults", "Vaults");
        let backup = VaultIdentity::generate();
        let plan = world.plan(None, "diary").unwrap();
        assert!(matches!(plan.parent, Parent::CreateIn { .. }));
        world.create(plan, "diary", &backup).unwrap();
        assert!(world.home.join("Vaults/diary/vault.age").exists());
        assert_eq!(mode(&world.home.join("Vaults")), 0o700);
    }

    #[test]
    fn at_names_the_vault_directory_itself() {
        let world = World::new("at");
        let backup = VaultIdentity::generate();
        let synced = world.base.join("synced");
        fs::create_dir(&synced).unwrap();
        fs::set_permissions(&synced, fs::Permissions::from_mode(0o755)).unwrap();
        let plan = world.plan(Some(&synced.join("notes")), "work").unwrap();
        world.create(plan, "work", &backup).unwrap();
        assert_eq!(mode(&synced.join("notes")), 0o700);
        assert!(synced.join("notes/vault.age").exists());
        assert!(!world.home.join("Vaults").exists());
    }

    #[test]
    fn plan_refuses_without_creating_anything() {
        let world = World::new("plan");
        let existing = world.base.join("existing");
        fs::create_dir(&existing).unwrap();
        assert_eq!(
            world.plan(Some(&existing), "x").err(),
            Some(InitError::Exists)
        );
        symlink(&existing, world.base.join("link")).unwrap();
        assert_eq!(
            world.plan(Some(&world.base.join("link")), "x").err(),
            Some(InitError::Exists),
            "a symlink at the target name exists too"
        );
        for bad in ["/", "..", "x/"] {
            let err = world.plan(Some(Path::new(bad)), "x").err();
            assert!(
                matches!(err, Some(InitError::BadTarget | InitError::Path(_))),
                "{bad}"
            );
        }
        let open = world.base.join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
        let err = world.plan(Some(&open.join("v")), "x").err();
        assert!(matches!(
            err,
            Some(InitError::Path(OpenVaultError::UnsafeParent { .. }))
        ));
        let inside = world.home.join(".config/crystal/v");
        assert_eq!(
            world.plan(Some(&inside), "x").err(),
            Some(InitError::Path(OpenVaultError::InsideCrystalDir))
        );
        let missing_parent = world.base.join("no/such/v");
        assert!(matches!(
            world.plan(Some(&missing_parent), "x").err(),
            Some(InitError::Resolve(_))
        ));
        // ~/Vaults/x already there.
        fs::create_dir_all(world.home.join("Vaults/x")).unwrap();
        fs::set_permissions(world.home.join("Vaults"), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(world.plan(None, "x").err(), Some(InitError::Exists));
        assert!(world.plan(None, "y").is_ok());
    }

    // If undoing fails (here: a file the run did not make is in the new
    // vault directory, so it cannot be removed), the error says so instead
    // of claiming the vault was removed.
    #[test]
    fn failed_cleanup_is_reported() {
        let world = World::new("cleanup_fails");
        let plan = world.plan(None, "diary").unwrap();
        let mut created = Created::default();
        let backup = VaultIdentity::generate();
        let config_dir = world.home.join(".config/crystal");
        fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o500)).unwrap();
        let err = build(
            &plan,
            &name("diary"),
            &pass("pw"),
            &[backup.recipient()],
            &mut Config::new(),
            world.home.as_os_str(),
            &world.config,
            &world.state,
            &mut created,
        )
        .unwrap_err();
        fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            matches!(err, InitError::Config(ConfigError::Write(_))),
            "{err:?}"
        );
        let vault = world.home.join("Vaults/diary");
        fs::write(vault.join("stray"), b"x").unwrap();
        assert_eq!(remove_created(&plan, created), Err(()));
        assert!(vault.join("stray").exists());
        assert!(
            !vault.join("vault.age").exists(),
            "its own files are still removed"
        );

        let shown = InitError::CleanupFailed(Box::new(err)).to_string();
        assert!(shown.ends_with("remove it by hand"), "{shown}");
    }

    // config.age was written but config.gen could not be: the vault is
    // complete and registered, so it is kept, not rolled back.
    #[test]
    fn config_gen_failure_keeps_the_registered_vault() {
        let world = World::new("gen_fails");
        let backup = VaultIdentity::generate();
        let plan = world.plan(None, "diary").unwrap();
        let state_dir = world.home.join(".local/state/crystal");
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o500)).unwrap();
        let err = world.create(plan, "diary", &backup).unwrap_err();
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            matches!(err, InitError::Config(ConfigError::Generation(_))),
            "{err:?}"
        );
        assert!(world.home.join("Vaults/diary/vault.age").exists());
        let (cfg, _) = config::load(&world.config, &world.state, &pass("pw"))
            .unwrap()
            .unwrap();
        assert!(cfg.find(&name("diary")).is_some());
    }

    // A failure after the directory exists removes everything this run
    // created, and nothing that existed before.
    #[test]
    fn late_failure_removes_what_was_created() {
        let world = World::new("rollback");
        let backup = VaultIdentity::generate();
        let config = world.home.join(".config/crystal");
        let plan = world.plan(None, "diary").unwrap();
        // config.age cannot be written: the last step fails.
        fs::set_permissions(&config, fs::Permissions::from_mode(0o500)).unwrap();
        let err = world.create(plan, "diary", &backup).unwrap_err();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(err, InitError::RolledBack(_)), "{err:?}");
        assert!(
            err.to_string()
                .ends_with("the partly created vault was removed")
        );
        assert!(
            !world.home.join("Vaults").exists(),
            "~/Vaults was created by this run"
        );

        // With ~/Vaults already there, only the vault directory goes.
        fs::create_dir(world.home.join("Vaults")).unwrap();
        fs::set_permissions(world.home.join("Vaults"), fs::Permissions::from_mode(0o700)).unwrap();
        let plan = world.plan(None, "diary").unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o500)).unwrap();
        let err = world.create(plan, "diary", &backup).unwrap_err();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(err, InitError::RolledBack(_)), "{err:?}");
        assert!(world.home.join("Vaults").exists());
        assert_eq!(fs::read_dir(world.home.join("Vaults")).unwrap().count(), 0);
        assert!(!config.join("config.age").exists());
    }
}
