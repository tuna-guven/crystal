//! Opening a vault directory, `~/.local/share/crystal/NAME`
//! (docs/decisions/0032-fixed-locations.md, docs/design/storage.md, "Vault
//! directory"). The data directory was reached from HOME one component at
//! a time without following symlinks (`DataDir`); the vault is one more
//! such step. NAME is a `VaultName`, so it can never be a path.

use std::fmt;
use std::os::fd::OwnedFd;

use rustix::fs::{Mode, fstat, openat};
use rustix::io::Errno;
use rustix::process::geteuid;

use super::format::{VersionError, read_version};
use super::{DataDir, OPEN_DIR_FLAGS, VaultName};

/// Why a vault directory was refused. Fixed text; never the name.
#[derive(Debug, PartialEq, Eq)]
pub enum OpenVaultError {
    /// No vault directory with that name.
    NoSuchVault,
    /// The name is a symlink or not a directory.
    NotARealDirectory,
    /// The vault directory is not ours.
    NotOwned,
    /// The vault directory's mode is not 0700.
    BadMode(u32),
    /// `VERSION` is missing, not a regular file, malformed or unknown.
    Version(VersionError),
    Open(Errno),
    Stat(Errno),
}

impl fmt::Display for OpenVaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchVault => write!(
                f,
                "there is no vault with that name in ~/.local/share/crystal"
            ),
            Self::NotARealDirectory => {
                write!(f, "the vault is a symlink or not a directory")
            }
            Self::NotOwned => write!(f, "the vault directory is owned by another user"),
            Self::BadMode(mode) => {
                write!(f, "the vault directory has mode {mode:o}, need 700")
            }
            Self::Version(e) => e.fmt(f),
            Self::Open(errno) => write!(f, "cannot open the vault directory ({errno})"),
            Self::Stat(errno) => write!(f, "cannot check the vault directory ({errno})"),
        }
    }
}

impl std::error::Error for OpenVaultError {}

/// A vault directory that passed every check. Its fd is the only handle
/// Crystal uses afterwards (and, from Phase 6, a Landlock rule fd).
pub struct VaultDir {
    fd: OwnedFd,
}

impl VaultDir {
    /// Opens `NAME` in the data directory with `O_NOFOLLOW`; it must be
    /// ours, mode 0700, and hold a valid `VERSION`.
    pub fn open(data: &DataDir, name: &VaultName) -> Result<Self, OpenVaultError> {
        let dir = Self::open_private(data, name)?;
        read_version(&dir.fd).map_err(OpenVaultError::Version)?;
        Ok(dir)
    }

    /// As `open`, without reading `VERSION`: for `crystal rm`, which must
    /// also remove a vault whose `VERSION` is damaged or from a newer
    /// Crystal.
    pub fn open_for_removal(data: &DataDir, name: &VaultName) -> Result<Self, OpenVaultError> {
        Self::open_private(data, name)
    }

    fn open_private(data: &DataDir, name: &VaultName) -> Result<Self, OpenVaultError> {
        let fd =
            openat(data.fd(), name.as_str(), OPEN_DIR_FLAGS, Mode::empty()).map_err(|errno| {
                match errno {
                    Errno::NOENT => OpenVaultError::NoSuchVault,
                    Errno::NOTDIR | Errno::LOOP => OpenVaultError::NotARealDirectory,
                    other => OpenVaultError::Open(other),
                }
            })?;
        let stat = fstat(&fd).map_err(OpenVaultError::Stat)?;
        if stat.st_uid != geteuid().as_raw() {
            return Err(OpenVaultError::NotOwned);
        }
        let mode = stat.st_mode & 0o7777;
        if mode != 0o700 {
            return Err(OpenVaultError::BadMode(mode));
        }
        Ok(Self { fd })
    }

    pub fn fd(&self) -> &OwnedFd {
        &self.fd
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    use super::*;

    fn fresh_home(test: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/unit-homes")
            .join(format!("vault_dir_{test}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn name(s: &str) -> VaultName {
        VaultName::new(OsString::from(s)).unwrap()
    }

    /// HOME with ~/.local/share/crystal and an empty vault directory
    /// `diary` (mode 0700) holding a valid VERSION.
    fn setup(test: &str) -> (PathBuf, DataDir, PathBuf) {
        let home = fresh_home(test);
        let data = DataDir::create(Some(home.as_os_str())).unwrap();
        let vault = home.join(".local/share/crystal/diary");
        fs::create_dir(&vault).unwrap();
        fs::set_permissions(&vault, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(vault.join("VERSION"), b"1\n").unwrap();
        (home, data, vault)
    }

    #[test]
    fn opens_a_private_vault() {
        let (_home, data, _vault) = setup("ok");
        VaultDir::open(&data, &name("diary")).unwrap();
    }

    #[test]
    fn refuses_a_missing_vault() {
        let (_home, data, _vault) = setup("missing");
        assert_eq!(
            VaultDir::open(&data, &name("other")).err(),
            Some(OpenVaultError::NoSuchVault)
        );
    }

    #[test]
    fn refuses_a_symlink_or_a_file() {
        let (home, data, vault) = setup("symlink");
        let data_path = home.join(".local/share/crystal");
        symlink(&vault, data_path.join("link")).unwrap();
        fs::write(data_path.join("file"), b"").unwrap();
        for n in ["link", "file"] {
            assert_eq!(
                VaultDir::open(&data, &name(n)).err(),
                Some(OpenVaultError::NotARealDirectory),
                "{n}"
            );
        }
    }

    #[test]
    fn refuses_a_loose_mode() {
        let (_home, data, vault) = setup("mode");
        fs::set_permissions(&vault, fs::Permissions::from_mode(0o750)).unwrap();
        assert_eq!(
            VaultDir::open(&data, &name("diary")).err(),
            Some(OpenVaultError::BadMode(0o750))
        );
    }

    #[test]
    fn refuses_a_bad_version_but_rm_may_open_it() {
        let (_home, data, vault) = setup("version");
        fs::write(vault.join("VERSION"), b"99\n").unwrap();
        assert!(matches!(
            VaultDir::open(&data, &name("diary")).err(),
            Some(OpenVaultError::Version(_))
        ));
        VaultDir::open_for_removal(&data, &name("diary")).unwrap();
        fs::remove_file(vault.join("VERSION")).unwrap();
        assert!(matches!(
            VaultDir::open(&data, &name("diary")).err(),
            Some(OpenVaultError::Version(_))
        ));
    }

    #[test]
    fn errors_never_show_the_name() {
        let errors = [
            OpenVaultError::NoSuchVault,
            OpenVaultError::NotARealDirectory,
            OpenVaultError::NotOwned,
            OpenVaultError::BadMode(0o755),
            OpenVaultError::Open(Errno::ACCESS),
            OpenVaultError::Stat(Errno::ACCESS),
        ];
        for e in errors {
            assert!(!e.to_string().contains("diary"), "{e}");
        }
    }
}
