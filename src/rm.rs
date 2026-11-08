//! `crystal rm NAME` (docs/decisions/0032-fixed-locations.md): delete a
//! vault for good, after its name is typed again. There is no trash; only
//! a backup brings it back (docs/decisions/0036-backups-external.md).
//!
//! Only Crystal's own files are removed: `VERSION`, `identity.age`,
//! `identity.backup.age`, `vault.age`, Syncthing conflict copies of
//! `vault.age`, and atomic-writer temp files of those four. Any other file
//! stays, and then so does the directory. The vault's pin and rollback
//! counter go too. Runs before the sandbox and then exits.

use std::fmt;

use rustix::fs::{AtFlags, Dir, Mode, OFlags, openat, unlinkat};
use rustix::io::Errno;

use crate::init::VAULT_FILES;
use crate::unlock::is_sync_conflict;
use crate::vault::atomic::{FileName, is_temp_of};
use crate::vault::generation::{self, Counter};
use crate::vault::pin::{self, PinError};
use crate::vault::vault_dir::{OpenVaultError, VaultDir};
use crate::vault::{DataDir, StateDir, VaultName};

/// Shown before the name is typed again.
pub const CONFIRM_PROMPT: &str = "This deletes the vault for good: there is no trash, and only a \
                                  backup can bring it back.\r\n\r\nType the vault's name to \
                                  confirm: ";

/// Why `crystal rm` stopped. Fixed text; never the name.
#[derive(Debug, PartialEq, Eq)]
pub enum RmError {
    Path(OpenVaultError),
    /// The name typed to confirm was not the vault's.
    NotConfirmed,
    ListDir(Errno),
    Remove(Errno),
    Pin(PinError),
    Counter(Errno),
}

impl fmt::Display for RmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(e) => e.fmt(f),
            Self::NotConfirmed => write!(f, "that is not the vault's name; nothing was deleted"),
            Self::ListDir(errno) => write!(f, "cannot list the vault directory ({errno})"),
            Self::Remove(errno) => write!(f, "cannot remove a vault file ({errno})"),
            Self::Pin(e) => e.fmt(f),
            Self::Counter(errno) => {
                write!(f, "cannot remove the vault's rollback counter ({errno})")
            }
        }
    }
}

impl std::error::Error for RmError {}

/// What was left behind.
#[derive(Debug, PartialEq, Eq)]
pub enum Removed {
    /// The directory and everything Crystal knows about the vault.
    All,
    /// Crystal's files are gone; this many other files stay, and so does
    /// the directory.
    KeptOthers(usize),
}

impl fmt::Display for Removed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::All => write!(f, "The vault was deleted."),
            Self::KeptOthers(n) => write!(
                f,
                "The vault's files were deleted. {n} other file(s) that Crystal did not make are \
                 still in its directory in ~/.local/share/crystal, so the directory stays."
            ),
        }
    }
}

/// Whether `entry` in a vault directory is Crystal's own.
fn is_crystal_file(entry: &[u8]) -> bool {
    VAULT_FILES.iter().any(|f| entry == f.as_bytes())
        || is_sync_conflict(entry)
        || VAULT_FILES
            .iter()
            .filter_map(|f| FileName::new(f))
            .any(|f| is_temp_of(entry, &f))
}

/// Deletes the vault `name`, which the user has confirmed.
pub fn remove(data: &DataDir, state: &StateDir, name: &VaultName) -> Result<Removed, RmError> {
    let vault_dir = VaultDir::open_for_removal(data, name).map_err(RmError::Path)?;
    // The pin names the rollback counter; read it before it goes. A
    // malformed pin is still removed, only the counter is then unknown.
    let pinned = pin::load(state.fd(), name).ok().flatten();

    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let listing = openat(vault_dir.fd(), c".", flags, Mode::empty()).map_err(RmError::ListDir)?;
    let mut ours = Vec::new();
    let mut kept = 0;
    for entry in Dir::new(listing).map_err(RmError::ListDir)? {
        let entry = entry.map_err(RmError::ListDir)?;
        let file = entry.file_name();
        match file.to_bytes() {
            b"." | b".." => {}
            bytes if is_crystal_file(bytes) => ours.push(file.to_owned()),
            _ => kept += 1,
        }
    }
    for file in ours {
        match unlinkat(vault_dir.fd(), file.as_c_str(), AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(errno) => return Err(RmError::Remove(errno)),
        }
    }
    drop(vault_dir);
    if kept == 0 {
        unlinkat(data.fd(), name.as_str(), AtFlags::REMOVEDIR).map_err(RmError::Remove)?;
    }
    if let Some(id) = pinned {
        generation::remove(state.fd(), Counter::Vault(id)).map_err(RmError::Counter)?;
    }
    pin::remove(state.fd(), name).map_err(RmError::Pin)?;
    Ok(if kept == 0 {
        Removed::All
    } else {
        Removed::KeptOthers(kept)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Passphrase;
    use crate::init;
    use crate::recovery::RecoveryCode;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use zeroize::Zeroizing;

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
                .join("target/unit-rm")
                .join(test);
            let _ = fs::remove_dir_all(&home);
            fs::create_dir_all(&home).unwrap();
            fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
            let data = DataDir::create(Some(home.as_os_str())).unwrap();
            let state = StateDir::open_or_create(Some(home.as_os_str())).unwrap();
            Self { home, data, state }
        }

        fn init(&self, n: &str) {
            let pass = Passphrase::new(Zeroizing::new(b"pw".to_vec())).unwrap();
            let code = RecoveryCode::generate().unwrap();
            let plan = init::plan(&self.data, &name(n)).unwrap();
            init::create(plan, &name(n), &pass, &code, &self.data, &self.state).unwrap();
        }

        fn vault(&self, n: &str) -> PathBuf {
            self.home.join(".local/share/crystal").join(n)
        }

        fn state_files(&self) -> Vec<String> {
            let mut files: Vec<String> = fs::read_dir(self.home.join(".local/state/crystal"))
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            files.sort();
            files
        }
    }

    #[test]
    fn removes_the_vault_its_pin_and_counter_only() {
        let world = World::new("all");
        world.init("diary");
        world.init("other");
        let others_state: Vec<String> = world
            .state_files()
            .into_iter()
            .filter(|f| f == "other.pin" || f.ends_with(".gen"))
            .collect();
        let vault = world.vault("diary");
        fs::write(
            vault.join("vault.sync-conflict-20261108-120000-ABC.age"),
            b"",
        )
        .unwrap();
        fs::write(vault.join(".vault.age.tmp-0123456789abcdef"), b"").unwrap();

        assert_eq!(
            remove(&world.data, &world.state, &name("diary")),
            Ok(Removed::All)
        );
        assert!(!vault.exists());
        assert!(world.vault("other").join("vault.age").exists());
        let left = world.state_files();
        assert!(left.contains(&"other.pin".to_owned()) && !left.contains(&"diary.pin".to_owned()));
        assert_eq!(
            left.len(),
            others_state.len() - 1,
            "diary's counter is gone: {left:?}"
        );
    }

    #[test]
    fn keeps_files_crystal_did_not_make() {
        let world = World::new("kept");
        world.init("diary");
        let vault = world.vault("diary");
        fs::write(vault.join(".stfolder"), b"").unwrap();
        fs::write(vault.join("notes.txt"), b"mine").unwrap();
        // Near misses are not Crystal's.
        fs::write(vault.join(".vault.age.tmp-XYZ"), b"").unwrap();
        assert_eq!(
            remove(&world.data, &world.state, &name("diary")),
            Ok(Removed::KeptOthers(3))
        );
        let mut left: Vec<String> = fs::read_dir(&vault)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, [".stfolder", ".vault.age.tmp-XYZ", "notes.txt"]);
        assert!(!world.state_files().contains(&"diary.pin".to_owned()));
        let shown = Removed::KeptOthers(3).to_string();
        assert!(shown.contains("3 other file(s)") && !shown.contains("diary"));
    }

    #[test]
    fn removes_a_vault_with_a_bad_version_or_no_pin() {
        let world = World::new("broken");
        world.init("diary");
        fs::write(world.vault("diary").join("VERSION"), b"99\n").unwrap();
        fs::remove_file(world.home.join(".local/state/crystal/diary.pin")).unwrap();
        assert_eq!(
            remove(&world.data, &world.state, &name("diary")),
            Ok(Removed::All)
        );
        assert!(!world.vault("diary").exists());
    }

    #[test]
    fn refuses_a_missing_vault() {
        let world = World::new("missing");
        assert_eq!(
            remove(&world.data, &world.state, &name("diary")),
            Err(RmError::Path(OpenVaultError::NoSuchVault))
        );
    }
}
